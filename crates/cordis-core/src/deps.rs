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
