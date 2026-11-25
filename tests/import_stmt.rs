//! 约定式 import（ADR-017）：脚本作者零配置的公共函数复用。
//!
//! 规则（脚本作者视角，见 TUTORIAL §4.7）：
//!   * `import "名字"` 只能写在文件最前（所有 func 之前）
//!   * 解析顺序：当前脚本所在目录的 `<名字>.bk`（子路径允许，`..`/绝对路径禁止）
//!   * 重复 import 幂等去重；循环 import 编译报错；撞名编译报错
//!   * load_str（内存源码）不支持 import —— 明确报错，或宿主配置
//!     set_import_dir / set_import_resolver
//!
//! 宿主实现细节（共享引擎、拼接视图、缓存依赖校验）见 HOST_GUIDE §12.1
//! 与 `tests/migration_so_demo.rs::cross_script_call_via_host_function`。

use brook::{Engine, Value};
use std::path::{Path, PathBuf};

struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("brook_import_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn same_directory_import_and_call() {
    let dir = TmpDir::new("same_dir");
    dir.write("common.bk", "func double(n) { n * 2 }");
    let main = dir.write(
        "task.bk",
        "import \"common\"\n\nfunc main() {\n    let r = double(21)\n    log_helper(r)\n}\n\nfunc log_helper(v) { v }",
    );

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );
}

#[test]
fn subdirectory_import_works() {
    let dir = TmpDir::new("subdir");
    dir.write("lib/net.bk", "func ping() { 1 }");
    let main = dir.write("task.bk", "import \"lib/net\"\n\nfunc main() { ping() }");

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(1));
}

#[test]
fn imports_are_order_independent_and_idempotent() {
    let dir = TmpDir::new("order");
    dir.write("a.bk", "func fa() { 1 }");
    dir.write("b.bk", "func fb() { 2 }");
    // 两个 import 顺序互换、重复 import 同一文件——都应正常编译
    let main = dir.write(
        "task.bk",
        "import \"b\"\nimport \"a\"\nimport \"a\"\n\nfunc main() { fa() + fb() }",
    );

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(3));
}

#[test]
fn imports_are_relative_to_the_importing_script() {
    let dir = TmpDir::new("relative");
    // lib/net.bk 自己再 import 同目录（lib/）下的 codec.bk —— 相对导入者解析
    dir.write("lib/codec.bk", "func enc(n) { n + 1 }");
    dir.write("lib/net.bk", "import \"codec\"\n\nfunc ping2() { enc(41) }");
    let main = dir.write("task.bk", "import \"lib/net\"\n\nfunc main() { ping2() }");

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );
}

#[test]
fn cycle_imports_are_compile_errors() {
    let dir = TmpDir::new("cycle");
    dir.write("a.bk", "import \"b\"\n\nfunc fa() { 1 }");
    dir.write("b.bk", "import \"a\"\n\nfunc fb() { 2 }");
    let main = dir.write("task.bk", "import \"a\"\n\nfunc main() { fa() }");

    let engine = Engine::new();
    let err = engine.load_file(&main).unwrap_err();
    assert!(err.message.contains("import 循环"), "{}", err.message);
}

#[test]
fn name_collision_with_library_is_a_compile_error() {
    let dir = TmpDir::new("collision");
    dir.write("common.bk", "func double(n) { n * 2 }");
    // 业务脚本定义了同名函数 → duplicate definition
    let main = dir.write(
        "task.bk",
        "import \"common\"\n\nfunc double(n) { n + 1 }\n\nfunc main() { double(1) }",
    );

    let engine = Engine::new();
    let err = engine.load_file(&main).unwrap_err();
    assert!(
        err.message.contains("duplicate definition"),
        "{}",
        err.message
    );
}

#[test]
fn path_guards_reject_escape_attempts() {
    let dir = TmpDir::new("guards");
    dir.write("task.bk", "import \"../escape\"\n\nfunc main() { 1 }");

    let engine = Engine::new();
    // .. 回溯被路径护栏拒绝（编译错误，不会读到目录树外的文件）
    let err = engine.load_file(dir.path().join("task.bk")).unwrap_err();
    assert!(err.message.contains("不允许 '.' / '..'"), "{}", err.message);

    // 绝对路径同样拒绝
    let abs = dir.write("abs.bk", "import \"/etc/passwd\"\n\nfunc main() { 1 }");
    let err = engine.load_file(&abs).unwrap_err();
    assert!(err.message.contains("相对路径"), "{}", err.message);
}

#[test]
fn load_str_with_import_reports_clearly() {
    let engine = Engine::new();
    // 内存源码没有路径，import 无法定位 → 明确报错并给替代方案
    let err = engine
        .load_str("import \"common\"\n\nfunc main() { 1 }")
        .unwrap_err();
    assert!(
        err.message.contains("set_import_dir") || err.message.contains("set_import_resolver"),
        "{}",
        err.message
    );

    // 宿主配置 import 目录后，load_str 的 import 也能解析
    let dir = TmpDir::new("load_str_dir");
    dir.write("common.bk", "func double(n) { n * 2 }");
    let engine = Engine::new().set_import_dir(dir.path());
    let script = engine
        .load_str("import \"common\"\n\nfunc main() { double(21) }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );
}

#[test]
fn set_import_resolver_overrides_file_lookup() {
    let dir = TmpDir::new("resolver");
    let engine = Engine::new().set_import_resolver(|name| {
        // 模拟 assets / DB 来源：内容不落盘
        Ok(format!("func from_{name}() {{ 7 }}"))
    });
    let script = engine
        .load_str("import \"magic\"\n\nfunc main() { from_magic() }")
        .unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(7));
    let _ = dir;
}

#[test]
fn cache_invalidates_when_library_changes() {
    let dir = TmpDir::new("cache_invalidate");
    let lib = dir.write("lib/common.bk", "func double(n) { n * 2 }");
    let engine = Engine::new().set_import_dir(dir.path().join("lib"));

    let src = "import \"common\"\n\nfunc main() { double(21) }";
    let cache = dir.0.join("task.sbc");

    let s1 = engine.compile_cached(src, "task", &cache).unwrap();
    assert_eq!(engine.call(&s1, "main", vec![]).unwrap(), Value::I64(42));

    // 改 lib 一字 → 依赖 hash 变化 → 缓存失效重建
    std::fs::write(&lib, "func double(n) { n * 3 }").unwrap();
    let s2 = engine.compile_cached(src, "task", &cache).unwrap();
    assert_eq!(engine.call(&s2, "main", vec![]).unwrap(), Value::I64(63));
}

#[test]
fn hot_reload_refreshes_imported_libraries() {
    let dir = TmpDir::new("reload_import");
    let lib = dir.write("lib/common.bk", "func double(n) { n * 2 }");
    let main = dir.write("task.bk", "import \"common\"\n\nfunc main() { double(21) }");

    let engine = Engine::new().set_import_dir(dir.path().join("lib"));
    let mut script = engine.load_file(&main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );

    std::fs::write(&lib, "func double(n) { n * 100 }").unwrap();
    engine.reload(&mut script, &main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(2100)
    );
}

#[test]
fn cached_reload_invalidates_transitive_imports_and_missing_dependencies() {
    let dir = TmpDir::new("cache_transitive");
    let codec = dir.write("lib/codec.bk", "func enc(n) { n + 1 }");
    let _net = dir.write("lib/net.bk", "import \"codec\"\n\nfunc ping2() { enc(41) }");
    let main = dir.write("task.bk", "import \"net\"\n\nfunc main() { ping2() }");

    let engine = Engine::new().set_import_dir(dir.path().join("lib"));
    let mut script = engine.load_cached(&main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );
    assert!(
        main.with_extension("sbc").exists(),
        "cache should be written"
    );

    // The changed file is imported by net.bk, not by the main script. The
    // dependency list must still include it, otherwise the stale .sbc would
    // keep serving the old answer.
    std::fs::write(&codec, "func enc(n) { n + 2 }").unwrap();
    engine.reload_cached(&mut script, &main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(43)
    );

    let fresh = engine.load_cached(&main).unwrap();
    assert_eq!(engine.call(&fresh, "main", vec![]).unwrap(), Value::I64(43));

    // A missing dependency must invalidate too; falling back to the cache here
    // would hide a broken deployment.
    std::fs::remove_file(&codec).unwrap();
    let err = engine.reload_cached(&mut script, &main).unwrap_err();
    assert!(
        err.message.contains("找不到 import 文件") || err.message.contains("codec"),
        "{}",
        err.message
    );
}

/// 缓存校验的安全加固（cache decode 硬校验）：恶意构造的 .sbc 依赖清单
/// 带 `..` 组件或坏 flag → decode 拒绝（不能用依赖校验做任意路径探测）。
/// 正常编译产物不可能产生这样的依赖清单（名字护栏编译期拦截）。
/// 依赖路径在测试里统一用正斜杠（Rust 字符串无需反斜杠转义，语义相同）。
#[test]
fn cache_decode_rejects_hostile_dependency_lists() {
    use brook::cache;
    use brook::ir::Program;
    use std::collections::HashMap;
    use std::rc::Rc;

    let fp: cache::HostFingerprint = Vec::new();
    let base = brook::Script {
        code: brook::ValidatedCode::new(
            Program {
                funcs: vec![],
                ids: HashMap::new(),
                strings: vec![],
                types: Vec::new(),
            },
            0,
        )
        .unwrap(),
        sigs: HashMap::new(),
        name: "hostile".into(),
        source_hash: 1,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
    };

    // 敌意依赖清单（`..` 组件）→ decode 拒绝
    let hostile = brook::Script {
        code: base.code.clone(),
        sigs: base.sigs.clone(),
        name: base.name.clone(),
        source_hash: base.source_hash,
        source: base.source.clone(),
        deps: vec![
            ("../../secrets.bk".into(), 1, true),
            ("..\\..\\secrets.bk".into(), 1, true),
        ],
        host_fp: Vec::new(),
    };
    let bytes = cache::encode(&hostile, &fp);
    let err = cache::decode(&bytes, &fp).unwrap_err();
    assert!(err.message.contains(".."), "{}", err.message);

    // 坏来源标记（手工翻转依赖记录的 flag 字节）→ decode 拒绝。
    // base 的 deps 为空，需要一个带【合法】依赖的脚本才能定位 flag 字节：
    // 依赖记录 = 路径 str(4+len) + hash(8) + flag(1)，flag 是 deps 段
    // 最后一字节。翻转后 flag = 0xFE > 1 被硬校验拒绝——证明依赖段的
    // 字节翻转不会被静默吞掉（D-046 3.7 加固后依赖段不再是文件末尾，
    // 末尾是 v6 的源码段）。
    let benign = brook::Script {
        code: base.code.clone(),
        sigs: base.sigs.clone(),
        name: base.name.clone(),
        source_hash: base.source_hash,
        source: base.source.clone(),
        deps: vec![("lib/common.bk".into(), 1, true)],
        host_fp: Vec::new(),
    };
    let mut bytes = cache::encode(&benign, &fp);
    // 源码段（4 字节长度头 + len 字节）在 deps 之后；flag 是 deps 段末字节
    let last = bytes.len() - (4 + benign.source.len()) - 1;
    bytes[last] = 0xFE;
    let err = cache::decode(&bytes, &fp).unwrap_err();
    assert!(
        err.message.contains("dependency") && err.message.contains("flag"),
        "{}",
        err.message
    );

    // 末尾（源码段）字节翻转同样不被静默吞掉
    let mut bytes = cache::encode(&base, &fp);
    let n = bytes.len();
    bytes[n - 1] = 0xFE;
    let err = cache::decode(&bytes, &fp).unwrap_err();
    assert!(
        err.message.contains("truncated") || err.message.contains("UTF-8"),
        "{}",
        err.message
    );
}

// ---- 编译快路径（D-048）-----------------------------------------------
// `compile_inner` 现在对不含 "import" 子串的源码跳过第一遍 lex+parse，
// 并且只对【主文件脚本】做 canonicalize。下面两条锁定这两个前提的
// 语义边界：子串误判不能变成真 import，虚拟名不需要是合法路径。

#[test]
fn keyword_in_literal_or_comment_does_not_trigger_import_resolution() {
    // 没有配置 import_dir / resolver：如果文本扫描的过近似被误当成真
    // import，这里会在解析期报 "import 需要宿主配置"。
    let engine = Engine::new();
    for src in [
        "func main() { \"import\" }",
        "// import \"ghost\"\nfunc main() { 7 }",
        "func important() { 3 }\nfunc main() { important() }",
    ] {
        let script = engine.load_str(src).unwrap();
        assert!(
            script.deps.is_empty(),
            "no dependency should be loaded: {src}"
        );
    }
    assert_eq!(
        engine
            .call(
                &engine.load_str("func main() { \"import\" }").unwrap(),
                "main",
                vec![]
            )
            .unwrap(),
        Value::Str(std::rc::Rc::from("import"))
    );
}

#[test]
fn virtual_names_work_without_touching_the_filesystem() {
    let engine = Engine::new().set_import_resolver(|name| {
        Ok(format!(
            "func from_lib() {{ {} }}",
            if name.contains('/') { 9 } else { 5 }
        ))
    });
    // '/' 名字在 Windows 上是非法的单文件名；作为虚拟 key 必须照常工作
    // （旧实现会先 canonicalize，失败后回退；这里验证不依赖那条回退）。
    let script = engine
        .compile(
            "import \"pkg/lib\"\nfunc main() { from_lib() }",
            "<virtual:main>",
        )
        .unwrap();
    assert_eq!(script.deps.len(), 1);
    assert!(!script.deps[0].2, "resolver deps are not file-backed");
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(9));

    // 无 import 的虚拟名同样正常，且依赖清单为空
    let plain = engine
        .compile("func main() { 1 }", "<virtual:plain>")
        .unwrap();
    assert!(plain.deps.is_empty());
    assert_eq!(engine.call(&plain, "main", vec![]).unwrap(), Value::I64(1));
}
