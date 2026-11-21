//! brook — a minimal, statically-typed embedded scripting language.
//!
//! The default execution path is a type-specialized register VM. A script is
//! parsed, type-checked, and lowered to bytecode by [`Engine`]; [`Engine::call`]
//! runs one function in a fresh VM with gas, depth, and heap limits.
//! `eval::Interpreter` remains a reference implementation and is not the
//! security boundary.
//!
//! Host functions are exposed with [`register_fn`], which derives the
//! script-facing signature from Rust types. [`Opaque`] passes host-owned Rust
//! values through scripts without exposing their fields. Compiled scripts can
//! be serialized to `.sbc` bytes and reloaded when the host registry matches.
//!
//! Every stage reports a source location, and runtime errors also capture the
//! interpreted call stack. [`Error::report`] (or `Script::report`) renders that
//! as a `file:line:col` snippet with a caret and the stack; `Display` stays a
//! single line for logs and the C ABI.
//!
//! One engine per thread: [`Engine`], [`Script`] and [`Value`] are `Rc`-backed
//! and therefore not `Send`. A multi-threaded host (JNI callbacks arrive on
//! arbitrary threads) registers its own callbacks per thread rather than
//! sharing an engine behind a lock; `reload` takes `&mut Script` while `call`
//! takes `&Script`, so safe Rust already rules out reloading mid-call.
//!
//! ```no_run
//! use brook::{register_fn, Engine, Result, Value};
//!
//! fn main() -> Result<()> {
//!     let mut engine = Engine::new();
//!     register_fn!(engine, "log", (String), |s| println!("{}", s))?;
//!     let script = engine.load_str("func go(x) { log(to_str(x)) x * 2 }")?;
//!     let value = engine.call(&script, "go", vec![Value::I64(21)])?;
//!     assert_eq!(value, Value::I64(42));
//!     Ok(())
//! }
//! ```

pub mod ast;
pub mod cache;
pub mod codegen;
pub mod diag;
pub mod engine;
pub mod error;
pub mod eval;
pub mod ffi;
pub mod host;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod ty;
pub mod typecheck;
pub mod validate;
pub mod value;
pub mod vm;

pub use engine::{Engine, Script};
pub use error::{Error, ErrorKind, Result, TraceFrame};
pub use host::Opaque;
pub use span::Span;
pub use value::{Handle, Value};
