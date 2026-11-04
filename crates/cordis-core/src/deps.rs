use crate::context::Context;
use crate::error::AccessError;
use crate::plugin::Inject;
use crate::service::{Service, Svc};

/// A tuple of service types declared and accessed together:
/// `type Inject = (Database, Cache)` → `ctx.deps::<(Database, Cache)>()?`.
pub trait Deps {
    type Output;

    fn keys() -> Vec<&'static str>;

    fn get(ctx: &Context) -> Result<Self::Output, AccessError>;
}

macro_rules! deps_tuple {
    ($($T:ident),*) => {
        impl<$($T: Service),*> Deps for ($($T,)*) {
            type Output = ($(Svc<$T>,)*);

            fn keys() -> Vec<&'static str> {
                vec![$($T::NAME),*]
            }

            #[allow(unused_variables)]
            fn get(ctx: &Context) -> Result<Self::Output, AccessError> {
                Ok(($(ctx.get::<$T>()?,)*))
            }
        }
    };
}

deps_tuple!();
deps_tuple!(A);
deps_tuple!(A, B);
deps_tuple!(A, B, C);
deps_tuple!(A, B, C, D);
deps_tuple!(A, B, C, D, E);
deps_tuple!(A, B, C, D, E, F);
deps_tuple!(A, B, C, D, E, F, G);
deps_tuple!(A, B, C, D, E, F, G, H);

impl Inject {
    /// The declaration of a tuple of service types.
    pub fn of<D: Deps>() -> Self {
        D::keys().into()
    }
}

impl Context {
    /// Accesses a tuple of declared services through the committed view.
    pub fn deps<D: Deps>(&self) -> Result<D::Output, AccessError> {
        D::get(self)
    }
}

/// Positions within a dependency tuple (inferred, never written by hand).
pub mod index {
    pub struct I0;
    pub struct I1;
    pub struct I2;
    pub struct I3;
    pub struct I4;
    pub struct I5;
    pub struct I6;
    pub struct I7;
}

/// `D` contains `S` (at position `I`).
pub trait Has<S, I> {}

macro_rules! has {
    ($params:tt $($S:ident => $I:ident),*) => {
        $(has!(@one $params $S => $I);)*
    };
    (@one [$($T:ident),*] $S:ident => $I:ident) => {
        impl<$($T),*> Has<$S, index::$I> for ($($T,)*) {}
    };
}

has!([A] A => I0);
has!([A, B] A => I0, B => I1);
has!([A, B, C] A => I0, B => I1, C => I2);
has!([A, B, C, D] A => I0, B => I1, C => I2, D => I3);
has!([A, B, C, D, E] A => I0, B => I1, C => I2, D => I3, E => I4);
has!([A, B, C, D, E, F] A => I0, B => I1, C => I2, D => I3, E => I4, F => I5);
has!([A, B, C, D, E, F, G] A => I0, B => I1, C => I2, D => I3, E => I4, F => I5, G => I6);
has!([A, B, C, D, E, F, G, H] A => I0, B => I1, C => I2, D => I3, E => I4, F => I5, G => I6, H => I7);

/// A plugin context whose typed accesses are checked against its declared
/// dependencies `D` at compile time: `get::<S>()` only compiles if `S` is in `D`.
pub struct Injected<D> {
    ctx: Context,
    _deps: std::marker::PhantomData<fn() -> D>,
}

impl<D> Clone for Injected<D> {
    fn clone(&self) -> Self {
        Self { ctx: self.ctx.clone(), _deps: std::marker::PhantomData }
    }
}

impl<D: Deps> Injected<D> {
    pub fn new(ctx: Context) -> Self {
        Self { ctx, _deps: std::marker::PhantomData }
    }

    /// A declared dependency, read through the committed view:
    /// `ctx.get::<Database, _>()` (the `_` is its inferred position in `D`).
    pub fn get<S: Service, I>(&self) -> Result<Svc<S>, AccessError>
    where
        D: Has<S, I>,
    {
        self.ctx.get::<S>()
    }

    /// All declared dependencies at once.
    pub fn deps(&self) -> Result<D::Output, AccessError> {
        D::get(&self.ctx)
    }

    /// The underlying (unchecked) context.
    pub fn context(&self) -> &Context {
        &self.ctx
    }
}

impl<D> std::ops::Deref for Injected<D> {
    type Target = Context;

    fn deref(&self) -> &Context {
        &self.ctx
    }
}
