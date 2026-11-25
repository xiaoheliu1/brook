//! ①a: `Assign` compiles its RHS straight into the target variable's slot.
//!
//! The hint is only applied to nodes with their own result register; operands
//! are compiled normally, so a hinted slot that is also read by the operation
//! (`x = x + 1`, `x = add1(x) + x`) must still see the old value.

use brook::ir::Op;
use brook::value::Value;
use brook::{register_fn, Engine};

#[test]
fn binary_assignment_writes_target_slot_without_mov() -> brook::Result<()> {
    let engine = Engine::new();
    let script = engine.load_str("func f(a, b) { let x = 0\n x = a + b\n x }")?;
    let code = &script.code.funcs[0].code;
    let x = match code[0].op {
        Op::ConstI64 { dst, .. } => dst,
        _ => panic!("first instruction should initialise x"),
    };
    assert!(
        code.iter().any(|i| matches!(i.op,
            Op::AddI64 { dst, a: 0, b: 1 } if dst == x)),
        "assignment should emit AddI64 directly into x's slot: {code:?}"
    );
    assert!(
        !code
            .iter()
            .any(|i| matches!(i.op, Op::Mov { dst, .. } if dst == x)),
        "the redundant Mov should be gone: {code:?}"
    );
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(3), Value::I64(4)])?,
        Value::I64(7)
    );
    Ok(())
}

#[test]
fn hinted_slot_may_also_be_an_operand() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "add1", (i64) -> i64, |x| x + 1)?;

    let self_alias = engine.load_str("func f(a) { let x = a\n x = x + 1\n x }")?;
    assert_eq!(
        engine.call(&self_alias, "f", vec![Value::I64(5)])?,
        Value::I64(6)
    );

    // `add1` gathers Values before the VM writes dst, so the second `x` in
    // `add1(x) + x` still reads the original slot.
    let host_alias = engine.load_str("func g(a) { let x = a\n x = add1(x) + x\n x }")?;
    assert_eq!(
        engine.call(&host_alias, "g", vec![Value::I64(5)])?,
        Value::I64(11)
    );
    Ok(())
}

#[test]
fn short_circuit_assignment_does_not_clobber_an_operand() -> brook::Result<()> {
    let engine = Engine::new();
    let and = engine.load_str("func f(a: bool, b: bool) { let x = a\n x = b && x\n x }")?;
    assert_eq!(
        engine.call(&and, "f", vec![Value::Bool(false), Value::Bool(true)])?,
        Value::Bool(false)
    );

    let or = engine.load_str("func g(a: bool, b: bool) { let x = a\n x = b || x\n x }")?;
    assert_eq!(
        engine.call(&or, "g", vec![Value::Bool(true), Value::Bool(false)])?,
        Value::Bool(true)
    );
    Ok(())
}

#[test]
fn fallback_nodes_keep_their_explicit_mov() -> brook::Result<()> {
    let engine = Engine::new();
    // `if` has its own result register; the hint must not leak into branches.
    let branch =
        engine.load_str("func f(a, b, c) { let x = 0\n x = if c { a } else { b }\n x }")?;
    assert_eq!(
        engine.call(
            &branch,
            "f",
            vec![Value::I64(3), Value::I64(4), Value::Bool(true)]
        )?,
        Value::I64(3)
    );
    assert_eq!(
        engine.call(
            &branch,
            "f",
            vec![Value::I64(3), Value::I64(4), Value::Bool(false)]
        )?,
        Value::I64(4)
    );

    // Tuple construction likewise ignores the hint.
    let tuple = engine.load_str("func t(a, b) { let x = (0, 0)\n x = (a, b)\n x.1 }")?;
    assert_eq!(
        engine.call(&tuple, "t", vec![Value::I64(3), Value::I64(9)])?,
        Value::I64(9)
    );
    Ok(())
}

#[test]
fn short_circuit_and_literals_honour_the_hint() -> brook::Result<()> {
    let engine = Engine::new();
    let logic = engine.load_str("func f(a, b) { let x = true\n x = a > 0 && b > 0\n x }")?;
    assert_eq!(
        engine.call(&logic, "f", vec![Value::I64(1), Value::I64(1)])?,
        Value::Bool(true)
    );
    assert_eq!(
        engine.call(&logic, "f", vec![Value::I64(0), Value::I64(1)])?,
        Value::Bool(false)
    );

    let text = engine.load_str("func g() { let s = \"a\"\n s = \"b\"\n s }")?;
    assert_eq!(engine.call(&text, "g", vec![])?, Value::Str("b".into()));
    Ok(())
}
