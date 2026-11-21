//! ⑤: `JmpIf*I64Imm` fused compare-immediate branches. Verify the six
//! comparison operators, boundary literals and negative literals all produce
//! the same result as the unfused (register-register) form.

use miniscript::{Engine, Value};

fn check(src: &str, n: i64, expect: i64) -> miniscript::Result<()> {
    let e = Engine::new();
    let script = e.load_str(src)?;
    assert_eq!(
        e.call(&script, "f", vec![Value::I64(n)])?,
        Value::I64(expect),
        "{src} with n={n}"
    );
    Ok(())
}

#[test]
fn all_six_i64_comparisons_against_literal() -> miniscript::Result<()> {
    // `if n < 2 { 1 } else { 0 }` returns 1 iff the predicate is true.
    check("func f(n) { if n < 2 { 1 } else { 0 } }", 1, 1)?;
    check("func f(n) { if n < 2 { 1 } else { 0 } }", 2, 0)?;
    check("func f(n) { if n <= 2 { 1 } else { 0 } }", 2, 1)?;
    check("func f(n) { if n <= 2 { 1 } else { 0 } }", 3, 0)?;
    check("func f(n) { if n > 2 { 1 } else { 0 } }", 3, 1)?;
    check("func f(n) { if n > 2 { 1 } else { 0 } }", 2, 0)?;
    check("func f(n) { if n >= 2 { 1 } else { 0 } }", 2, 1)?;
    check("func f(n) { if n >= 2 { 1 } else { 0 } }", 1, 0)?;
    check("func f(n) { if n == 2 { 1 } else { 0 } }", 2, 1)?;
    check("func f(n) { if n == 2 { 1 } else { 0 } }", 5, 0)?;
    check("func f(n) { if n != 2 { 1 } else { 0 } }", 5, 1)?;
    check("func f(n) { if n != 2 { 1 } else { 0 } }", 2, 0)?;
    Ok(())
}

#[test]
fn negative_literal_and_loop_condition() -> miniscript::Result<()> {
    // Negative immediate must round-trip through `imm as u64` in the cache.
    check("func f(n) { if n >= -1 { 1 } else { 0 } }", -1, 1)?;
    check("func f(n) { if n >= -1 { 1 } else { 0 } }", -2, 0)?;
    // Literal on the left is NOT fused (lhs must be non-literal) but must
    // still evaluate correctly via the ordinary register-register path.
    check("func f(n) { if 2 < n { 1 } else { 0 } }", 3, 1)?;
    // Loop condition with a literal bound exercises back-edge patching of the
    // new opcode's target.
    let e = Engine::new();
    let s = e.load_str("func f(n) { let t = 0\n while n < 3 { t = t + n\n n = n + 1 }\n t }")?;
    assert_eq!(e.call(&s, "f", vec![Value::I64(0)])?, Value::I64(3));
    Ok(())
}

#[test]
fn fib_recursion_still_correct() -> miniscript::Result<()> {
    let e = Engine::new();
    let s = e.load_str(
        "func fib(n) { if n < 2 { return n } fib(n-1) + fib(n-2) }",
    )?;
    assert_eq!(e.call(&s, "fib", vec![Value::I64(10)])?, Value::I64(55));
    Ok(())
}
