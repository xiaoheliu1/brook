//! Typed host-function bridging for `register_fn!`.
//!
//! The low-level [`Engine::register_fn`](crate::engine::Engine::register_fn)
//! takes raw `Value`s and an explicit `Vec<Ty>` signature. That is flexible but
//! verbose and easy to get wrong: a signature that disagrees with the callback
//! makes the VM reinterpret bits under the wrong type. This module supplies the
//! typed layer instead:
//!
//! ```text
//! register_fn!(engine, "total", (Opaque<Order>) -> f64, |o| o.total);
//! ```
//!
//! The macro reads the parameter/return types from the Rust signature, derives
//! `Ty` via [`ScriptType`]/[`HostResult`], and wraps the callback with checked
//! `Value` conversions. No proc-macro, no dependencies, no runtime type tags.

use crate::error::{Error, Result};
use crate::ty::Ty;
use crate::value::{short_type_name, Handle, Value};
use std::any::type_name;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;

/// A Rust type with a script-facing type. Implemented for all supported
/// argument and result types; `Opaque<T>` maps to `Ty::Handle("T")`.
pub trait ScriptType {
    fn script_ty() -> Ty;
}

/// Checked conversion from a runtime `Value` into a typed host argument.
/// Every failure is an `Err`, never a panic.
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self>;
}

/// Convenience bound for a complete host argument type.
pub trait HostArg: ScriptType + FromValue {}
impl<T: ScriptType + FromValue> HostArg for T {}

/// A typed host return value. `script_ty()` supplies the signature seen by the
/// type checker; `into_value()` performs the (possibly fallible) conversion.
///
/// `Result<T, Error>` is also a `HostResult`, so a callback may fail with a
/// normal engine error instead of panicking.
pub trait HostResult {
    fn script_ty() -> Ty;
    fn into_value(self) -> Result<Value>;
}

/// An opaque handle to a host-owned Rust value.
///
/// Scripts can only pass these back to host functions; they cannot construct
/// one, read its fields, or downcast it to another type. `FromValue` validates
/// both the registered type name and the concrete downcast at the boundary, so
/// [`Opaque::get`] cannot fail afterwards.
pub struct Opaque<T: 'static> {
    handle: Handle,
    _marker: PhantomData<T>,
}

impl<T: 'static> Opaque<T> {
    /// Wrap a host value for passing into a script call (`Value::from(o)`).
    pub fn new(value: T) -> Self {
        Self { handle: Handle::new(value), _marker: PhantomData }
    }

    /// Borrow the underlying host value. The downcast is verified by
    /// `FromValue` before the callback is invoked.
    pub fn get(&self) -> &T {
        self.handle
            .data
            .downcast_ref::<T>()
            .expect("Opaque: downcast already validated at the host boundary")
    }
}

impl<T: 'static> Clone for Opaque<T> {
    fn clone(&self) -> Self {
        Self { handle: self.handle.clone(), _marker: PhantomData }
    }
}

impl<T: 'static> Deref for Opaque<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.get()
    }
}

impl<T: 'static> std::fmt::Debug for Opaque<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Opaque<{}>", self.handle.type_name)
    }
}

impl<T: 'static> From<Opaque<T>> for Value {
    fn from(o: Opaque<T>) -> Self {
        Value::Handle(o.handle)
    }
}

impl<T: 'static> ScriptType for Opaque<T> {
    fn script_ty() -> Ty {
        Ty::Handle(short_type_name(type_name::<T>()))
    }
}

impl<T: 'static> FromValue for Opaque<T> {
    fn from_value(v: &Value) -> Result<Self> {
        let h = match v {
            Value::Handle(h) => h,
            _ => return Err(Error::runtime(format!("expected handle, found {}", v.ty()))),
        };
        let expected = short_type_name(type_name::<T>());
        if h.type_name != expected || h.data.downcast_ref::<T>().is_none() {
            return Err(Error::runtime(format!(
                "handle type mismatch: expected {}, found {}",
                expected, h.type_name
            )));
        }
        Ok(Self { handle: h.clone(), _marker: PhantomData })
    }
}

impl<T: 'static> HostResult for Opaque<T> {
    fn script_ty() -> Ty {
        <Self as ScriptType>::script_ty()
    }
    fn into_value(self) -> Result<Value> {
        Ok(Value::Handle(self.handle))
    }
}

/// Pull the next argument from a host call, advancing the cursor.
/// Arity is checked once by the wrapper before any conversion happens.
pub(crate) fn take_arg<T: FromValue>(args: &[Value], idx: &mut usize) -> Result<T> {
    let v = args.get(*idx).ok_or_else(|| {
        Error::runtime(format!("host function argument {} is missing", idx))
    })?;
    *idx += 1;
    T::from_value(v)
}

macro_rules! scalar {
    ($t:ty, $ty:expr, $from:expr, $into:expr) => {
        impl ScriptType for $t {
            fn script_ty() -> Ty { $ty }
        }
        impl FromValue for $t {
            fn from_value(v: &Value) -> Result<Self> { $from(v) }
        }
        impl HostResult for $t {
            fn script_ty() -> Ty { $ty }
            fn into_value(self) -> Result<Value> { Ok($into(self)) }
        }
    };
}

scalar!(i64, Ty::I64, |v: &Value| v.as_i64(), Value::I64);
scalar!(f64, Ty::F64, |v: &Value| v.as_f64(), Value::F64);
scalar!(bool, Ty::Bool, |v: &Value| v.as_bool(), Value::Bool);
scalar!(String, Ty::Str, |v: &Value| Ok(v.as_str()?.to_string()), |s: String| Value::from(s));
scalar!(Rc<str>, Ty::Str, |v: &Value| Ok(Rc::from(v.as_str()?)), Value::Str);

impl ScriptType for () {
    fn script_ty() -> Ty { Ty::Unit }
}
impl FromValue for () {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Unit => Ok(()),
            _ => Err(Error::runtime(format!("expected (), found {}", v.ty()))),
        }
    }
}
impl HostResult for () {
    fn script_ty() -> Ty { Ty::Unit }
    fn into_value(self) -> Result<Value> { Ok(Value::Unit) }
}

impl<'a> HostResult for &'a str {
    fn script_ty() -> Ty { Ty::Str }
    fn into_value(self) -> Result<Value> { Ok(Value::from(self)) }
}

impl<T: HostResult> HostResult for std::result::Result<T, Error> {
    fn script_ty() -> Ty { T::script_ty() }
    fn into_value(self) -> Result<Value> { self?.into_value() }
}

// Fixed-arity tuples are the script's multi-value type. Variable-length `Vec`
// is deliberately not supported: a host signature must have a static shape.
// Arity 2 was the original limit (ADR-013 recorded "host tuples stop at 2,
// use Opaque beyond that"); that was a bridging-layer gap, not a language one
// — the script language itself accepts tuples up to MAX_OPERANDS — so the
// bridge now covers 2–8. Collections of unknown length (record lists) still
// belong in `Opaque<T>` + indexed getters.
macro_rules! impl_tuple_n {
    ($n:literal; $($idx:tt => $T:ident),+ $(,)?) => {
        impl<$($T: ScriptType),+> ScriptType for ($($T,)+) {
            fn script_ty() -> Ty {
                Ty::Tuple(vec![$(<$T as ScriptType>::script_ty()),+])
            }
        }
        impl<$($T: FromValue),+> FromValue for ($($T,)+) {
            fn from_value(v: &Value) -> Result<Self> {
                match v {
                    Value::Tuple(items) if items.len() == $n => {
                        Ok(($( <$T as FromValue>::from_value(&items[$idx])?, )+))
                    }
                    _ => Err(Error::runtime(format!(
                        "expected {}-tuple, found {}",
                        $n,
                        v.ty()
                    ))),
                }
            }
        }
        impl<$($T: HostResult),+> HostResult for ($($T,)+) {
            fn script_ty() -> Ty {
                Ty::Tuple(vec![$(<$T as HostResult>::script_ty()),+])
            }
            fn into_value(self) -> Result<Value> {
                Ok(Value::Tuple(vec![$(self.$idx.into_value()?),+]))
            }
        }
    };
}
impl_tuple_n!(2; 0 => A, 1 => B);
impl_tuple_n!(3; 0 => A, 1 => B, 2 => C);
impl_tuple_n!(4; 0 => A, 1 => B, 2 => C, 3 => D);
impl_tuple_n!(5; 0 => A, 1 => B, 2 => C, 3 => D, 4 => E);
impl_tuple_n!(6; 0 => A, 1 => B, 2 => C, 3 => D, 4 => E, 5 => F);
impl_tuple_n!(7; 0 => A, 1 => B, 2 => C, 3 => D, 4 => E, 5 => F, 6 => G);
impl_tuple_n!(8; 0 => A, 1 => B, 2 => C, 3 => D, 4 => E, 5 => F, 6 => G, 7 => H);

/// Register a host function with a Rust-typed signature.
///
/// ```
/// use brook::{register_fn, Engine, Opaque, Value};
/// # struct Order { total: f64 }
/// # fn main() -> brook::Result<()> {
/// let mut engine = Engine::new();
/// register_fn!(engine, "total", (Opaque<Order>) -> f64, |o| o.total)?;
/// register_fn!(engine, "add", (i64, i64), |a, b| a + b)?;       // inferred result
/// register_fn!(engine, "checked", (f64) -> brook::Result<f64>, |x| {
///     if x < 0.0 { Err(brook::Error::runtime("negative")) } else { Ok(x) }
/// })?;
/// let script = engine.load_str("func go(o) { add(to_i64(total(o)), 1) }")?;
/// let order = Value::from(Opaque::new(Order { total: 12.0 }));
/// assert_eq!(engine.call(&script, "go", vec![order])?, Value::I64(13));
/// # Ok(())
/// # }
/// ```
///
/// The `-> T` form checks the callback's return type at compile time; the short
/// form infers it. Both derive the script-facing signature from Rust types, so
/// it can never drift from the callback. Supports 0–6 arguments, scalars,
/// `String`/`Rc<str>`, `()`, 2-tuples, `Opaque<T>`, and `Result<T, Error>`.
#[macro_export]
macro_rules! register_fn {
    ($eng:expr, $name:expr, () -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed0::<$r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed1::<$a, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed2::<$a, $b, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed3::<$a, $b, $c, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed4::<$a, $b, $c, $d, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty, $e:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed5::<$a, $b, $c, $d, $e, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty, $e:ty, $g:ty) -> $r:ty, $f:expr $(,)?) => {
        ($eng).register_typed6::<$a, $b, $c, $d, $e, $g, $r, _>($name, $f)
    };
    ($eng:expr, $name:expr, (), $f:expr $(,)?) => {
        ($eng).register_typed0::<_, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty), $f:expr $(,)?) => {
        ($eng).register_typed1::<$a, _, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty), $f:expr $(,)?) => {
        ($eng).register_typed2::<$a, $b, _, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty), $f:expr $(,)?) => {
        ($eng).register_typed3::<$a, $b, $c, _, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty), $f:expr $(,)?) => {
        ($eng).register_typed4::<$a, $b, $c, $d, _, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty, $e:ty), $f:expr $(,)?) => {
        ($eng).register_typed5::<$a, $b, $c, $d, $e, _, _>($name, $f)
    };
    ($eng:expr, $name:expr, ($a:ty, $b:ty, $c:ty, $d:ty, $e:ty, $g:ty), $f:expr $(,)?) => {
        ($eng).register_typed6::<$a, $b, $c, $d, $e, $g, _, _>($name, $f)
    };
}
