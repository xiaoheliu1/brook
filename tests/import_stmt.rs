//! 约定式 import（ADR-017）：脚本作者零配置的公共函数复用。
//!
//! 规则（脚本作者视角，见 TUTORIAL §4.7）：
//!   * `import "名字"` 只能写在文件最前（所有 func 之前）
//!   * 解析顺序：当前脚本所在目录的 `<名字>.vt`（子路径允许，`..`/绝对路径禁止）
//!   * 重复 import 幂等去重；循环 import 编译报错；撞名编译报错
//!   * load_str（内存源码）不支持 import —— 明确报错，或宿主配置
//!     set_import_dir / set_import_resolver
//!
//! 宿主实现细节（共享引擎、拼接视图、缓存依赖校验）见 HOST_GUIDE §12.1
//! 与 `tests/migration_so_demo.rs::cross_script_call_via_host_function`。

use std::path::{Path, PathBuf};
use vitul::{Engine, Value};

struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vitul_import_{}_{}", std::process::id(), tag));
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
    dir.write("common.vt", "func double(n) { n * 2 }");
    let main = dir.write(
        "task.vt",
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
    dir.write("lib/net.vt", "func ping() { 1 }");
    let main = dir.write(
        "task.vt",
        "import \"lib/net\"\n\nfunc main() { ping() }",
    );

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(1));
}

#[test]
fn imports_are_order_independent_and_idempotent() {
    let dir = TmpDir::new("order");
    dir.write("a.vt", "func fa() { 1 }");
    dir.write("b.vt", "func fb() { 2 }");
    // 两个 import 顺序互换、重复 import 同一文件——都应正常编译
    let main = dir.write(
        "task.vt",
        "import \"b\"\nimport \"a\"\nimport \"a\"\n\nfunc main() { fa() + fb() }",
    );

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(3));
}

#[test]
fn imports_are_relative_to_the_importing_script() {
    let dir = TmpDir::new("relative");
    // lib/net.vt 自己再 import 同目录（lib/）下的 codec.vt —— 相对导入者解析
    dir.write("lib/codec.vt", "func enc(n) { n + 1 }");
    dir.write("lib/net.vt", "import \"codec\"\n\nfunc ping2() { enc(41) }");
    let main = dir.write(
        "task.vt",
        "import \"lib/net\"\n\nfunc main() { ping2() }",
    );

    let engine = Engine::new();
    let script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(42));
}

#[test]
fn cycle_imports_are_compile_errors() {
    let dir = TmpDir::new("cycle");
    dir.write("a.vt", "import \"b\"\n\nfunc fa() { 1 }");
    dir.write("b.vt", "import \"a\"\n\nfunc fb() { 2 }");
    let main = dir.write("task.vt", "import \"a\"\n\nfunc main() { fa() }");

    let engine = Engine::new();
    let err = engine.load_file(&main).unwrap_err();
    assert!(err.message.contains("import 循环"), "{}", err.message);
}

#[test]
fn name_collision_with_library_is_a_compile_error() {
    let dir = TmpDir::new("collision");
    dir.write("common.vt", "func double(n) { n * 2 }");
    // 业务脚本定义了同名函数 → duplicate definition
    let main = dir.write(
        "task.vt",
        "import \"common\"\n\nfunc double(n) { n + 1 }\n\nfunc main() { double(1) }",
    );

    let engine = Engine::new();
    let err = engine.load_file(&main).unwrap_err();
    assert!(err.message.contains("duplicate definition"), "{}", err.message);
}

#[test]
fn path_guards_reject_escape_attempts() {
    let dir = TmpDir::new("guards");
    dir.write("task.vt", "import \"../escape\"\n\nfunc main() { 1 }");

    let engine = Engine::new();
    // .. 回溯被路径护栏拒绝（编译错误，不会读到目录树外的文件）
    let err = engine.load_file(dir.path().join("task.vt")).unwrap_err();
    assert!(
        err.message.contains("不允许 '.' / '..'"),
        "{}",
        err.message
    );

    // 绝对路径同样拒绝
    let abs = dir
        .write("abs.vt", "import \"/etc/passwd\"\n\nfunc main() { 1 }");
    let err = engine.load_file(&abs).unwrap_err();
    assert!(
        err.message.contains("相对路径"),
        "{}",
        err.message
    );
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
    dir.write("common.vt", "func double(n) { n * 2 }");
    let engine = Engine::new().set_import_dir(dir.path());
    let script = engine
        .load_str("import \"common\"\n\nfunc main() { double(21) }")
        .unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(42));
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
    let lib = dir.write("lib/common.vt", "func double(n) { n * 2 }");
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
    let lib = dir.write("lib/common.vt", "func double(n) { n * 2 }");
    let main = dir.write(
        "task.vt",
        "import \"common\"\n\nfunc main() { double(21) }",
    );

    let engine = Engine::new().set_import_dir(dir.path().join("lib"));
    let mut script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(42));

    std::fs::write(&lib, "func double(n) { n * 100 }").unwrap();
    engine.reload(&mut script, &main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(2100));
}
