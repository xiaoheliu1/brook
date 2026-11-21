//! Regression tests for ⑥a (call/ret slimming).
//!
//! The dispatch loop keeps `nregs` in a local, restored from the frame on
//! `Ret`. A self-recursive function (fib) cannot expose a wrong restore because
//! every frame has the same window size. These tests chain functions with
//! *different* `nregs` at each depth so a stale `nregs` corrupts the next
//! call's window start and fails loudly.

use brook::engine::Engine;
use brook::error::Result;
use brook::value::Value;

fn run(src: &str, func: &str, args: Vec<Value>) -> Result<Value> {
    let engine = Engine::new();
    let script = engine.load_str(src)?;
    engine.call(&script, func, args)
}

#[test]
fn nested_frames_with_different_window_sizes() -> Result<()> {
    // outer has 5 registers (a,b + 3 temps), mid has 3, inner has 2. Each
    // returns a value that the caller then uses, so argument copying at each
    // depth must land in the right window slot.
    let src = r#"
        func inner(x) { let t = x + 1
 t * 2 }
        func mid(p, q) { let u = inner(p)
 u + q }
        func outer(a, b) { let v = mid(a, b)
 v - 7 }
    "#;
    // inner(5)=12; mid(5,8)=20; outer=13
    assert_eq!(run(src, "outer", vec![Value::I64(5), Value::I64(8)])?, Value::I64(13));
    Ok(())
}

#[test]
fn sibling_calls_after_return() -> Result<()> {
    // Two sequential calls from a wide caller into a narrow callee: after the
    // first Ret restores the caller window, the second must start at the same
    // base again. A stale callee nregs would shift the second window.
    let src = r#"
        func one(x) { x + 1 }
        func two(x, y) { x * y }
        func go(a, b, c) {
            let p = one(a)
            let q = two(b, c)
            p + q
        }
    "#;
    // one(4)=5; two(5,6)=30; 35
    assert_eq!(run(src, "go", vec![Value::I64(4), Value::I64(5), Value::I64(6)])?, Value::I64(35));
    Ok(())
}

#[test]
fn deep_mixed_recursion() -> Result<()> {
    // Alternating window sizes down a recursive chain.
    let src = r#"
        func even(n) { if n == 0 { 1 } else { odd(n - 1) } }
        func odd(n) { if n == 0 { 0 } else { even(n - 1) } }
    "#;
    assert_eq!(run(src, "even", vec![Value::I64(10)])?, Value::I64(1));
    assert_eq!(run(src, "odd", vec![Value::I64(10)])?, Value::I64(0));
    assert_eq!(run(src, "even", vec![Value::I64(11)])?, Value::I64(0));
    Ok(())
}

#[test]
fn many_args_across_windows() -> Result<()> {
    // Six arguments (near MAX_OPERANDS) exercise the full in-place copy loop.
    let src = r#"
        func sum6(a, b, c, d, e, f) { a + b + c + d + e + f }
        func wrap(x, y) { let s = sum6(x, y, x, y, x, y)
 s + 1 }
    "#;
    // sum6(3,4,3,4,3,4)=21; +1=22
    assert_eq!(run(src, "wrap", vec![Value::I64(3), Value::I64(4)])?, Value::I64(22));
    Ok(())
}
