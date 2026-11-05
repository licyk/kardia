use std::any::TypeId;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{ErasedConfig, ValidationError, decode};
use crate::context::Context;
use crate::error::Result;
use crate::key::Key;

/// A statically typed plugin (the paper's component (d, p, e)).
pub trait Plugin: Send + Sync + 'static {
    const NAME: &'static str;
    type Config: DeserializeOwned + Send + Sync + 'static;

    /// Declared dependencies (d), with optional interception metadata.
    fn inject() -> Inject {
        Inject::new()
    }

    /// Declared provisions (p). Metadata only: used for tooling and the
    /// dependency-cycle check.
    fn provide() -> Vec<Key> {
        Vec::new()
    }

    fn apply(ctx: Context, config: Arc<Self::Config>) -> impl Future<Output = Result<()>> + Send;
}

/// Declared dependencies: ordered keys, each with optional interception metadata.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inject {
    entries: IndexMap<Key, Option<Value>>,
}

impl Inject {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: impl Into<Key>) -> Self {
        self.entries.insert(key.into(), None);
        self
    }

    pub fn with_meta(mut self, key: impl Into<Key>, meta: Value) -> Self {
        self.entries.insert(key.into(), Some(meta));
        self
    }

    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.entries.keys()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn meta(&self, key: &str) -> Option<&Value> {
        self.entries.get(key).and_then(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K: Into<Key>, const N: usize> From<[K; N]> for Inject {
    fn from(keys: [K; N]) -> Self {
        keys.into_iter().fold(Inject::new(), Inject::with)
    }
}

impl<K: Into<Key>> From<Vec<K>> for Inject {
    fn from(keys: Vec<K>) -> Self {
        keys.into_iter().fold(Inject::new(), Inject::with)
    }
}

/// Identity of a plugin in the registry: the plugin type, or the identity
/// of a closure-based definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKey {
    Type(TypeId),
    Def(u64),
}

type ApplyFn = Arc<dyn Fn(Context, ErasedConfig) -> BoxFuture<'static, Result<()>> + Send + Sync>;
type DecodeFn = Arc<dyn Fn(&Value) -> Result<ErasedConfig, ValidationError> + Send + Sync>;

/// A type-erased plugin definition, as stored in the registry.
#[derive(Clone)]
pub struct PluginDef(Arc<PluginDefInner>);

#[derive(Clone)]
struct PluginDefInner {
    key: RuntimeKey,
    name: Option<String>,
    inject: Inject,
    provide: Vec<Key>,
    tags: Vec<&'static str>,
    decode: DecodeFn,
    apply: ApplyFn,
}

static DEF_COUNTER: AtomicU64 = AtomicU64::new(0);

impl PluginDef {
    /// The definition of a typed plugin. Every call yields the same registry identity.
    pub fn of<P: Plugin>() -> Self {
        Self(Arc::new(PluginDefInner {
            key: RuntimeKey::Type(TypeId::of::<P>()),
            name: Some(P::NAME.into()),
            inject: P::inject(),
            provide: P::provide(),
            tags: Vec::new(),
            decode: typed_decoder::<P::Config>(),
            apply: Arc::new(|ctx, config| {
                let config = config.downcast::<P::Config>().expect("config decoded by this plugin");
                P::apply(ctx, config).boxed()
            }),
        }))
    }

    /// A closure plugin receiving its configuration as raw JSON. Every call
    /// creates a new registry identity.
    pub fn new<F, Fut>(apply: F) -> Self
    where
        F: Fn(Context, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self::erased(
            Arc::new(|value: &Value| Ok(Arc::new(value.clone()) as ErasedConfig)),
            Arc::new(move |ctx, config| {
                let config = config.downcast_ref::<Value>().cloned().unwrap_or_default();
                apply(ctx, config).boxed()
            }),
        )
    }

    /// A closure plugin with a typed, deserialized configuration.
    pub fn typed<C, F, Fut>(apply: F) -> Self
    where
        C: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Context, Arc<C>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self::erased(
            typed_decoder::<C>(),
            Arc::new(move |ctx, config| {
                let config = config.downcast::<C>().expect("config decoded by this plugin");
                apply(ctx, config).boxed()
            }),
        )
    }

    fn erased(decode: DecodeFn, apply: ApplyFn) -> Self {
        let key = RuntimeKey::Def(DEF_COUNTER.fetch_add(1, Ordering::Relaxed) + 1);
        Self(Arc::new(PluginDefInner {
            key,
            name: None,
            inject: Inject::new(),
            provide: Vec::new(),
            tags: Vec::new(),
            decode,
            apply,
        }))
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        Arc::make_mut(&mut self.0).name = Some(name.into());
        self
    }

    pub fn inject(mut self, inject: impl Into<Inject>) -> Self {
        Arc::make_mut(&mut self.0).inject = inject.into();
        self
    }

    pub fn provides(mut self, keys: impl IntoIterator<Item = impl Into<Key>>) -> Self {
        Arc::make_mut(&mut self.0).provide = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Marks the definition, so that hosts (e.g. the loader) can recognise it.
    pub fn tag(mut self, tag: &'static str) -> Self {
        Arc::make_mut(&mut self.0).tags.push(tag);
        self
    }

    /// Validates or transforms the raw configuration before it is decoded.
    pub fn validate_with(
        mut self,
        validate: impl Fn(&Value) -> Result<Value, ValidationError> + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::make_mut(&mut self.0);
        let decode = inner.decode.clone();
        inner.decode = Arc::new(move |value| decode(&validate(value)?));
        self
    }

    pub fn key(&self) -> RuntimeKey {
        self.0.key
    }

    pub fn plugin_name(&self) -> Option<&str> {
        self.0.name.as_deref()
    }

    pub fn inject_spec(&self) -> &Inject {
        &self.0.inject
    }

    pub fn provide_spec(&self) -> &[Key] {
        &self.0.provide
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.0.tags.contains(&tag)
    }

    pub fn decode(&self, value: &Value) -> Result<ErasedConfig, ValidationError> {
        (self.0.decode)(value)
    }

    pub(crate) fn apply(&self, ctx: Context, config: ErasedConfig) -> BoxFuture<'static, Result<()>> {
        (self.0.apply)(ctx, config)
    }
}

impl fmt::Debug for PluginDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginDef").field("key", &self.0.key).field("name", &self.0.name).finish()
    }
}

fn typed_decoder<C: DeserializeOwned + Send + Sync + 'static>() -> DecodeFn {
    Arc::new(|value| decode::<C>(value).map(|config| Arc::new(config) as ErasedConfig))
}
