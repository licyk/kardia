use std::fmt;

use crate::context::Context;
use crate::effect::EffectMeta;
use crate::key::FiberId;

/// The paper's θ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FiberState {
    /// Waiting for its dependencies.
    Pending,
    Loading,
    Active,
    Unloading,
    /// Its last load failed; only `update` / `restart` / `replace` recover it.
    Failed,
    Disposed,
}

/// A handle to a fiber (one instantiation of a plugin).
#[derive(Clone)]
pub struct Fiber {
    pub(crate) ctx: Context,
}

impl Fiber {
    pub fn id(&self) -> FiberId {
        self.ctx.fiber
    }

    /// The fiber's own context.
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    pub fn state(&self) -> FiberState {
        self.ctx.root.with(|st| {
            st.fiber(self.ctx.fiber)
                .map_or(FiberState::Disposed, |fiber| fiber.state)
        })
    }

    /// The effects currently registered on the fiber, in insertion order.
    pub fn effects(&self) -> Vec<EffectMeta> {
        let cells = self.ctx.root.with(|st| {
            st.fiber(self.ctx.fiber)
                .map(|fiber| fiber.disposables.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });
        cells.iter().map(|cell| cell.meta()).collect()
    }

    /// Number of effects in the fiber's accumulator.
    pub fn effect_count(&self) -> usize {
        self.ctx
            .root
            .with(|st| st.fiber(self.ctx.fiber).map_or(0, |fiber| fiber.disposables.len()))
    }
}

impl PartialEq for Fiber {
    fn eq(&self, other: &Self) -> bool {
        self.ctx.same_root(&other.ctx) && self.ctx.fiber == other.ctx.fiber
    }
}

impl Eq for Fiber {}

impl fmt::Debug for Fiber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fiber({:?}, {:?})", self.ctx.fiber, self.state())
    }
}

impl Context {
    /// The fiber this context belongs to.
    pub fn fiber(&self) -> Fiber {
        let scope = self
            .root
            .with(|st| st.fiber(self.fiber).map(|fiber| fiber.scope.clone()));
        let ctx = Context {
            root: self.root.clone(),
            fiber: self.fiber,
            scope: scope.unwrap_or_else(|| self.scope.clone()),
        };
        Fiber { ctx }
    }
}
