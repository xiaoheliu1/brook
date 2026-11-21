//! 安全审查（D-046，外部交叉审查发现的问题）的回归锚。
//!
//! 每一项都对应一个曾经真实存在的洞：
//! 1. 恶意 `.sbc` 让 `params.len() > nregs` → `Vm::call` unchecked 越界写（3.1）
//! 2. `Vm` 复用（pub safe API）→ 陈旧帧恢复 → OOB 写 / 结果错乱（3.2）
//! 3. 类型文法无深度上限 → 解析期栈溢出 abort（3.3，parser 侧）
//! 4. `.sbc` 里的深嵌套类型 → 解码期栈溢出 abort（3.3，cache 侧）
//! 5. 热重载漏更新 deps → 缓存依据错误依赖集服役旧码（5.1）
//! 6. `substr` 的 `start + len` 加法在 32 位目标溢出 → panic（3.10，行为锚）

use brook::cache;
use brook::engine::HostRegistry;
use brook::ir::{Function, Instr, Op, Program};
use brook::ty::Ty;
use brook::value::Value;
use brook::{Engine, Script};
use std::collections::HashMap;
use std::rc::Rc;

/// 1. 恶意 `.sbc`：200 个参数塞进 1 格寄存器窗口。编译产物不可能产生
/// （codegen 的 nregs 从 params 起算），必须由 validate 结构性拒绝——
/// 否则 `Vm::call` 会把实参写到窗口之外的堆内存（unchecked 写，无 panic）。
#[test]
fn hostile_sbc_rejects_params_over_register_window() {
    let code = Program {
        funcs: vec![Function {
            name: "f".into(),
            code: vec![
                Instr::new(Op::ConstUnit { dst: 0 }, brook::Span::synthetic()),
                Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic()),
            ],
            nregs: 1,
            params: vec![Ty::I64; 200],
            ret: Ty::I64,
        }],
        ids: HashMap::from([("f".to_string(), 0u16)]),
        strings: vec![],
    };
    let script = Script {
        sigs: HashMap::from([(
            "f".to_string(),
            brook::typecheck::FuncSig { params: vec![Ty::I64; 200], ret: Ty::I64 },
        )]),
        code,
        name: "hostile".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
    };
    let fp: cache::HostFingerprint = Vec::new();
    let bytes = cache::encode(&script, &fp);
    let engine = Engine::new();
    let err = engine.script_from_bytes(&bytes).unwrap_err();
    assert!(
        err.message.contains("params 200 > nregs 1"),
        "{}",
        err.message
    );
}

/// 2. `Vm` 是 pub 的 safe API，但错误路径刻意保留 frames、成功路径的底帧
/// 也不 pop——复用同一个 `Vm` 第二次 call 时，`Ret` 会恢复上一次调用的
/// 帧（旧 code/ip），而寄存器窗口已按新函数缩小：旧 code 的 `dst` 可能
/// 落在窗口之外（unchecked 写）。重置防护后，复用必须像新 `Vm` 一样工作。
#[test]
fn vm_reuse_after_call_is_clean() {
    // big 的寄存器窗口远大于 f 的；修复前第二次 call(f) 的 Ret 会切回
    // big 的 code 继续执行，用 f 的小窗口跑 big 的指令。
    let src = r#"
func big() {
    let a = 1  let b = 2  let c = 3  let d = 4  let e = 5
    let g = 6  let h = 7  let i = 8  let j = 9  let k = 10
    let l = 11  let m = 12  let n = 13  let o = 14  let p = 15
    a + p
}
func f() { 42 }
"#;
    let engine = Engine::new();
    let script = engine.load_str(src).unwrap();
    let host = HostRegistry::new();
    let mut vm = brook::vm::Vm::new(&script.code, &host, 1_000_000, 64, 4 << 20);
    let big = script.code.id_of("big").unwrap();
    let f = script.code.id_of("f").unwrap();
    assert_eq!(vm.call(big, vec![]).unwrap(), Value::I64(16));
    // 修复前：这一步恢复 big 的帧 → 用 f 的窗口执行 big 的指令
    //（debug build 下 debug_assert 直接崩）。
    assert_eq!(vm.call(f, vec![]).unwrap(), Value::I64(42));
    // 错误路径之后同样必须干净（错误时 frames 被刻意保留供 trace 用）。
    let err = vm.call(f, vec![Value::I64(1)]).unwrap_err();
    assert!(err.message.contains("expects"), "{}", err.message);
    assert_eq!(vm.call(f, vec![]).unwrap(), Value::I64(42));
}

/// 3. 类型文法的深度守卫：`let x: ((((...i64...)))) = 1` 的递归
/// `parse_type` 不受表达式侧 `MAX_NESTING` 保护，2 万层（约 40 KB）足以
/// 栈溢出 abort——`catch_unwind` 抓不住。这里 80 层（> 64 上限）必须
/// 得到解析错误而不是崩溃。
#[test]
fn deep_nested_type_annotation_is_rejected() {
    let depth = 80;
    let mut src = String::from("func f() { let x: ");
    for _ in 0..depth {
        src.push('(');
    }
    src.push_str("i64");
    for _ in 0..depth {
        src.push(')');
    }
    src.push_str(" = 1  x }");
    let engine = Engine::new();
    let err = engine.load_str(&src).unwrap_err();
    assert!(
        err.message.contains("nesting too deep"),
        "{}",
        err.message
    );
}

/// 4. 缓存侧同一条洞：`.sbc` 里每层嵌套类型只花 5 字节，1 MiB 能编码
/// 20 万层——`read_ty` 的递归必须被显式深度上限拦住，返回 Err 而非 abort。
#[test]
fn deep_nested_type_in_cache_is_rejected() {
    // 手工构造 100 层嵌套的 Ty（构造侧安全：100 层的 Drop 无风险）
    let mut ty = Ty::I64;
    for _ in 0..100 {
        ty = Ty::Tuple(vec![ty]);
    }
    let code = Program {
        funcs: vec![Function {
            name: "f".into(),
            code: vec![
                Instr::new(Op::ConstUnit { dst: 0 }, brook::Span::synthetic()),
                Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic()),
            ],
            nregs: 1,
            params: vec![],
            ret: ty.clone(),
        }],
        ids: HashMap::from([("f".to_string(), 0u16)]),
        strings: vec![],
    };
    let script = Script {
        sigs: HashMap::from([(
            "f".to_string(),
            brook::typecheck::FuncSig { params: vec![], ret: ty },
        )]),
        code,
        name: "deep".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
    };
    let fp: cache::HostFingerprint = Vec::new();
    let bytes = cache::encode(&script, &fp);
    let engine = Engine::new();
    let err = engine.script_from_bytes(&bytes).unwrap_err();
    assert!(
        err.message.contains("type nesting too deep"),
        "{}",
        err.message
    );
}

/// 5. 热重载必须同步更新依赖清单：漏掉时 save_sbc 会把【旧 deps】写进
/// 缓存，compile_cached 随后依据错误路径/哈希判定"新鲜"而服役旧字节码
/// （典型触发：reload 前后宿主换了 import_dir）。
#[test]
fn reload_updates_dependency_list() {
    use std::path::PathBuf;
    let dir = std::env::temp_dir().join(format!("brook_reload_deps_{}", std::process::id()));
    let lib_a = dir.join("a");
    let lib_b = dir.join("b");
    std::fs::create_dir_all(&lib_a).unwrap();
    std::fs::create_dir_all(&lib_b).unwrap();
    std::fs::write(lib_a.join("common.bk"), "func double(n) { n * 2 }").unwrap();
    std::fs::write(lib_b.join("common.bk"), "func double(n) { n * 100 }").unwrap();
    let main = dir.join("task.bk");
    std::fs::write(&main, "import \"common\"\n\nfunc main() { double(21) }").unwrap();

    let mut engine = Engine::new().set_import_dir(&lib_a);
    let mut script = engine.load_file(&main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(42));

    // 换目录后 reload：新依赖应指向 b/，旧依赖（a/）必须被替换
    engine = engine.set_import_dir(&lib_b);
    engine.reload(&mut script, &main).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::I64(2100));
    let dep_paths: Vec<&String> = script.deps.iter().map(|(p, _, _)| p).collect();
    let sep = std::path::MAIN_SEPARATOR;
    assert!(
        dep_paths.iter().any(|p| p.ends_with(&format!("b{sep}common.bk")) || p.ends_with("b/common.bk")),
        "deps should point at the new import dir: {:?}",
        dep_paths
    );
    let _ = PathBuf::from(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 6. `substr` 的巨大 start：64 位上行为必须稳定返回空串；修复前
/// `start + len` 的加法在 32 位目标（armeabi-v7a）上因 `i64::MAX as usize`
/// 截断回绕导致 slice panic。新实现用 char_indices 定位，无加法。
#[test]
fn substr_huge_start_is_empty_not_panic() {
    let engine = Engine::new();
    let script = engine
        .load_str("func main() { substr(\"ab\", 9223372036854775807, 1) }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::from("")
    );
    // 正常路径回归：字符（而非字节）语义保持
    let script = engine
        .load_str("func main() { substr(\"中文abc\", 2, 2) }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::from("ab")
    );
}
