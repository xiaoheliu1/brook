//! 数组（ADR-018）：字面量 / 下标 / for-in / push / len / contains，
//! Python 式引用语义，元素推断与标注，宿主 `Vec<T>` 边界，配额与缓存。
//!
//! 语义双引擎对齐：标注 `#[cfg(not(feature = "vm_only"))]` 风格不适用——
//! 本文件统一用 `run_vm` / `run_eval` 两个入口跑同一断言，防止参考解释器
//! 与 VM 再次分叉（见 tests/eval_parity.rs 的教训）。

use brook::{register_fn, Engine, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn arr(items: Vec<Value>) -> Value {
    Value::arr(items)
}

/// VM 路径：编译并调用。
fn call_vm(src: &str, func: &str, args: Vec<Value>) -> Result<Value, brook::Error> {
    let engine = Engine::new();
    let script = engine.load_str(src)?;
    engine.call(&script, func, args)
}

/// 参考解释器路径：同一语义的第二个引擎（管线与 tests/eval_parity.rs 一致）。
fn call_eval(src: &str, func: &str, args: Vec<Value>) -> Result<Value, brook::Error> {
    use brook::engine::HostRegistry;
    use brook::eval::Interpreter;
    use brook::lexer::Lexer;
    use brook::parser::Parser;
    use brook::typecheck::Checker;
    use std::collections::HashMap;
    let host = HostRegistry::new();
    let tokens = Lexer::new(src).tokenize()?;
    let program = Parser::new(tokens).parse()?;
    let sigs = Checker::new(&host.signatures()).check(&program)?;
    let funcs: HashMap<String, brook::ast::FuncDecl> = program
        .funcs
        .into_iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let mut it = Interpreter::new(&funcs, &host, &sigs, 1_000_000, 64);
    it.call(func, args)
}

/// 两个引擎必须给出同一结果。
macro_rules! assert_both {
    ($src:expr, $func:expr, $args:expr, $want:expr) => {{
        let want = $want;
        assert_eq!(call_vm($src, $func, $args.clone()).unwrap(), want, "vm: {}", $src);
        assert_eq!(call_eval($src, $func, $args.clone()).unwrap(), want, "eval: {}", $src);
    }};
}

// ===========================================================================
// 基础：字面量 / 下标 / len / for-in
// ===========================================================================

#[test]
fn literal_index_len_and_for_in() {
    assert_both!(
        "func main() { let xs = [10, 20, 30]  xs[1] + len(xs) }",
        "main",
        vec![],
        Value::I64(23)
    );
    // for-in 累加：for x in xs
    assert_both!(
        "func main() { let xs = [1, 2, 3, 4]  let t = 0  for x in xs { t = t + x }  t }",
        "main",
        vec![],
        Value::I64(10)
    );
    // 空数组字面量 + 遍历零次
    assert_both!(
        "func main() { let xs: [i64] = []  let t = 0  for x in xs { t = t + 1 }  t }",
        "main",
        vec![],
        Value::I64(0)
    );
    // 字符串数组 + contains 元素查找
    assert_both!(
        r#"func main() { let xs = ["a", "b"]  contains(xs, "b") }"#,
        "main",
        vec![],
        Value::Bool(true)
    );
    assert_both!(
        r#"func main() { let xs = ["a", "b"]  contains(xs, "z") }"#,
        "main",
        vec![],
        Value::Bool(false)
    );
}

// ===========================================================================
// 引用语义（Python 式）：赋值别名、传参别名
// ===========================================================================

#[test]
fn assignment_is_aliasing() {
    // let b = a 之后 push 双方可见 —— VM 槽位拷贝堆索引 / 解释器共享 Rc，
    // 两引擎必须一致。
    assert_both!(
        "func main() { let a = [1]  let b = a  push(b, 2)  len(a) }",
        "main",
        vec![],
        Value::I64(2)
    );
}

#[test]
fn function_args_pass_by_reference() {
    // 被调函数内的 push 对调用者可见（Python 语义，ADR-018 决策项）。
    assert_both!(
        r#"
func add_fails(fails) { push(fails, "later")  return () }
func main() {
    let fails = ["early"]
    add_fails(fails)
    len(fails)
}
"#,
        "main",
        vec![],
        Value::I64(2)
    );
}

#[test]
fn push_accumulates_and_infers_element_type() {
    // `let fails = []` + `push(fails, "x")` → 元素定型 [str]（ADR-018 推断规则）
    assert_both!(
        r#"
func main() {
    let fails = []
    for i in 0..3 { push(fails, "e" + to_str(i)) }
    fails[2]
}
"#,
        "main",
        vec![],
        Value::from("e2")
    );
}

// ===========================================================================
// 元组数组：宿主记录列表的载体（fs_list 模式）
// ===========================================================================

#[test]
fn array_of_tuples_with_field_access() {
    assert_both!(
        r#"
func main() {
    let rows = [("a.txt", "a", true, 3), ("b.txt", "b", false, 5)]
    let t = 0
    for r in rows {
        if r.2 { t = t + r.3 }
    }
    t
}
"#,
        "main",
        vec![],
        Value::I64(3)
    );
}

// ===========================================================================
// 嵌套数组
// ===========================================================================

#[test]
fn nested_arrays() {
    assert_both!(
        "func main() { let g = [[1, 2], [3, 4]]  g[1][0] }",
        "main",
        vec![],
        Value::I64(3)
    );
}

// ===========================================================================
// 运行时错误：越界（不 panic、带定位）
// ===========================================================================

#[test]
fn out_of_range_is_a_clean_error() {
    for runner in [call_vm, call_eval] {
        let err = runner(
            "func main() { let xs = [1, 2]  xs[5] }",
            "main",
            vec![],
        )
        .unwrap_err();
        assert!(
            err.message.contains("array index 5 out of range"),
            "{}",
            err.message
        );
        let err = runner(
            "func main() { let xs = [1, 2]  xs[0 - 1] }",
            "main",
            vec![],
        )
        .unwrap_err();
        assert!(err.message.contains("out of range"), "{}", err.message);
    }
}

// ===========================================================================
// 编译期类型错误
// ===========================================================================

#[test]
fn type_errors_are_rejected_at_compile() {
    let engine = Engine::new();
    // 异构字面量
    let err = engine
        .load_str("func main() { let xs = [1, \"a\"] }")
        .unwrap_err();
    assert!(err.message.contains("array literal"), "{}", err.message);
    // push 到非数组
    let err = engine.load_str("func main() { push(1, 2) }").unwrap_err();
    assert!(err.message.contains("push expects an array"), "{}", err.message);
    // 数组相等不支持（与元组同策略）
    let err = engine
        .load_str("func main() { let a = [1]  let b = [1]  a == b }")
        .unwrap_err();
    assert!(
        err.message.contains("equality comparison is not supported"),
        "{}",
        err.message
    );
    // push 元素类型不匹配
    let err = engine
        .load_str("func main() { let xs = [1]  push(xs, \"a\") }")
        .unwrap_err();
    assert!(err.message.contains("type mismatch"), "{}", err.message);
    // for-in 遍历非数组
    let err = engine
        .load_str("func main() { for x in 5 { } }")
        .unwrap_err();
    assert!(err.message.contains("for-in expects"), "{}", err.message);
}

#[test]
fn tuple_indexing_syntax_gets_a_clear_hint() {
    let engine = Engine::new();
    let err = engine
        .load_str("func main() { let t = (1, 2)  t[0] }")
        .unwrap_err();
    assert!(
        err.message.contains(".0") && err.message.contains("`[i]`"),
        "{}",
        err.message
    );
}

#[test]
fn push_on_itself_is_rejected_infinite_type() {
    // push(a, a) → e = Arr(e) 是 infinite type —— 这正是「循环引用不可构造、
    // 不需要 GC」的类型系统保证（ADR-018 的核心论证）。
    let engine = Engine::new();
    let err = engine
        .load_str("func main() { let a = []  push(a, a) }")
        .unwrap_err();
    assert!(err.message.contains("infinite type"), "{}", err.message);
}

// ===========================================================================
// 配额：push 循环受 max_heap 约束（先查配额后分配）
// ===========================================================================

#[test]
fn push_loop_hits_heap_quota() {
    let engine = Engine::new()
        .with_limits(10_000_000, 64)
        .with_heap_limit(8 * 1024);
    let script = engine
        .load_str("func main() { let xs = []  for i in 0..100000 { push(xs, i) }  len(xs) }")
        .unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    assert!(err.message.contains("memory limit exceeded"), "{}", err.message);
}

// ===========================================================================
// 宿主边界：Vec<T> 返回 / 接收
// ===========================================================================

/// 宿主返回「记录列表」——迁移 so_demo 的 fs_list 场景（ADR-018 的核心动机）。
#[test]
fn host_returns_array_of_tuples() {
    let mut engine = Engine::new();
    register_fn!(engine, "fs_list", (String) -> Vec<(String, String, bool, i64)>, |dir: String| {
        vec![
            (format!("{dir}/a.txt"), "a.txt".into(), true, 3),
            ("b.log".into(), "b.log".into(), false, 5),
        ]
    })
    .unwrap();
    let script = engine
        .load_str(
            r#"
func main(unc) {
    let rows = fs_list(unc)
    let names = []
    for r in rows {
        if r.2 { push(names, r.0) }
    }
    names[0]
}
"#,
        )
        .unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![Value::from("unc")]).unwrap(),
        Value::from("unc/a.txt")
    );
}

#[test]
fn host_takes_array_parameter() {
    let mut engine = Engine::new();
    register_fn!(engine, "sum_list", (Vec<i64>) -> i64, |xs: Vec<i64>| {
        xs.iter().sum()
    })
    .unwrap();
    let script = engine
        .load_str("func main() { let xs = [1, 2, 3]  push(xs, 40)  sum_list(xs) }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "main", vec![]).unwrap(),
        Value::I64(46)
    );
}

#[test]
fn host_rejects_non_array_for_vec_param() {
    let mut engine = Engine::new();
    register_fn!(engine, "sum_list", (Vec<i64>) -> i64, |xs: Vec<i64>| {
        xs.iter().sum()
    })
    .unwrap();
    let script = engine.load_str("func main(x) { sum_list(x) }").unwrap();
    let err = engine
        .call(&script, "main", vec![Value::I64(1)])
        .unwrap_err();
    // 边界按签名拒绝：参数推断出的类型是 [i64]（元素默认收敛），值是标量
    assert!(
        err.message.contains("expected [i64], got i64"),
        "{}",
        err.message
    );
}

// ===========================================================================
// 缓存：数组签名过 .sbc v5 往返
// ===========================================================================

#[test]
fn array_signatures_survive_cache_roundtrip() {
    let dir = std::env::temp_dir().join(format!("brook_arrays_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sbc = dir.join("rows.sbc");
    // 参数标注是逃生舱（既有语义，Q6）：参数只被 `rows[0].0` 使用时，
    // 元素按默认数值收敛成 [(i64)]，宿主传 [(str, str)] 会被边界拒绝——
    // 与元组参数的行为一致，显式标注即解。
    let src = r#"
func pick(rows: [(str, str)]) { rows[0].0 }
func main(rows: [(str, str)]) { pick(rows) }
"#;
    let engine = Engine::new();
    let s1 = engine.compile_cached(src, "rows", &sbc).unwrap();
    let rows = arr(vec![Value::Tuple(vec![
        Value::from("x"),
        Value::from("y"),
    ])]);
    assert_eq!(
        engine.call(&s1, "main", vec![rows.clone()]).unwrap(),
        Value::from("x")
    );

    // 缓存解码后行为一致（签名里含 [T] 与 [(str, str)]）
    let s2 = engine.load_sbc(&sbc).unwrap();
    assert_eq!(
        engine.call(&s2, "main", vec![rows]).unwrap(),
        Value::from("x")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// for-in 期间的 push：与 Python 一致，遍历会看到新元素
// ===========================================================================

#[test]
fn push_during_iteration_extends_the_walk() {
    // [1] → push 2, 3 → 遍历看到 1,2,3（两个引擎同语义）
    assert_both!(
        "func main() { let xs = [1]  let seen = []  for x in xs { push(seen, x)  if x < 3 { push(xs, x + 1) } }  len(seen) }",
        "main",
        vec![],
        Value::I64(3)
    );
}

// ===========================================================================
// 数组作为返回值跨函数传递
// ===========================================================================

#[test]
fn arrays_flow_through_functions() {
    assert_both!(
        r#"
func evens(xs) {
    let out = []
    for x in xs { if x % 2 == 0 { push(out, x) } }
    out
}
func main() {
    let r = evens([1, 2, 3, 4, 5, 6])
    len(r) + r[2]
}
"#,
        "main",
        vec![],
        Value::I64(9)
    );
}

// ===========================================================================
// 解释器与 VM 的数组 Display 一致（错误消息 / to_str 场景的前置）
// ===========================================================================

#[test]
fn array_display_is_stable() {
    let v = arr(vec![Value::I64(1), Value::from("a"), Value::Bool(false)]);
    assert_eq!(v.to_string(), "[1, a, false]");
}

/// 桥接辅助：确认 Rc/RefCell 仍来自 std（零依赖原则——数组 payload 用
/// `Rc<RefCell<Vec<Value>>>` 不引入任何 crate）。
#[allow(dead_code)]
fn bridge_check() {
    let _ = Rc::new(RefCell::new(0i64));
}
