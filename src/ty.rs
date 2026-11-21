//! Type representation.
//!
//! `Ty::Var` is an inference variable used during unification. After a
//! successful check every variable in a function signature is resolved.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    I64,
    F64,
    Bool,
    Str,
    /// Opaque host handle, identified by the Rust type's name.
    Handle(String),
    Tuple(Vec<Ty>),
    /// Homogeneous array (ADR-018). Element type is a single compile-time
    /// type — an array of tuples is `Arr(Tuple([..]))`, which is what lets
    /// host functions hand over record lists (`fs_list` 等) with full
    /// static typing. Reference semantics live in the runtime representation,
    /// not here: assignment and argument passing alias the same array.
    Arr(Box<Ty>),
    /// Inference variable. Never escapes a successful typecheck.
    Var(usize),
    /// The unit value (empty tuple / no value).
    Unit,
}

impl Ty {
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::I64 | Ty::F64)
    }

    /// Display form used in diagnostics and optional annotations.
    pub fn render(&self) -> String {
        match self {
            Ty::I64 => "i64".into(),
            Ty::F64 => "f64".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Handle(n) => n.clone(),
            Ty::Tuple(ts) => {
                let inner = ts.iter().map(|t| t.render()).collect::<Vec<_>>().join(", ");
                format!("({})", inner)
            }
            Ty::Arr(t) => format!("[{}]", t.render()),
            Ty::Var(id) => format!("?{}", id),
            Ty::Unit => "()".into(),
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render())
    }
}
