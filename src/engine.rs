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

    /// `Err` 而不是直接索引（D-046 3.10：`invoke_by_id` 用 `.get()`，
    /// 两者不一致——一个 panic 一个 Err 的边界不是承诺的边界）。
    pub fn sig_by_id(&self, id: u16) -> Result<&FuncSig> {
        self.entries
            .get(id as usize)
            .map(|e| &e.sig)
            .ok_or_else(|| Error::runtime("unknown host function"))
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
    /// 约定式 import 的依赖清单：(解析后路径, 源码 hash, 是否文件来源)。
    /// `load_cached` / `compile_cached` 用它做时效校验——任一依赖文件变更，
    /// 缓存自动失效重建。resolver 来源的依赖由宿主自行决定失效时机。
    /// `load_str` 场景没有依赖，为空。
    pub deps: Vec<(String, u64, bool)>,
    /// 编译时宿主注册表的指纹（D-046 3.8）。缓存路径的指纹校验只存在于
    /// `cache::decode`——内存里的 `Script` 拿到别的 `Engine` 上调用时，
    /// `CallHost` 的槽位 id 会打到**另一个宿主的函数**（类型桥保证不 UB，
    /// 保证不了"调对了函数"）。`Engine::call` 用它做 O(n) 比对。
    pub host_fp: crate::cache::HostFingerprint,
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
    /// 约定式 import 的查找目录：`import "common"` → `<目录>/common.bk`。
    import_dir: Option<PathBuf>,
    /// 已注册的不透明类型名（`register_type`；防同名不同型混入诊断）
    type_names: std::collections::HashSet<String>,
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
            type_names: std::collections::HashSet::new(),
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
    // The language allows 8 arguments (`MAX_OPERANDS`); host callbacks should
    // be able to match that ceiling. `F` is skipped as a type-parameter name
    // because the macro uses it for the closure type.
    typed_registration!(register_typed7, A: a, B: b, C: c, D: d, E: e, G: g, H: h);
    typed_registration!(register_typed8, A: a, B: b, C: c, D: d, E: e, G: g, H: h, I: i);

    /// Register an opaque Rust type so handlers can receive it by name.
    /// 注册不透明类型名（供诊断与 `Opaque` 的名字校验对齐）。同名不同型
    /// 会被拒绝——静态类型系统按名字区分句柄，两个模块的同名类型在脚本
    /// 侧不可区分（运行时由 `FromValue` 的 downcast 兜底，见 5.8）。
    pub fn register_type<T: 'static>(&mut self) -> Result<()> {
        let name = short_type_name(std::any::type_name::<T>());
        if self.type_names.contains(&name) {
            return Err(Error::runtime(format!(
                "opaque type `{}` is already registered",
                name
            )));
        }
        self.type_names.insert(name);
        Ok(())
    }

    pub fn with_limits(mut self, max_steps: u64, max_depth: usize) -> Self {
        self.max_steps = max_steps;
        self.max_depth = max_depth;
        self
    }

    /// 本引擎宿主注册表的指纹（缓存构建/校验、脚本归属判断用）。
    pub fn fingerprint(&self) -> crate::cache::HostFingerprint {
        self.host.fingerprint()
    }

    /// Cap the bytes a script may allocate inside the VM: runtime strings,
    /// tuples and arrays all consume heap quota (including one slot per
    /// entry). Opaque handles are host-owned and never copied, but the slot
    /// holding the handle is still charged.
    pub fn with_heap_limit(mut self, bytes: u64) -> Self {
        self.max_heap = bytes;
        self
    }

    /// 约定式 import 的目录：`import "common"` → `<目录>/common.bk`。
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
            host_fp: self.host.fingerprint(),
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
        // deps 同步更新（D-046 5.1）：漏掉这一行会让热重载后的脚本带着
        // 旧依赖清单——后续 save_sbc 把它写进缓存，compile_cached 校验的
        // 就是错误的依赖集（例如 reload 前后 import_dir 变化时，缓存可能
        // 依据旧路径的文件判定"新鲜"而服役旧字节码）。reload_cached 用
        // `*script = ...` 不受影响；这里对齐两条路径。
        script.deps = fresh.deps;
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
                // 改一个 lib 文件，依赖它的脚本缓存自动失效重建。deps_fresh
                // 按 import_dir 做包含性收敛（D-046 3.7）。
                if script.source_hash == hash
                    && ImportLoader::deps_fresh(&script.deps, self.import_dir.as_deref())
                {
                    script.name = name.into();
                    // v6 起缓存携带拼接视图源码（D-046 5.2），命中后
                    // report 的源码行 / caret 直接可用。
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
        // 指纹比对（D-046 3.8）：跨 Engine 复用编译产物会静默调错宿主函数
        // ——缓存路径有 decode 的指纹校验兜底，内存路径此前完全没有。
        if script.host_fp != self.host.fingerprint() {
            return Err(Error::runtime(
                "script was compiled against a different host registry; \
                 recompile it on this engine",
            ));
        }
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
        (Value::Arr(items), Ty::Arr(t)) => {
            // Homogeneous arrays (ADR-018): every element must match. Empty
            // arrays trivially satisfy any element type.
            items.borrow().iter().all(|x| type_matches(x, t))
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
    /// 显示路径（报错与依赖清单用；resolver 来源为名字而非路径）
    path: String,
    src: String,
    /// 按 first_line 偏移解析出的 AST（拼接视图行号）
    program: ast::Program,
    is_main: bool,
    /// true = 文件来源（缓存可按路径校验时效）；false = resolver 来源
    /// （失效由宿主 resolver 自行决定，缓存校验跳过）
    is_file: bool,
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
    ) -> Result<(ast::Program, String, Vec<(String, u64, bool)>)> {
        self.load_unit(
            name.to_string(),
            src.to_string(),
            true,
            self.base_dir.is_some(),
            0,
            self.base_dir.clone(),
        )?;
        // `self` is consumed: move the ASTs and the combined source instead of
        // cloning every function (a hidden O(program size) cost on EVERY
        // compile, including scripts with no imports).
        let mut funcs = Vec::new();
        let mut deps = Vec::new();
        for unit in self.units {
            if !unit.is_main {
                deps.push((unit.path, cache::hash_source(&unit.src), unit.is_file));
            }
            funcs.extend(unit.program.funcs);
        }
        Ok((
            ast::Program {
                imports: Vec::new(),
                funcs,
            },
            self.combined,
            deps,
        ))
    }

    fn load_unit(
        &mut self,
        path: String,
        src: String,
        is_main: bool,
        is_file: bool,
        depth: usize,
        base_dir: Option<PathBuf>,
    ) -> Result<()> {
        if depth > MAX_IMPORT_DEPTH {
            return Err(Error::type_error(
                format!("import 链超过 {} 层", MAX_IMPORT_DEPTH),
                Span::synthetic(),
            ));
        }
        // 幂等去重 + 循环检测，两个集合各管一件事：
        //   loaded 已完成 → 重复 import 静默跳过（幂等）
        //   stack 正在加载 → A 引 B、B 引 A 的真循环，编译报错
        // 去重 key 只在【主文件脚本】上做 canonicalize：它可能以相对路径
        // 传入，需要与依赖的绝对路径认成同一份。依赖路径在 `resolve` 里
        // 已为安全护栏做过 canonicalize，虚拟名（`<string>` / resolver）
        // 则根本不是路径——对它们调 fs 是每次编译白付的 syscall。
        let key = if is_main && is_file {
            std::fs::canonicalize(&path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
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

        // 第一遍解析只为了拿 import 名单。`import` 是关键字，任何 Import
        // token 都必然在源码里留下 "import" 子串，所以 `contains` 是安全的
        // 过近似：没有这个子串时一定没有依赖，跳过整遍 lex+parse。
        // 有语法错误时仍由下方按拼接视图行号的那遍解析报错（同样带来源）。
        let imports: Vec<ast::ImportDecl> = if src.contains("import") {
            Parser::new(Lexer::new(&src).tokenize()?)
                .parse()
                .map_err(|e| {
                    Error::parse(
                        format!("{} (来自 \"{}\")", e.message, path),
                        e.span.unwrap_or_default(),
                    )
                })?
                .imports
        } else {
            Vec::new()
        };

        // 递归：依赖先 placement（依赖在前、主脚本最后）
        for imp in &imports {
            Self::validate_import_name(&imp.name, imp.span)?;
            let (dep_path, dep_src, dep_base, dep_is_file) =
                self.resolve(base_dir.as_deref(), &imp.name, imp.span)?;
            self.load_unit(dep_path, dep_src, false, dep_is_file, depth + 1, dep_base)?;
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

        // 拼接视图累加（行号计数增量维护——整段重扫是 O(拼接文本)×单元数
        // 的二次方行为，增量只扫当前单元）
        // 数量上限在 placement 点检查：此时全部递归依赖已载入完毕，
        // 总单元数确定（入口处检查会漏掉"主脚本自己"这一份）
        if self.units.len() >= MAX_IMPORT_UNITS {
            return Err(Error::type_error(
                format!("import 数量超过上限 ({})", MAX_IMPORT_UNITS),
                Span::synthetic(),
            ));
        }
        self.combined.push_str(&src);
        if !self.combined.ends_with('\n') {
            self.combined.push('\n');
        }
        self.line_count += src.matches('\n').count() + usize::from(!src.ends_with('\n'));
        self.units.push(ImportUnit {
            path,
            src,
            program,
            is_main,
            is_file,
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
    ) -> Result<(String, String, Option<PathBuf>, bool)> {
        Self::validate_import_name(name, span)?;

        // 自定义解析器优先（assets / DB / 加密等特殊部署）
        if let Some(res) = &self.engine.import_resolver {
            let src = res(name).map_err(|e| {
                Error::type_error(
                    format!("import \"{}\" 解析失败: {}", name, e.message),
                    span,
                )
            })?;
            return Ok((name.to_string(), src, None, false));
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
        let path = dir.join(format!("{name}.bk"));
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

        // 逃逸防护：解析后的真实路径必须落在查找目录之内——防符号链接 /
        // junction 把 import 引到脚本目录树外（与 so_demo fs 护栏同一哲学）
        let canon_dir = std::fs::canonicalize(&dir).map_err(|e| {
            Error::io(format!("import 目录不可访问 {}: {}", dir.display(), e))
        })?;
        let canon_path = std::fs::canonicalize(&path).map_err(|e| {
            Error::io(format!("import \"{}\": {}", path.display(), e))
        })?;
        if !canon_path.starts_with(&canon_dir) {
            return Err(Error::type_error(
                format!(
                    "import \"{}\" 逃出脚本目录树 (解析到 {})",
                    name,
                    canon_path.display()
                ),
                span,
            ));
        }
        // 返回 canon_path：load_unit 的 key 与依赖清单都直接用它，省掉
        // 第二次 canonicalize；deps_fresh 按真实路径复核也更稳。
        Ok((
            canon_path.display().to_string(),
            src,
            canon_path.parent().map(|p| p.to_path_buf()),
            true,
        ))
    }

    /// import 名字的硬性规则：相对路径、'/' 分隔、禁 '.'/'..'、禁盘符、
    /// 禁控制字符、禁 Windows 保留设备名、禁组件尾点/尾空格（Win32 会
    /// 剥除尾部点/空格产生别名路径）。这是引擎义务（内置查找的安全护栏），
    /// 宿主 resolver 可自行加码。
    fn validate_import_name(name: &str, span: Span) -> Result<()> {
        if name.is_empty() || name.len() > 255 {
            return Err(Error::type_error("import 路径长度非法", span));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err(Error::type_error("import 路径包含控制字符", span));
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
            if comp.ends_with('.') || comp.ends_with(' ') {
                return Err(Error::type_error(
                    format!("import 路径组件不能以点或空格结尾: \"{name}\""),
                    span,
                ));
            }
            // Windows 保留设备名（任意扩展名都命中设备，如 NUL.vt）
            let stem = comp.split('.').next().unwrap_or(comp);
            if matches!(
                stem.to_ascii_uppercase().as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "COM0" | "COM1" | "COM2" | "COM3" | "COM4"
                | "COM5" | "COM6" | "COM7" | "COM8" | "COM9" | "LPT0" | "LPT1" | "LPT2"
                | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
            ) {
                return Err(Error::type_error(
                    format!("import 路径包含 Windows 保留设备名: \"{name}\""),
                    span,
                ));
            }
        }
        Ok(())
    }

    /// 依赖时效校验：文件来源依赖逐个读文件、重算 hash 对比（任何一个
    /// 读不到或内容变了 → 缓存失效，重建会重新走 import 解析并如实报错）。
    /// resolver 来源依赖无路径可校验——失效判定由宿主 resolver 自行决定，
    /// 校验跳过（诚实限制：resolver 内容变更不会触发缓存重建）。
    ///
    /// 包含性收敛（D-046 3.7）：依赖路径存的是 resolve 后的真实路径（可
    /// 以是绝对路径），恶意构造的 .sbc 借它做任意路径探测的口子在
    /// `cache::decode` 只拦了 `..`/控制字符——这里用 import_dir 重做与
    /// 编译期 `resolve` 同款的 canonicalize 包含检查，探测被收敛到宿主
    /// 声明的目录树内。import_dir 未配置却有文件依赖的缓存一律判失效
    /// （合法路径不可能产生这种组合：无目录时 import 编译直接报错）。
    fn deps_fresh(deps: &[(String, u64, bool)], import_dir: Option<&Path>) -> bool {
        let canon_dir = import_dir.and_then(|d| std::fs::canonicalize(d).ok());
        deps.iter().all(|(path, hash, is_file)| {
            if !*is_file {
                return true;
            }
            match &canon_dir {
                Some(cd) => match std::fs::canonicalize(path) {
                    Ok(cp) if cp.starts_with(cd) => {}
                    _ => return false,
                },
                None => return false,
            }
            std::fs::read_to_string(path)
                .map(|text| cache::hash_source(&text) == *hash)
                .unwrap_or(false)
        })
    }
}
