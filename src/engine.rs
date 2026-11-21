//! Public engine API: registration, compilation, calling, and hot reload.

use crate::cache::{self, HostFingerprint};
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::host::{take_arg, HostArg, HostResult, ScriptType};
use crate::ir;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::ty::Ty;
use crate::typecheck::{Checker, FuncSig, SigTable};
use crate::validate;
use crate::value::{short_type_name, Value};
use crate::vm::Vm;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

type NativeFn = dyn Fn(&[Value]) -> Result<Value>;

/// A host function together with its stable slot id and script-facing signature.
struct HostEntry {
    sig: FuncSig,
    f: Box<NativeFn>,
}

/// Registry of host-provided functions. Slot ids are assigned in registration
/// order so bytecode can refer to functions by a stable `u16` (no name lookup
/// at runtime).
pub struct HostRegistry {
    entries: Vec<HostEntry>,
    ids: HashMap<String, u16>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self { entries: Vec::new(), ids: HashMap::new() }
    }

    pub fn register(
        &mut self,
        name: impl Into<String>,
        params: Vec<Ty>,
        ret: Ty,
        f: Box<NativeFn>,
    ) -> Result<()> {
        let name = name.into();
        if self.ids.contains_key(&name) {
            return Err(Error::runtime(format!("host function `{}` is already registered", name)));
        }
        let id = self.entries.len() as u16;
        self.ids.insert(name, id);
        self.entries.push(HostEntry { sig: FuncSig { params, ret }, f });
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.ids.contains_key(name)
    }

    pub fn id_of(&self, name: &str) -> Option<u16> {
        self.ids.get(name).copied()
    }

    pub fn sig_by_id(&self, id: u16) -> &FuncSig {
        &self.entries[id as usize].sig
    }

    pub fn invoke_by_id(&self, id: u16, args: &[Value]) -> Result<Value> {
        let entry = self.entries.get(id as usize).ok_or_else(|| Error::runtime("unknown host function"))?;
        (entry.f)(args)
    }

    /// Name-based lookup retained for the reference AST interpreter (`eval.rs`).
    /// The bytecode VM never uses this path — calls are resolved to slot ids.
    pub fn invoke(&self, name: &str, args: &[Value]) -> Result<Value> {
        let id = self.ids.get(name).ok_or_else(|| Error::runtime("unknown host function"))?;
        self.invoke_by_id(*id, args)
    }

    /// Number of registered host slots; `CallHost.func` must be below it.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn signatures(&self) -> HashMap<String, FuncSig> {
        self.ids
            .iter()
            .map(|(name, &id)| (name.clone(), self.entries[id as usize].sig.clone()))
            .collect()
    }

    pub fn ids(&self) -> &HashMap<String, u16> {
        &self.ids
    }

    /// Stable snapshot of (name, slot id, params, ret), sorted by name.
    /// `CallHost` bytecode embeds slot ids, so a cache is only valid against
    /// the exact registry it was compiled with.
    pub fn fingerprint(&self) -> HostFingerprint {
        let mut v: HostFingerprint = self
            .ids
            .iter()
            .map(|(name, &id)| {
                let sig = &self.entries[id as usize].sig;
                (name.clone(), id, sig.params.clone(), sig.ret.clone())
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// A compiled script: typed bytecode plus the signature table and source name.
#[derive(Clone, Debug)]
pub struct Script {
    pub code: ir::Program,
    pub sigs: SigTable,
    /// Source name, e.g. a file path; exposed for diagnostics.
    pub name: String,
    /// FNV-1a hash of the source text used to detect stale `.sbc` caches.
    pub source_hash: u64,
    /// Source text, kept for [`Script::report`]. Empty for a script decoded
    /// straight from `.sbc` bytes, where no source was supplied.
    pub source: Rc<str>,
}

impl Script {
    /// Render an error against this script's own source and name.
    ///
    /// ```ignore
    /// if let Err(e) = engine.call(&script, "check", args) {
    ///     eprintln!("{}", script.report(&e));
    /// }
    /// ```
    pub fn report(&self, err: &crate::Error) -> String {
        err.report(&self.source, &self.name)
    }
}

// Generates one typed-registration method per arity. Rust has no stable way to
// apply a tuple as a closure's argument list, so arity is fixed at the call
// site. Each wrapper derives its script signature from the Rust types, checks
// the arity, converts arguments with `FromValue`, and delegates to the raw
// `Engine::register_fn`. The macro must live outside the `impl` block: macro
// definitions are module items, not associated items.
macro_rules! typed_registration {
    ($method:ident $(, $T:ident : $v:ident)* $(,)?) => {
        #[doc(hidden)]
        pub fn $method<$($T,)* R, F>(&mut self, name: impl Into<String>, f: F) -> Result<()>
        where
            $($T: HostArg,)*
            R: HostResult,
            F: Fn($($T,)*) -> R + 'static,
        {
            let params = vec![$(<$T as ScriptType>::script_ty(),)*];
            let arity = params.len();
            let ret = <R as HostResult>::script_ty();
            self.register_fn(name, params, ret, move |args| {
                if args.len() != arity {
                    return Err(Error::runtime(format!(
                        "host function arity mismatch: expected {}, got {}",
                        arity,
                        args.len()
                    )));
                }
                #[allow(unused_mut, unused_variables)]
                let mut idx = 0usize;
                $(
                    let $v = take_arg::<$T>(args, &mut idx)?;
                )*
                f($($v,)*).into_value()
            })
        }
    };
}

/// The scripting engine.
///
/// Neither `Engine` nor [`Script`] is `Send`/`Sync`: host callbacks are plain
/// `Fn` closures that may capture `Rc` / `RefCell` state, and the program and
/// registry are `Rc`-backed. A multi-threaded host builds **one engine per
/// thread** — registering a handful of functions costs far less than a shared
/// lock on every call, and each thread then owns its own bytecode. This is a
/// compile-time guarantee, not a convention: passing either type across
/// threads does not compile.
pub struct Engine {
    host: Rc<HostRegistry>,
    max_steps: u64,
    max_depth: usize,
    /// Cap on bytes allocated inside the VM (strings + tuple payloads).
    max_heap: u64,
}

impl Engine {
    pub fn new() -> Self {
        let host = HostRegistry::new();
        Self {
            host: Rc::new(host),
            max_steps: 10_000_000,
            max_depth: 256,
            max_heap: 4 << 20,
        }
    }

    /// Register a host function. `params`/`ret` describe its script-facing
    /// signature. The closure receives raw `Value`s (use helpers on `Value`).
    pub fn register_fn<F>(
        &mut self,
        name: impl Into<String>,
        params: Vec<Ty>,
        ret: Ty,
        f: F,
    ) -> Result<()>
    where
        F: Fn(&[Value]) -> Result<Value> + 'static,
    {
        let name = name.into();
        if crate::typecheck::is_builtin(&name) {
            return Err(Error::runtime(format!("`{}` is a reserved built-in", name)));
        }
        Rc::get_mut(&mut self.host)
            .ok_or_else(|| Error::runtime("cannot register: the host registry is already shared"))?
            .register(name, params, ret, Box::new(f))
    }

    // ---- typed registration (used by the `register_fn!` macro) ----
    typed_registration!(register_typed0);
    typed_registration!(register_typed1, A: a);
    typed_registration!(register_typed2, A: a, B: b);
    typed_registration!(register_typed3, A: a, B: b, C: c);
    typed_registration!(register_typed4, A: a, B: b, C: c, D: d);
    typed_registration!(register_typed5, A: a, B: b, C: c, D: d, E: e);
    typed_registration!(register_typed6, A: a, B: b, C: c, D: d, E: e, G: g);

    /// Register an opaque Rust type so handlers can receive it by name.
    pub fn register_type<T: 'static>(&mut self) -> Result<()> {
        let _ = short_type_name(std::any::type_name::<T>());
        Ok(())
    }

    pub fn with_limits(mut self, max_steps: u64, max_depth: usize) -> Self {
        self.max_steps = max_steps;
        self.max_depth = max_depth;
        self
    }

    /// Cap the bytes a script may allocate inside the VM (strings only —
    /// handles are host-owned and never copied).
    pub fn with_heap_limit(mut self, bytes: u64) -> Self {
        self.max_heap = bytes;
        self
    }

    /// Compile source text into runnable bytecode (parse + typecheck + codegen).
    pub fn compile(&self, src: &str, name: impl Into<String>) -> Result<Script> {
        let tokens = Lexer::new(src).tokenize()?;
        let program = Parser::new(tokens).parse()?;
        let host_sigs = self.host.signatures();
        let sigs = Checker::new(&host_sigs).check(&program)?;
        let code = Codegen::new(&sigs, self.host.ids()).compile(&program)?;
        validate::validate(&code, self.host.len())?;
        Ok(Script {
            code,
            sigs,
            name: name.into(),
            source_hash: cache::hash_source(src),
            source: Rc::from(src),
        })
    }

    pub fn load_str(&self, src: &str) -> Result<Script> {
        self.compile(src, "<string>")
    }

    pub fn load_file<P: AsRef<Path>>(&self, path: P) -> Result<Script> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("{}: {}", path.display(), e)))?;
        self.compile(&src, path.to_string_lossy().to_string())
    }

    /// Recompile a script from a file and replace its bytecode in place.
    ///
    /// Takes `&mut Script` on purpose: `call` takes `&Script`, so safe Rust
    /// cannot reload while a call is running. A call in flight keeps the
    /// bytecode it borrowed and finishes on it; the next call sees the new
    /// code. On failure the old script is left untouched.
    pub fn reload<P: AsRef<Path>>(&self, script: &mut Script, path: P) -> Result<()> {
        let fresh = self.load_file(path)?;
        script.code = fresh.code;
        script.sigs = fresh.sigs;
        script.name = fresh.name;
        script.source_hash = fresh.source_hash;
        // Keep `Script::report` pointing at the text that produced this
        // bytecode; otherwise carets would be placed against the old file.
        script.source = fresh.source;
        Ok(())
    }

    /// Serialize a compiled script into the `.sbc` format. Useful for an
    /// in-memory cache or for shipping bytecode without its source: the bytes
    /// are self-contained and pinned to this engine's host registry.
    pub fn script_to_bytes(&self, script: &Script) -> Vec<u8> {
        cache::encode(script, &self.host.fingerprint())
    }

    /// Deserialize `.sbc` bytes (no file I/O). Rejects bad magic, unsupported
    /// versions, corrupt/truncated data and registries that changed since the
    /// bytes were produced.
    pub fn script_from_bytes(&self, bytes: &[u8]) -> Result<Script> {
        cache::decode(bytes, &self.host.fingerprint())
    }

    /// Write a compiled script to a `.sbc` cache file.
    pub fn save_sbc<P: AsRef<Path>>(&self, script: &Script, path: P) -> Result<()> {
        let path = path.as_ref();
        let bytes = self.script_to_bytes(script);
        std::fs::write(path, bytes)
            .map_err(|e| Error::io(format!("{}: {}", path.display(), e)))
    }

    /// Load a `.sbc` file without touching the source file.
    pub fn load_sbc<P: AsRef<Path>>(&self, path: P) -> Result<Script> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| Error::io(format!("{}: {}", path.display(), e)))?;
        self.script_from_bytes(&bytes)
    }

    /// Compile source that the host already holds in memory, using `cache_path`
    /// as an optional bytecode cache. Only the cache file is read, so on a hit
    /// lexing/parsing/type-checking/codegen are all skipped. This is the
    /// preferred path on Android (source from assets / network payload).
    /// A stale or invalid cache is transparently rebuilt (best-effort write).
    pub fn compile_cached<P: AsRef<Path>>(
        &self,
        src: &str,
        name: impl Into<String>,
        cache_path: P,
    ) -> Result<Script> {
        let path = cache_path.as_ref();
        let hash = cache::hash_source(src);
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(mut script) = self.script_from_bytes(&bytes) {
                if script.source_hash == hash {
                    script.name = name.into();
                    // A cache hit still reports against the caller's source.
                    script.source = Rc::from(src);
                    return Ok(script);
                }
            }
        }
        let script = self.compile(src, name)?;
        // A read-only directory must not make loading fail: caching is optional.
        let _ = self.save_sbc(&script, path);
        Ok(script)
    }

    /// Compile from a source file, using a sibling `.sbc` cache when it is
    /// fresh. Reads both files (source for the hash, cache for the bytecode),
    /// so it is a convenience API rather than a fast path — see the benchmark
    /// notes in PROJECT_PLAN.md. Prefer `compile_cached` when the source is
    /// already in memory.
    pub fn load_cached<P: AsRef<Path>>(&self, path: P) -> Result<Script> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("{}: {}", path.display(), e)))?;
        self.compile_cached(&src, path.to_string_lossy().to_string(), path.with_extension("sbc"))
    }

    /// Hot reload backed by `load_cached` (cache hit skips recompilation).
    pub fn reload_cached<P: AsRef<Path>>(&self, script: &mut Script, path: P) -> Result<()> {
        *script = self.load_cached(path)?;
        Ok(())
    }

    /// Call a script function with runtime values.
    pub fn call(&self, script: &Script, name: &str, args: Vec<Value>) -> Result<Value> {
        // Coerce args to the declared parameter types (e.g. host passes i64 to
        // a parameter inferred as f64). Same semantics as the phase-1 evaluator.
        let sig = script.sigs.get(name).ok_or_else(|| {
            Error::runtime(format!("unknown function `{}`", name))
        })?;
        if sig.params.len() != args.len() {
            return Err(Error::runtime(format!(
                "`{}` expects {} args, got {}",
                name,
                sig.params.len(),
                args.len()
            )));
        }
        let coerced: Vec<Value> = args
            .into_iter()
            .zip(sig.params.iter())
            .map(|(v, t)| {
                let c = coerce(v, t)?;
                // Guard against inferred-default mismatches (e.g. a function
                // whose params defaulted to i64 receiving a str from the host).
                // Without this the VM would reinterpret the bits under the
                // wrong type and silently return a wrong result.
                if !type_matches(&c, t) {
                    return Err(Error::runtime(format!(
                        "argument type mismatch: expected {}, got {}. \
                         Annotate the parameter (e.g. `x: str`) to fix inference.",
                        t.render(),
                        c.ty()
                    )));
                }
                Ok(c)
            })
            .collect::<Result<Vec<_>>>()?;

        let func = script.code.id_of(name).ok_or_else(|| {
            Error::runtime(format!("`{}` is not a script function", name))
        })?;

        // Panic boundary: a bug in the VM (or in a host callback) must not
        // unwind through the host's call stack. This is a hard guarantee of the
        // engine API — see PROJECT_PLAN.md, "安全边界". Note it requires the
        // default `panic = "unwind"` strategy; with `panic = "abort"` the
        // process aborts instead (documented in Cargo.toml).
        let mut vm = Vm::new(
            &script.code,
            &self.host,
            self.max_steps,
            self.max_depth,
            self.max_heap,
        );
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.call(func, coerced))) {
            Ok(result) => result,
            Err(_) => Err(Error::runtime(
                "script aborted: internal panic (engine bug or panicking host function)",
            )),
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

fn coerce(v: Value, target: &Ty) -> Result<Value> {
    match (v, target) {
        (Value::I64(n), Ty::F64) => Ok(Value::F64(n as f64)),
        (v, _) => Ok(v),
    }
}

/// Whether a runtime value is compatible with a declared parameter type.
/// Handles are compared by their registered type name.
fn type_matches(v: &Value, ty: &Ty) -> bool {
    match (v, ty) {
        (Value::I64(_), Ty::I64) => true,
        (Value::F64(_), Ty::F64) => true,
        (Value::Bool(_), Ty::Bool) => true,
        (Value::Str(_), Ty::Str) => true,
        (Value::Unit, Ty::Unit) => true,
        (Value::Handle(h), Ty::Handle(name)) => &h.type_name == name,
        (Value::Tuple(vals), Ty::Tuple(tys)) => {
            // A param inferred from `p.0` is reverse-typed as a minimal tuple
            // (len = index+1); accept any tuple at least that long whose known
            // elements match. Excess elements are harmless for `.N` access.
            vals.len() >= tys.len() && vals.iter().zip(tys).all(|(x, t)| type_matches(x, t))
        }
        // A defaulted inference variable becomes I64; accept any scalar there
        // only if it round-trips (keeps host-called polymorphic funcs usable).
        (_, Ty::Var(_)) => true,
        _ => false,
    }
}
