//! Runtime values.
//!
//! The VM hot path uses unboxed register slots; `Value` remains the host
//! boundary type and the reference evaluator's runtime representation.

use crate::error::{Error, Result};
use crate::ty::Ty;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A host object handed to scripts. Carries its Rust type name so the type
/// checker and diagnostics can refer to it without TypeId-to-name magic.
#[derive(Clone)]
pub struct Handle {
    pub type_name: String,
    pub data: Rc<dyn std::any::Any>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Handle<{}>", self.type_name)
    }
}

impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        self.type_name == other.type_name && Rc::ptr_eq(&self.data, &other.data)
    }
}

impl Handle {
    pub fn new<T: 'static>(value: T) -> Self {
        Self {
            type_name: short_type_name(std::any::type_name::<T>()),
            data: Rc::new(value),
        }
    }
}

pub fn short_type_name(full: &str) -> String {
    full.rsplit("::").next().unwrap_or(full).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(Rc<str>),
    Handle(Handle),
    Tuple(Vec<Value>),
    /// Array (ADR-018). Shared via `Rc` so that the reference interpreter's
    /// `let b = a` behaves exactly like the bytecode VM's slot copy: both are
    /// aliases of the same array (Python-style reference semantics). Cycles
    /// are unconstructible (`push(a, a)` is an infinite type), so no GC.
    /// The host boundary treats an array as a snapshot: `FromValue for
    /// Vec<T>` clones elements out, and host-returned `Vec<T>` arrives as a
    /// fresh array.
    Arr(Rc<RefCell<Vec<Value>>>),
    Unit,
}

impl Value {
    pub fn handle<T: 'static>(value: T) -> Self {
        Value::Handle(Handle::new(value))
    }

    /// Build an array value from Rust elements (host side).
    pub fn arr(items: Vec<Value>) -> Self {
        Value::Arr(Rc::new(RefCell::new(items)))
    }

    /// Borrow the array payload, or `Err` for non-array values.
    pub fn as_array(&self) -> Result<std::cell::Ref<'_, Vec<Value>>> {
        match self {
            Value::Arr(items) => Ok(items.borrow()),
            _ => Err(Error::runtime(format!(
                "expected array, found {}",
                self.ty()
            ))),
        }
    }

    pub fn ty(&self) -> Ty {
        match self {
            Value::I64(_) => Ty::I64,
            Value::F64(_) => Ty::F64,
            Value::Bool(_) => Ty::Bool,
            Value::Str(_) => Ty::Str,
            Value::Handle(h) => Ty::Handle(h.type_name.clone()),
            Value::Tuple(v) => Ty::Tuple(v.iter().map(|x| x.ty()).collect()),
            // Empty arrays have no element to read; `Unit` renders as the
            // placeholder for diagnostics only (the static checker always
            // knows the real element type).
            Value::Arr(v) => {
                let elem = v.borrow().first().map(|x| x.ty()).unwrap_or(Ty::Unit);
                Ty::Arr(Box::new(elem))
            }
            Value::Unit => Ty::Unit,
        }
    }

    pub fn as_i64(&self) -> Result<i64> {
        match self {
            Value::I64(n) => Ok(*n),
            _ => Err(Error::runtime(format!("expected i64, found {}", self.ty()))),
        }
    }

    pub fn as_f64(&self) -> Result<f64> {
        match self {
            Value::F64(n) => Ok(*n),
            Value::I64(n) => Ok(*n as f64),
            _ => Err(Error::runtime(format!("expected f64, found {}", self.ty()))),
        }
    }

    pub fn as_bool(&self) -> Result<bool> {
        match self {
            Value::Bool(b) => Ok(*b),
            _ => Err(Error::runtime(format!(
                "expected bool, found {}",
                self.ty()
            ))),
        }
    }

    pub fn as_str(&self) -> Result<&str> {
        match self {
            Value::Str(s) => Ok(s.as_ref()),
            _ => Err(Error::runtime(format!("expected str, found {}", self.ty()))),
        }
    }

    pub fn as_handle<T: 'static>(&self) -> Result<&T> {
        match self {
            Value::Handle(h) => h.data.downcast_ref::<T>().ok_or_else(|| {
                Error::runtime(format!(
                    "handle type mismatch: expected {}, found {}",
                    short_type_name(std::any::type_name::<T>()),
                    h.type_name
                ))
            }),
            _ => Err(Error::runtime(format!(
                "expected handle, found {}",
                self.ty()
            ))),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::I64(n) => write!(f, "{}", n),
            Value::F64(n) => write!(f, "{}", n),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Str(s) => write!(f, "{}", s),
            Value::Handle(h) => write!(f, "<{}>", h.type_name),
            Value::Tuple(v) => {
                let parts: Vec<_> = v.iter().map(|x| x.to_string()).collect();
                write!(f, "({})", parts.join(", "))
            }
            Value::Arr(v) => {
                let parts: Vec<_> = v.borrow().iter().map(|x| x.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Unit => write!(f, "()"),
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::I64(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(Rc::from(v))
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(Rc::from(v))
    }
}
impl<T> From<Vec<T>> for Value
where
    T: Into<Value>,
{
    fn from(v: Vec<T>) -> Self {
        Value::Tuple(v.into_iter().map(|x| x.into()).collect())
    }
}
