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
use brook::{register_fn, Engine, Script};
use std::collections::HashMap;
use std::rc::Rc;

/// 1. 恶意 `.sbc`：200 个参数塞进 1 格寄存器窗口。编译产物不可能产生
///    （codegen 的 nregs 从 params 起算），必须由 validate 结构性拒绝——
///    否则 `Vm::call` 会把实参写到窗口之外的堆内存（unchecked 写，无 panic）。
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
        types: Vec::new(),
    };
    let script = Script {
        sigs: HashMap::from([(
            "f".to_string(),
            brook::typecheck::FuncSig {
                params: vec![Ty::I64; 200],
                ret: Ty::I64,
            },
        )]),
        code,
        name: "hostile".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
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
///    也不 pop——复用同一个 `Vm` 第二次 call 时，`Ret` 会恢复上一次调用的
///    帧（旧 code/ip），而寄存器窗口已按新函数缩小：旧 code 的 `dst` 可能
///    落在窗口之外（unchecked 写）。重置防护后，复用必须像新 `Vm` 一样工作。
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
///    `parse_type` 不受表达式侧 `MAX_NESTING` 保护，2 万层（约 40 KB）足以
///    栈溢出 abort——`catch_unwind` 抓不住。这里 80 层（> 64 上限）必须
///    得到解析错误而不是崩溃。
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
    assert!(err.message.contains("nesting too deep"), "{}", err.message);
}

/// 4. 缓存侧同一条洞：`.sbc` 里每层嵌套类型只花 5 字节，1 MiB 能编码
///    20 万层——`read_ty` 的递归必须被显式深度上限拦住，返回 Err 而非 abort。
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
        types: Vec::new(),
    };
    let script = Script {
        sigs: HashMap::from([(
            "f".to_string(),
            brook::typecheck::FuncSig {
                params: vec![],
                ret: ty,
            },
        )]),
        code,
        name: "deep".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
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
///    缓存，compile_cached 随后依据错误路径/哈希判定"新鲜"而服役旧字节码
///    （典型触发：reload 前后宿主换了 import_dir）。
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
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(42)
    );

    // 换目录后 reload：新依赖应指向 b/，旧依赖（a/）必须被替换
    engine = engine.set_import_dir(&lib_b);
    engine.reload(&mut script, &main).unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(2100)
    );
    let dep_paths: Vec<&String> = script.deps.iter().map(|(p, _, _)| p).collect();
    let sep = std::path::MAIN_SEPARATOR;
    assert!(
        dep_paths
            .iter()
            .any(|p| p.ends_with(&format!("b{sep}common.bk")) || p.ends_with("b/common.bk")),
        "deps should point at the new import dir: {:?}",
        dep_paths
    );
    let _ = PathBuf::from(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 6. `substr` 的巨大 start：64 位上行为必须稳定返回空串；修复前
///    `start + len` 的加法在 32 位目标（armeabi-v7a）上因 `i64::MAX as usize`
///    截断回绕导致 slice panic。新实现用 char_indices 定位，无加法。
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

// ===========================================================================
// 第二批（D-046）：gas 计费、配额盲区、decode 语义校验、指纹绑定、
// deps 收敛、循环变量、缓存源码段
// ===========================================================================

fn encode_program(
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    code: Vec<Instr>,
    nregs: u16,
    sig_ret: Ty,
) -> Vec<u8> {
    let prog = Program {
        funcs: vec![Function {
            name: name.into(),
            code,
            nregs,
            params: params.clone(),
            ret: ret.clone(),
        }],
        ids: HashMap::from([(name.to_string(), 0u16)]),
        strings: vec![],
        types: Vec::new(),
    };
    let script = Script {
        sigs: HashMap::from([(
            name.to_string(),
            brook::typecheck::FuncSig {
                params: params.clone(),
                ret: sig_ret,
            },
        )]),
        code: prog,
        name: name.into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
    };
    let fp: cache::HostFingerprint = Vec::new();
    cache::encode(&script, &fp)
}

/// 5.7-1：解码签名含推断变量 → 拒绝。`type_matches` 的
///    `(_, Ty::Var(_)) => true` 会放行任意值，函数体按 I64 位解释堆索引。
#[test]
fn hostile_sbc_rejects_inference_variable_in_signature() {
    let bytes = encode_program(
        "f",
        vec![],
        Ty::Var(0),
        vec![
            Instr::new(Op::ConstUnit { dst: 0 }, brook::Span::synthetic()),
            Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic()),
        ],
        1,
        Ty::Var(0),
    );
    let err = Engine::new().script_from_bytes(&bytes).unwrap_err();
    assert!(
        err.message.contains("inference variable"),
        "{}",
        err.message
    );
}

/// 5.7-2：sigs 与 funcs 的签名不一致 → 拒绝（coerce 与位解释各用一份）。
#[test]
fn hostile_sbc_rejects_signature_mismatch() {
    let bytes = encode_program(
        "f",
        vec![Ty::I64],
        Ty::I64,
        vec![
            Instr::new(Op::ConstI64 { dst: 0, val: 1 }, brook::Span::synthetic()),
            Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic()),
        ],
        1,
        Ty::Str, // sigs 说返回 str，funcs 说 i64
    );
    let err = Engine::new().script_from_bytes(&bytes).unwrap_err();
    assert!(
        err.message.contains("signature mismatch"),
        "{}",
        err.message
    );
}

/// 5.7-3：ids 非双射 → 拒绝（入口名映射到任意函数会静默调错代码）。
#[test]
fn hostile_sbc_rejects_id_table_mismatch() {
    let prog = Program {
        funcs: vec![
            Function {
                name: "f".into(),
                code: vec![Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic())],
                nregs: 1,
                params: vec![],
                ret: Ty::Unit,
            },
            Function {
                name: "g".into(),
                code: vec![Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic())],
                nregs: 1,
                params: vec![],
                ret: Ty::Unit,
            },
        ],
        ids: HashMap::from([("f".to_string(), 0u16)]),
        strings: vec![],
        types: Vec::new(),
    };
    let script = Script {
        sigs: HashMap::new(),
        code: prog,
        name: "ids".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
    };
    let fp: cache::HostFingerprint = Vec::new();
    let bytes = cache::encode(&script, &fp);
    let err = Engine::new().script_from_bytes(&bytes).unwrap_err();
    assert!(err.message.contains("id table"), "{}", err.message);
}

/// 5.3：堆元组与签名元数不一致 → 运行时明确报错（旧实现 zip 截断静默丢
///    元素，且与参考解释器分歧）。
#[test]
fn hostile_sbc_reports_tuple_arity_mismatch() {
    let bytes = encode_program(
        "f",
        vec![],
        Ty::Tuple(vec![Ty::I64]),
        vec![
            Instr::new(Op::ConstI64 { dst: 0, val: 1 }, brook::Span::synthetic()),
            Instr::new(Op::ConstI64 { dst: 1, val: 2 }, brook::Span::synthetic()),
            Instr::new(Op::ConstI64 { dst: 2, val: 3 }, brook::Span::synthetic()),
            Instr::new(
                Op::MakeTuple {
                    dst: 3,
                    elems: brook::ir::Regs::new(&[0, 1, 2]),
                },
                brook::Span::synthetic(),
            ),
            Instr::new(Op::Ret { src: 3 }, brook::Span::synthetic()),
        ],
        4,
        Ty::Tuple(vec![Ty::I64]),
    );
    let engine = Engine::new();
    let script = engine.script_from_bytes(&bytes).unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert!(
        err.message.contains("tuple arity mismatch") && err.message.contains("value has 3"),
        "{}",
        err.message
    );
}

/// 3.8：内存里的 Script 绑定编译时宿主指纹——拿到别的 Engine 上调用
///    明确报错，而不是按槽位 id 静默调错宿主函数。
#[test]
fn script_rejects_foreign_engine_registry() {
    let mut e1 = Engine::new();
    register_fn!(e1, "read_file", (i64) -> i64, |n| n).unwrap();
    let script = e1.load_str("func f() { read_file(1) }").unwrap();
    assert_eq!(e1.call(&script, "f", vec![]).unwrap(), Value::I64(1));

    let mut e2 = Engine::new();
    register_fn!(e2, "delete_all", (i64) -> i64, |n| n * 100).unwrap();
    let err = e2.call(&script, "f", vec![]).unwrap_err();
    assert!(
        err.message.contains("different host registry"),
        "{}",
        err.message
    );
}

/// 3.4：gas 与 O(n) 内建挂钩——`len(64KB 串)` 百万轮在默认 10M 步预算内
///    必须被拦（修复前每轮 1 步，实际扫描 6.4e10 字符，宿主挂死数小时）。
#[test]
fn gas_charges_string_work() {
    let mut src = String::from("func main() {\n    let s = \"");
    src.push_str(&"x".repeat(64 * 1024));
    src.push_str("\"\n    let t = 0\n    for i in 0..1000000 { t = t + len(s) }\n    t\n}");
    let engine = Engine::new();
    let script = engine.load_str(&src).unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    assert!(
        err.message.contains("gas") || err.message.contains("step"),
        "{}",
        err.message
    );
}

/// 3.5b：常量池计入内存配额——修复前 2 MiB 字面量的配额消耗为 0。
#[test]
fn constant_pool_counts_against_heap_quota() {
    let mut src = String::from("func main() { return \"");
    src.push_str(&"x".repeat(2 * 1024 * 1024));
    src.push_str("\"\n}");
    let engine = Engine::new().with_heap_limit(1024 * 1024);
    let script = engine.load_str(&src).unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    assert!(
        err.message.contains("memory limit exceeded"),
        "{}",
        err.message
    );
}

/// 5.9：循环变量不可赋值（旧实现放行 → 每轮重置 → 死循环靠 gas 兜底）；
///    内层 `let` 遮蔽后的同名变量是合法赋值目标。
#[test]
fn loop_variable_assignment_is_rejected() {
    let engine = Engine::new();
    let err = engine
        .load_str("func main() { for i in 0..10 { i = 5 } }")
        .unwrap_err();
    assert!(
        err.message.contains("cannot assign to loop variable"),
        "{}",
        err.message
    );
    // 数组 for-in 的循环变量同样受保护
    let err = engine
        .load_str("func main() { let xs = [1]  for x in xs { x = 2 } }")
        .unwrap_err();
    assert!(
        err.message.contains("cannot assign to loop variable"),
        "{}",
        err.message
    );
    // 遮蔽：循环体内 let 出新绑定 → 赋值合法
    let ok = engine
        .load_str("func main() { let t = 0  for i in 0..3 { let i = 5  t = t + i }  t }")
        .unwrap();
    assert_eq!(engine.call(&ok, "main", vec![]).unwrap(), Value::I64(15));
}

/// 3.7：deps_fresh 的包含性收敛——缓存里的依赖路径指向 import_dir 之外
///    时，缓存必须判失效（重建走 resolve 护栏），宿主目录树外的诱饵文件
///    不再能借缓存校验路径被读取。
#[test]
fn cache_deps_outside_import_dir_invalidate() {
    use std::path::PathBuf;
    let dir = std::env::temp_dir().join(format!("brook_deps_scope_{}", std::process::id()));
    let lib_dir = dir.join("libs");
    let outside_dir = dir.join("outside");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(lib_dir.join("common.bk"), "func double(n) { n * 2 }").unwrap();
    std::fs::write(outside_dir.join("common.bk"), "func double(n) { n * 3 }").unwrap();
    let src = "import \"common\"\n\nfunc main() { double(21) }";
    let cache_path = dir.join("task.sbc");

    let engine = Engine::new().set_import_dir(&lib_dir);
    // 正常编译 + 缓存（deps 指向 libs/common.bk，hash = n*2 版本）
    let s = engine.compile_cached(src, "task", &cache_path).unwrap();
    assert_eq!(engine.call(&s, "main", vec![]).unwrap(), Value::I64(42));

    // 手工构造一个"指向树外诱饵"的缓存：主源码 hash 正确、deps 的路径
    // 与 hash 都是诱饵文件的——修复前 deps_fresh 按路径读诱饵、hash 命中
    // → 缓存命中 → 调用 n*3；修复后包含性检查失败 → 判失效 → 重建走
    // resolve（找到 libs/ 里的真 lib）→ n*2。
    let decoy_hash =
        brook::cache::hash_source(&std::fs::read_to_string(outside_dir.join("common.bk")).unwrap());
    let prog = Program {
        funcs: vec![Function {
            name: "main".into(),
            code: vec![
                Instr::new(Op::ConstUnit { dst: 0 }, brook::Span::synthetic()),
                Instr::new(Op::Ret { src: 0 }, brook::Span::synthetic()),
            ],
            nregs: 1,
            params: vec![],
            ret: Ty::Unit,
        }],
        ids: HashMap::from([("main".to_string(), 0u16)]),
        strings: vec![],
        types: Vec::new(),
    };
    let hostile = Script {
        sigs: HashMap::new(),
        code: prog,
        name: "task".into(),
        source_hash: brook::cache::hash_source(src),
        source: Rc::from(src),
        deps: vec![(
            outside_dir.join("common.bk").display().to_string(),
            decoy_hash,
            true,
        )],
        host_fp: engine.fingerprint(),
    };
    std::fs::write(
        &cache_path,
        brook::cache::encode(&hostile, &engine.fingerprint()),
    )
    .unwrap();

    let s2 = engine.compile_cached(src, "task", &cache_path).unwrap();
    assert_eq!(
        engine.call(&s2, "main", vec![]).unwrap(),
        Value::I64(42),
        "缓存必须失效重建（树外依赖不可作为时效依据）"
    );
    let _ = PathBuf::from(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 5.2：带 import 的脚本缓存命中后，`Script::source` 必须是拼接视图
///    （v6 源码段）——report 的源码行 / caret 才对得上行号。
#[test]
fn cached_import_script_keeps_combined_source() {
    let dir = std::env::temp_dir().join(format!("brook_cached_src_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("common.bk"), "func double(n) { n * 2 }").unwrap();
    let src = "import \"common\"\n\nfunc main() { double(21) }";
    let cache_path = dir.join("task.sbc");

    let engine = Engine::new().set_import_dir(&dir);
    let s1 = engine.compile_cached(src, "task", &cache_path).unwrap();
    // 第二次 compile_cached 命中缓存（源码 hash + deps 全部新鲜）
    let s2 = engine.compile_cached(src, "task", &cache_path).unwrap();
    assert!(
        s2.source.contains("import \"common\"") && s2.source.contains("double"),
        "cached script must carry the combined view source, got: {:?}",
        &*s2.source
    );
    assert_eq!(s1.source, s2.source);
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// 第三批（D-046 续）：确定性 encode、FFI UTF-8 截断、参数上限口径
// ===========================================================================

/// 4.7：同一脚本 + 同一指纹，两次 encode 产出的字节必须完全一致——
///    HashMap 无序迭代曾让构建产物比对与缓存 diff 失效。
#[test]
fn encode_is_deterministic() {
    let mut engine = Engine::new();
    register_fn!(engine, "log", (String) -> i64, |s: String| s.len() as i64).unwrap();
    let script = engine
        .load_str("func main() { log(\"a\") + log(\"b\") }")
        .unwrap();
    let fp = engine.fingerprint();
    let b1 = brook::cache::encode(&script, &fp);
    let b2 = brook::cache::encode(&script, &fp);
    assert_eq!(b1, b2, "encode must be byte-for-byte reproducible");
}

/// 5.13：`ms_last_error` 按字节截断必须落在 UTF-8 字符边界上——C 侧拿到
///    的一定是合法 UTF-8（旧实现会在 CJK 字符中间切断）。
#[test]
fn ffi_error_truncation_is_utf8_safe() {
    use brook::ffi::ms_last_error;
    use std::ffi::CString;

    // 触发一个含 CJK 的错误消息（import 文件找不到，消息带路径与中文）
    let src = CString::new(
        "import \"不存在\"

func main() { 1 }",
    )
    .unwrap();
    let func = CString::new("main").unwrap();
    let mut out: i64 = 0;
    unsafe {
        brook::ffi::ms_call_i64(src.as_ptr(), func.as_ptr(), 1, &mut out);
        // 小缓冲（6 字节 → 至多 5 字节消息 + NUL）：中文 3 字节/字符，
        // 截断必须回退到边界
        for cap in 2..8usize {
            let mut buf = vec![b'a'; cap];
            let n = ms_last_error(buf.as_mut_ptr() as *mut _, cap);
            assert!(n < cap, "n={} cap={}", n, cap);
            buf[n] = 0;
            let s = std::str::from_utf8(&buf[..n]);
            assert!(
                s.is_ok(),
                "cap={}: truncated message is not UTF-8: {:x?}",
                cap,
                &buf[..n]
            );
        }
    }
}

/// 5.13：函数参数上限与 MAX_OPERANDS 同口径——入口参数 >8 在定义处
///    一并报错（旧实现定义能过、脚本内部调用必挂的不对称）。
#[test]
fn function_params_capped_at_max_operands() {
    let engine = Engine::new();
    let err = engine
        .load_str("func f(a, b, c, d, e, g, h, i, j) { a }")
        .unwrap_err();
    assert!(err.message.contains("has 9 parameters"), "{}", err.message);
    // 8 个参数合法
    let ok = engine
        .load_str("func f(a, b, c, d, e, g, h, i) { a + i }")
        .unwrap();
    assert_eq!(
        engine.call(&ok, "f", vec![Value::I64(1); 8]).unwrap(),
        Value::I64(2)
    );
}
