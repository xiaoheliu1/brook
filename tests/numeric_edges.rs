//! Numeric edge cases that must stay deterministic across the VM and the
//! direct interpreter path. In particular, host-supplied `i64::MIN` must
//! never unwind into Rust's release-mode wrapping behaviour.

use brook::{Engine, Value};

fn call(src: &str, name: &str, args: Vec<Value>) -> Value {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script must compile");
    engine.call(&script, name, args).expect("call must succeed")
}

fn call_err(src: &str, name: &str, args: Vec<Value>) -> String {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script must compile");
    engine
        .call(&script, name, args)
        .expect_err("call must fail")
        .to_string()
}

#[test]
fn i64_min_is_a_valid_value_for_min_and_max() {
    let src = "func lo(a: i64, b: i64) { min(a, b) }\n\
               func hi(a: i64, b: i64) { max(a, b) }";
    assert_eq!(
        call(src, "lo", vec![Value::I64(i64::MIN), Value::I64(-1)]),
        Value::I64(i64::MIN)
    );
    assert_eq!(
        call(src, "hi", vec![Value::I64(i64::MIN), Value::I64(-1)]),
        Value::I64(-1)
    );
}

#[test]
fn i64_division_and_remainder_edges_are_errors_not_panics() {
    let div = "func f(a: i64, b: i64) { a / b }";
    let rem = "func f(a: i64, b: i64) { a % b }";

    let cases = [
        (div, i64::MIN, -1, "i64 div overflow"),
        (div, 7, 0, "division by zero"),
        (rem, i64::MIN, -1, "i64 mod overflow"),
        (rem, 7, 0, "modulo by zero"),
    ];
    for (src, a, b, expected) in cases {
        let msg = call_err(src, "f", vec![Value::I64(a), Value::I64(b)]);
        assert!(msg.contains(expected), "expected {expected:?}, got {msg}");
    }
}

#[test]
fn i64_arithmetic_overflow_is_reported_with_source_location() {
    let engine = Engine::new();
    let script = engine
        .load_str("func f(a: i64, b: i64) {\n    a * b\n}")
        .unwrap();
    let err = engine
        .call(&script, "f", vec![Value::I64(i64::MAX), Value::I64(2)])
        .expect_err("overflow must fail");
    let msg = err.to_string();
    assert!(msg.contains("i64 mul overflow"), "{msg}");
    assert_eq!(err.span.map(|s| s.line), Some(2), "{msg}");
}

#[test]
fn f64_min_max_ignore_nan_instead_of_propagating_it() {
    let src = "func lo(a: f64, b: f64) { min(a, b) }\n\
               func hi(a: f64, b: f64) { max(a, b) }";
    match call(src, "lo", vec![Value::F64(f64::NAN), Value::F64(2.5)]) {
        Value::F64(v) => assert_eq!(v, 2.5),
        other => panic!("expected f64, got {other:?}"),
    }
    match call(src, "hi", vec![Value::F64(f64::NAN), Value::F64(2.5)]) {
        Value::F64(v) => assert_eq!(v, 2.5),
        other => panic!("expected f64, got {other:?}"),
    }
}

#[test]
fn f64_abs_and_contains_have_stable_nan_semantics() {
    let engine = Engine::new();
    let script = engine
        .load_str("func a(x: f64) { abs(x) }\nfunc c(xs: [f64], x: f64) { contains(xs, x) }")
        .unwrap();

    match engine
        .call(&script, "a", vec![Value::F64(f64::NAN)])
        .unwrap()
    {
        Value::F64(v) => assert!(v.is_nan()),
        other => panic!("expected f64, got {other:?}"),
    }

    // IEEE equality says NaN is not equal to itself; `contains` must not
    // accidentally treat bit patterns as identity. Conversely, -0.0 and 0.0
    // are equal as f64 values even though their bits differ.
    assert_eq!(
        engine
            .call(
                &script,
                "c",
                vec![Value::arr(vec![Value::F64(f64::NAN)]), Value::F64(f64::NAN),],
            )
            .unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        engine
            .call(
                &script,
                "c",
                vec![Value::arr(vec![Value::F64(-0.0)]), Value::F64(0.0),],
            )
            .unwrap(),
        Value::Bool(true)
    );
}
