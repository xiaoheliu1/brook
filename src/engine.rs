//! Public engine API: registration, compilation, calling, and hot reload.

use crate::ast;
use crate::cache::{self, HostFingerprint};
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::host::{take_arg, HostArg, HostResult, ScriptType};
use crate::ir;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::span::Span;
use crate::ty::Ty;
use crate::typecheck::{Checker, FuncSig, SigTable};
use crate::validate;
use crate::value::{short_type_name, Value};
use crate::vm::Vm;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// 约定式 import 的依赖清单：(解析后路径, 源码 hash)。
    /// `load_cached` / `compile_cached` 用它做时效校验——任一依赖文件变更，
    /// 缓存自动失效重建。`load_str` 场景没有依赖，为空。
    pub deps: Vec<(String, u64)>,
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
    /// Cap on bytes a script may allocate inside the VM (strings + tuple payloads).
    max_heap: u64,
    /// 约定式 import 的查找目录：`import "common"` → `<目录>/common.vt`。
    import_dir: Option<PathBuf>,
    /// 约定式 import 的自定义解析器——设置时优先于 `import_dir`，
    /// 宿主决定 `import "名字"` 的源码内容（assets / DB / 内置字符串）。
    import_resolver: Option<Rc<dyn Fn(&str) -> std::result::Result<String, Error>>>,
}

impl Engine {
    pub fn new() -> Self {
        let host = HostRegistry::new();
        Self {
            host: Rc::new(host),
            max_steps: 10_000_000,
            max_depth: 256,
            max_heap: 4 << 20,
            import_dir: None,
            import_resolver: None,
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

    /// 约定式 import 的目录：`import "common"` → `<目录>/common.vt`。
    /// 与 set_import_resolver 二选一（设置了 resolver 时优先生效）。
    pub fn set_import_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.import_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// 约定式 import 的自定义解析器：宿主决定 `import "名字"` 的源码内容
    /// （Android assets、数据库、内置字符串……引擎不做任何路径假设）。
    /// 硬性护栏（禁 `..` / 绝对路径 / 盘符）由引擎强制；resolver 内的
    /// 额外校验是宿主的责任。
    pub fn set_import_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> std::result::Result<String, Error> + 'static,
    {
        self.import_resolver = Some(std::rc::Rc::new(f));
        self
    }

    /// Compile source text into runnable bytecode (parse + typecheck + codegen).
    /// 约定式 import：源码里的 `import "名字"` 会从脚本所在目录
    /// （或 set_import_dir / set_import_resolver 配置的位置）载入库源码，
    /// 合并为一份编译单元。仅文件脚本支持（load_str 里写 import 会报错）。
    pub fn compile(&self, src: &str, name: impl Into<String>) -> Result<Script> {
        self.compile_inner(src, name.into(), None)
    }

    pub fn load_str(&self, src: &str) -> Result<Script> {
        self.compile_inner(src, "<string>".to_string(), None)
    }

    pub fn load_file<P: AsRef<Path>>(&self, path: P) -> Result<Script> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("{}: {}", path.display(), e)))?;
        let base = path.parent().map(|p| p.to_path_buf());
        self.compile_inner(&src, path.to_string_lossy().to_string(), base)
    }

    fn compile_inner(&self, src: &str, name: String, base_dir: Option<PathBuf>) -> Result<Script> {
        // —— 约定式 import：依赖先载入，主脚本最后（拼接视图，见 HOST_GUIDE §12.1）——
        let loader = ImportLoader::new(self, base_dir);
        let (program, combined, deps) = loader.load(src, &name)?;

        let host_sigs = self.host.signatures();
        let sigs = Checker::new(&host_sigs).check(&program)?;
        let code = Codegen::new(&sigs, self.host.ids()).compile(&program)?;
        validate::validate(&code, self.host.len())?;

        Ok(Script {
            code,
            sigs,
            name,
            source_hash: cache::hash_source(src),
            // 拼接视图（含依赖源码）：报错行号按它算，渲染与行号对得上
            source: Rc::from(combined.as_str()),
            deps,
        })
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
                // 主源码 hash + 每个依赖文件 hash 全部命中才算缓存有效——
                // 改一个 lib 文件，依赖它的脚本缓存自动失效重建
                if script.source_hash == hash && ImportLoader::deps_fresh(&script.deps) {
                    script.name = name.into();
                    // A cache hit still reports against the caller's source.
                    // 有依赖的脚本保留缓存内的拼接视图源码（报错行号按它算）；
                    // 无依赖时才用调用方源码刷新（等价于拼接视图）。
                    if script.deps.is_empty() {
                        script.source = Rc::from(src);
                    }
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

// ---------------------------------------------------------------------------
// 约定式 import：依赖载入与合并（脚本作者零配置，见 TUTORIAL §4.7）
// ---------------------------------------------------------------------------

/// 约定式 import 的上限（防 I/O 放大与失控嵌套）
const MAX_IMPORT_UNITS: usize = 32; // 单次编译最多 32 份文件（含主脚本）
const MAX_IMPORT_DEPTH: usize = 8; // import 链最深 8 层
const MAX_IMPORT_BYTES: u64 = 1 << 20; // 单份 1 MiB（与字符串上限同口径）

struct ImportUnit {
    /// 显示路径（报错与依赖清单用）
    path: String,
    src: String,
    /// 按 first_line 偏移解析出的 AST（拼接视图行号）
    program: ast::Program,
    is_main: bool,
}

struct ImportLoader<'a> {
    engine: &'a Engine,
    base_dir: Option<PathBuf>,
    /// 当前 DFS 路径（正在加载的单元 key）——命中即真循环
    stack: Vec<String>,
    /// 已完成加载的单元 key——重复 import 幂等去重（静默跳过）
    loaded: std::collections::HashSet<String>,
    /// 拼接顺序：依赖在前、主脚本最后
    units: Vec<ImportUnit>,
    combined: String,
    line_count: usize,
}

impl<'a> ImportLoader<'a> {
    fn new(engine: &'a Engine, base_dir: Option<PathBuf>) -> Self {
        Self {
            engine,
            base_dir,
            stack: Vec::new(),
            loaded: std::collections::HashSet::new(),
            units: Vec::new(),
            combined: String::new(),
            line_count: 1,
        }
    }

    /// 载入主脚本（含递归依赖，依赖在前）。
    /// 返回 (合并 AST, 拼接视图文本, 依赖清单)。
    fn load(
        mut self,
        src: &str,
        name: &str,
    ) -> Result<(ast::Program, String, Vec<(String, u64)>)> {
        self.load_unit(name.to_string(), src.to_string(), true, 0, self.base_dir.clone())?;
        let mut funcs = Vec::new();
        for unit in &self.units {
            funcs.extend(unit.program.funcs.iter().cloned());
        }
        let deps = self
            .units
            .iter()
            .filter(|u| !u.is_main)
            .map(|u| (u.path.clone(), cache::hash_source(&u.src)))
            .collect();
        Ok((
            ast::Program {
                imports: Vec::new(),
                funcs,
            },
            self.combined.clone(),
            deps,
        ))
    }

    fn load_unit(
        &mut self,
        path: String,
        src: String,
        is_main: bool,
        depth: usize,
        base_dir: Option<PathBuf>,
    ) -> Result<()> {
        if depth > MAX_IMPORT_DEPTH {
            return Err(Error::type_error(
                format!("import 链超过 {} 层", MAX_IMPORT_DEPTH),
                Span::synthetic(),
            ));
        }
        if self.units.len() >= MAX_IMPORT_UNITS {
            return Err(Error::type_error(
                format!("import 数量超过上限 ({})", MAX_IMPORT_UNITS - 1),
                Span::synthetic(),
            ));
        }
        // 幂等去重 + 循环检测，两个集合各管一件事：
        //   loaded 已完成 → 重复 import 静默跳过（幂等）
        //   stack 正在加载 → A 引 B、B 引 A 的真循环，编译报错
        let key = std::fs::canonicalize(&path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.clone());
        if self.loaded.contains(&key) {
            return Ok(());
        }
        if self.stack.iter().any(|s| s == &key) {
            return Err(Error::type_error(
                format!("import 循环: \"{path}\""),
                Span::synthetic(),
            ));
        }
        self.stack.push(key.clone());

        // 第一遍解析：提取本单元的 import 名单（span 为单元本地行号，
        // 仅用于定位依赖声明；最终 AST 在下方按拼接视图行号重解析）
        let program0 = Parser::new(Lexer::new(&src).tokenize()?)
            .parse()
            .map_err(|e| {
                Error::parse(
                    format!("{} (来自 \"{}\")", e.message, path),
                    e.span.unwrap_or_default(),
                )
            })?;

        // 递归：依赖先 placement（依赖在前、主脚本最后）
        for imp in &program0.imports {
            Self::validate_import_name(&imp.name, imp.span)?;
            let (dep_path, dep_src, dep_base) =
                self.resolve(base_dir.as_deref(), &imp.name, imp.span)?;
            self.load_unit(dep_path, dep_src, false, depth + 1, dep_base)?;
        }

        // 依赖已占用拼接视图行位 → 本单元起始行 = 当前行数
        let first_line = self.line_count;
        let program = Parser::new(Lexer::new_with_offset(&src, first_line).tokenize()?)
            .parse()
            .map_err(|e| {
                Error::parse(
                    format!("{} (来自 \"{}\")", e.message, path),
                    e.span.unwrap_or_default(),
                )
            })?;

        // 拼接视图累加（文本与行号计数严格同步）
        self.combined.push_str(&src);
        if !self.combined.ends_with('\n') {
            self.combined.push('\n');
        }
        self.line_count = 1 + self.combined.matches('\n').count();
        self.units.push(ImportUnit {
            path,
            src,
            program,
            is_main,
        });
        // 本单元完成：弹出 DFS 路径、标记已完成
        self.stack.pop();
        self.loaded.insert(key);
        Ok(())
    }

    fn resolve(
        &self,
        base_dir: Option<&Path>,
        name: &str,
        span: Span,
    ) -> Result<(String, String, Option<PathBuf>)> {
        Self::validate_import_name(name, span)?;

        // 自定义解析器优先（assets / DB / 加密等特殊部署）
        if let Some(res) = &self.engine.import_resolver {
            let src = res(name).map_err(|e| {
                Error::type_error(
                    format!("import \"{}\" 解析失败: {}", name, e.message),
                    span,
                )
            })?;
            return Ok((name.to_string(), src, None));
        }

        // 目录便捷式：import_dir 优先，其次脚本自身目录
        let dir = self
            .engine
            .import_dir
            .clone()
            .or_else(|| base_dir.map(|p| p.to_path_buf()))
            .ok_or_else(|| {
                Error::type_error(
                    "import 需要宿主配置：set_import_dir 或 set_import_resolver（内存脚本 load_str 无法解析 import）".to_string(),
                    span,
                )
            })?;
        let path = dir.join(format!("{name}.vt"));
        let meta = path
            .metadata()
            .map_err(|e| Error::io(format!("找不到 import 文件 {}: {}", path.display(), e)))?;
        if meta.len() > MAX_IMPORT_BYTES {
            return Err(Error::type_error(
                format!(
                    "import 文件过大: {} ({} bytes > {})",
                    path.display(),
                    meta.len(),
                    MAX_IMPORT_BYTES
                ),
                span,
            ));
        }
        let src = std::fs::read_to_string(&path).map_err(|e| {
            Error::io(format!("读取 import 文件失败 {}: {}", path.display(), e))
        })?;
        Ok((
            path.display().to_string(),
            src,
            path.parent().map(|p| p.to_path_buf()),
        ))
    }

    /// import 名字的硬性规则：相对路径、'/' 分隔、禁 '.'/'..'、禁盘符。
    /// 这是引擎义务（内置查找的安全护栏），宿主 resolver 可自行加码。
    fn validate_import_name(name: &str, span: Span) -> Result<()> {
        if name.is_empty() || name.len() > 255 {
            return Err(Error::type_error("import 路径长度非法", span));
        }
        if name.starts_with('/') || name.contains('\\') || name.contains(':') {
            return Err(Error::type_error(
                format!("import 路径必须是相对路径且使用 '/' 分隔: \"{name}\""),
                span,
            ));
        }
        for comp in name.split('/') {
            if comp.is_empty() || comp == "." || comp == ".." {
                return Err(Error::type_error(
                    format!("import 路径不允许 '.' / '..' / 空段: \"{name}\""),
                    span,
                ));
            }
        }
        Ok(())
    }

    /// 依赖时效校验：逐个读依赖文件、重算 hash 对比。任何一个读不到或
    /// 内容变了 → 缓存失效（重建会重新走 import 解析，错误如实报出）。
    fn deps_fresh(deps: &[(String, u64)]) -> bool {
        deps.iter().all(|(path, hash)| {
            std::fs::read_to_string(path)
                .map(|text| cache::hash_source(&text) == *hash)
                .unwrap_or(false)
        })
    }
}
