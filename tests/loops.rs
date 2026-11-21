//! `break` / `continue` semantics.
//!
//! The expansion reuses `Op::Jump`: `break` lands on the first instruction
//! after the loop, `continue` lands on the loop's next-iteration entry point
//! (the condition test for `while`, the increment for `for`). The `for` case
//! is the subtle one — landing on the condition instead of the increment
//! would spin forever on the same index, which is exactly what
//! `for_continue_still_advances` guards.

use miniscript::engine::Engine;
use miniscript::error::Result;
use miniscript::value::Value;

#[test]
fn while_break_exits_early() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func first_over(limit) {
            let i = 0
            let found = -1
            while i < 100 {
                if i * i > limit {
                    found = i
                    break
                }
                i = i + 1
            }
            found
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "first_over", vec![Value::I64(50)])?, Value::I64(8));
    Ok(())
}

#[test]
fn while_continue_skips_rest_of_body() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func sum_odd(n) {
            let i = 0
            let total = 0
            while i < n {
                i = i + 1
                if i % 2 == 0 { continue }
                total = total + i
            }
            total
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "sum_odd", vec![Value::I64(6)])?, Value::I64(9));
    Ok(())
}

#[test]
fn for_break_exits_early() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func first_divisor(n) {
            let d = -1
            for i in 2..n {
                if n % i == 0 {
                    d = i
                    break
                }
            }
            d
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "first_divisor", vec![Value::I64(91)])?, Value::I64(7));
    assert_eq!(engine.call(&script, "first_divisor", vec![Value::I64(97)])?, Value::I64(-1));
    Ok(())
}

/// Regression: `continue` in a `for` body must run the increment. If it jumped
/// to the condition test instead, `i` would never change and the gas limit
/// would abort the call.
#[test]
fn for_continue_still_advances() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func sum_even(n) {
            let total = 0
            for i in 0..n {
                if i % 2 == 1 { continue }
                total = total + i
            }
            total
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "sum_even", vec![Value::I64(6)])?, Value::I64(6));
    Ok(())
}

/// `break` leaves only the innermost loop; the outer one keeps going.
#[test]
fn break_only_leaves_the_inner_loop() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func triangle(n) {
            let count = 0
            for i in 0..n {
                for j in 0..n {
                    if j > i { break }
                    count = count + 1
                }
            }
            count
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "triangle", vec![Value::I64(3)])?, Value::I64(6));
    Ok(())
}

/// `continue` in the inner loop must not disturb the outer iteration.
#[test]
fn continue_in_inner_loop_is_scoped() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func even_cells(n) {
            let count = 0
            for i in 0..n {
                for j in 0..n {
                    if j % 2 == 1 { continue }
                    count = count + 1
                }
            }
            count
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "even_cells", vec![Value::I64(3)])?, Value::I64(6));
    Ok(())
}

/// A `while` as the function's last statement makes `break`'s target the very
/// end of the instruction stream. `validate` requires `target < code.len()`,
/// so this would fail at load time if the target were off by one.
#[test]
fn break_at_function_end_keeps_target_in_range() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func f(n) {
            while n > 0 {
                break
            }
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "f", vec![Value::I64(5)])?, Value::Unit);
    Ok(())
}

/// `break` in expression position: the branch diverges, so the value type must
/// come from the other branch rather than collapsing to unit.
#[test]
fn break_in_if_expression_keeps_value_type() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func sum_until(n) {
            let total = 0
            for i in 0..n {
                let v = if i == 2 { break } else { i }
                total = total + v
            }
            total
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "sum_until", vec![Value::I64(5)])?, Value::I64(1));
    Ok(())
}

#[test]
fn break_outside_loop_is_a_compile_error() {
    let engine = Engine::new();
    let err = engine.load_str("func f() { break }").unwrap_err();
    assert!(err.message.contains("outside of a loop"), "got: {}", err.message);
}

#[test]
fn continue_outside_loop_is_a_compile_error() {
    let engine = Engine::new();
    let err = engine.load_str("func f() { continue }").unwrap_err();
    assert!(err.message.contains("outside of a loop"), "got: {}", err.message);
}

/// `break` inside an `if` that is not inside a loop is still an error: the
/// type checker tracks loop nesting, not block nesting.
#[test]
fn break_inside_if_without_loop_is_rejected() {
    let engine = Engine::new();
    let err = engine
        .load_str("func f(n) { if n > 0 { break } }")
        .unwrap_err();
    assert!(err.message.contains("outside of a loop"), "got: {}", err.message);
}

/// Jump targets are absolute instruction indices, so they must survive the
/// `.sbc` roundtrip unchanged. A cached script must behave identically to the
/// freshly compiled one.
#[test]
fn cached_script_preserves_loop_targets() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("miniscript_loops_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("loops.sbc");

    let src = r#"
        func sum_odd(n) {
            let i = 0
            let total = 0
            while i < n {
                i = i + 1
                if i % 2 == 0 { continue }
                total = total + i
            }
            total
        }
        func first_divisor(n) {
            let d = -1
            for i in 2..n {
                if n % i == 0 {
                    d = i
                    break
                }
            }
            d
        }
        "#;

    let engine = Engine::new();
    let fresh = engine.compile_cached(src, "loops", &path)?;
    assert_eq!(engine.call(&fresh, "sum_odd", vec![Value::I64(6)])?, Value::I64(9));

    let cached = engine.compile_cached(src, "loops", &path)?;
    assert_eq!(engine.call(&cached, "sum_odd", vec![Value::I64(6)])?, Value::I64(9));
    assert_eq!(
        engine.call(&cached, "first_divisor", vec![Value::I64(91)])?,
        Value::I64(7)
    );

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
