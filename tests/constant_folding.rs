//! Constant folding (D-050, shallow) and immediate-operand coverage.
//!
//! Three invariants matter here beyond "the answer is right":
//!
//! * a *shallow* foldable operation (operands are direct literals) collapses
//!   to ONE Const* instruction — otherwise the pass is dead code;
//! * deep literal trees are deliberately NOT whole-tree folded at the AST
//!   layer: `compile_expr`'s per-node whole-tree retry is O(n²) on
//!   variable-bearing left-deep chains and measured a ~40% compile-time
//!   regression on fib.bk. Deeper chains are covered by the existing
//!   `AddI64Imm` immediate fusion instead;
//! * checked failures (overflow, div-by-zero) must stay runtime errors in the
//!   VM, so a bad literal in a never-executed branch cannot fail compilation.

use brook::ir::Op;
use brook::{Engine, Value};

fn eval(src: &str, args: Vec<Value>) -> Value {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script must compile");
    engine.call(&script, "f", args).expect("call must succeed")
}

fn count<F: Fn(&Op) -> bool>(src: &str, pred: F) -> usize {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script must compile");
    script.code.funcs[0]
        .code
        .iter()
        .filter(|i| pred(&i.op))
        .count()
}

#[test]
fn shallow_literal_ops_collapse_and_deep_trees_stay_correct() {
    let engine = Engine::new();
    // 浅折叠：`3 * 4`（操作数都是直接字面量）折成一条 ConstI64，乘法指令消失
    let script = engine.load_str("func f(x) { x + 3 * 4 }").unwrap();
    let code = &script.code.funcs[0].code;
    assert!(
        !code.iter().any(|i| matches!(i.op, Op::MulI64 { .. })),
        "3 * 4 must be folded away, got {code:?}"
    );
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(2)]).unwrap(),
        Value::I64(14)
    );
    // 深字面量树不做 AST 级全折（见模块注释的 O(n²) 取舍），结果不变
    let deep = engine.load_str("func f() { 2 + 3 * 4 }").unwrap();
    assert_eq!(engine.call(&deep, "f", vec![]).unwrap(), Value::I64(14));
}

#[test]
fn float_and_bool_literal_trees_fold() {
    assert_eq!(
        eval("func f() { 1.5 * 2.0 + 0.5 }", vec![]),
        Value::F64(3.5)
    );
    assert_eq!(eval("func f() { !(1 < 2) }", vec![]), Value::Bool(false));
    assert_eq!(
        eval("func f() { (2 == 2) && (3 > 4) }", vec![]),
        Value::Bool(false)
    );
    // IEEE equality must survive folding just as it does in the VM.
    assert_eq!(eval("func f() { 0.0 == -0.0 }", vec![]), Value::Bool(true));
}

#[test]
fn constant_operand_uses_immediate_opcode() {
    // `2 + x` commutes; `x + (3+4)` has its RHS folded first. Both must become
    // AddI64Imm rather than a materialised literal + AddI64.
    let src = "func f(x) { 2 + x + (3 + 4) }";
    assert_eq!(
        count(src, |op| matches!(op, Op::AddI64Imm { .. })),
        2,
        "expected two immediates"
    );
    assert_eq!(count(src, |op| matches!(op, Op::AddI64 { .. })), 0);
    assert_eq!(
        count(src, |op| matches!(op, Op::ConstI64 { val: 7, .. })),
        0
    );
    assert_eq!(eval(src, vec![Value::I64(5)]), Value::I64(14));
}

#[test]
fn subtraction_by_folded_constant_uses_add_immediate() {
    let src = "func f(x) { x - (2 + 3) }";
    assert_eq!(
        count(src, |op| matches!(op, Op::AddI64Imm { imm: -5, .. })),
        1
    );
    assert_eq!(count(src, |op| matches!(op, Op::SubI64 { .. })), 0);
    assert_eq!(eval(src, vec![Value::I64(10)]), Value::I64(5));
}

#[test]
fn fused_branch_recognises_folded_literal() {
    let src = "func f(n) { if n < 2 + 3 { 1 } else { 0 } }";
    assert_eq!(
        count(src, |op| matches!(op, Op::JmpIfGeI64Imm { imm: 5, .. })),
        1
    );
    assert_eq!(eval(src, vec![Value::I64(4)]), Value::I64(1));
    assert_eq!(eval(src, vec![Value::I64(5)]), Value::I64(0));
}

#[test]
fn checked_failures_stay_runtime_errors() {
    let cases = [
        ("func f() { 9223372036854775807 + 1 }", "i64 add overflow"),
        ("func f() { 1 / 0 }", "division by zero"),
        ("func f() { 1 % 0 }", "modulo by zero"),
        ("func f() { 1.0 / 0.0 }", "division by zero"),
    ];
    for (src, expected) in cases {
        let engine = Engine::new();
        let script = engine
            .load_str(src)
            .expect("a bad constant must still compile");
        let msg = engine
            .call(&script, "f", vec![])
            .expect_err("error must surface at runtime")
            .to_string();
        assert!(msg.contains(expected), "expected {expected:?}, got {msg}");
    }
}

#[test]
fn bad_constant_in_dead_branch_is_not_a_compile_error() {
    let engine = Engine::new();
    let script = engine
        .load_str("func f(x) { if x > 100 { 1 / 0 } else { x } }")
        .expect("dead bad literal must not fail compilation");
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(1)]).unwrap(),
        Value::I64(1)
    );
    assert!(engine.call(&script, "f", vec![Value::I64(200)]).is_err());
}

#[test]
fn folding_does_not_remove_calls_or_allocations() {
    // Only literal-only subtrees are folded. `"a"+"b"` allocates on the VM
    // heap and a host call may have side effects; neither may be collapsed.
    let src = "func f() { \"a\" + \"b\" }";
    assert_eq!(count(src, |op| matches!(op, Op::Concat { .. })), 1);
    assert_eq!(eval(src, vec![]), Value::Str("ab".into()));

    let mut engine = Engine::new();
    brook::register_fn!(engine, "bump", (i64) -> i64, |x| x + 1).unwrap();
    let script = engine.load_str("func f(x) { bump(x) * 0 }").unwrap();
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(41)]).unwrap(),
        Value::I64(0)
    );
}
