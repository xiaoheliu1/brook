//! Phase 3: typed host registration via `register_fn!`.
//!
//! These tests exist to prove that the macro-derived signature is actually
//! used by the type checker and the VM — a handwritten `Vec<Ty>` that drifts
//! from its callback used to cause silent miscomputation.

use vitul::host::{FromValue, ScriptType};
use vitul::ty::Ty;
use vitul::value::Value;
use vitul::{register_fn, Engine, Error, Opaque};

#[test]
fn typed_scalar_signature_flows_through_typechecker() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "scale", (i64, f64) -> f64, |n, k| n as f64 * k)?;

    let script = engine.load_str("func go(n) { scale(n, 1.5) }")?;
    assert_eq!(engine.call(&script, "go", vec![Value::I64(4)])?, Value::F64(6.0));

    // The i64 parameter is not coercible from a string: the signature came
    // from Rust types, so the mismatch is a compile error, not a runtime bug.
    let err = engine
        .load_str("func bad() { scale(\"x\", 1.5) }")
        .unwrap_err();
    assert_eq!(err.kind, vitul::ErrorKind::Type);
    Ok(())
}

#[test]
fn short_form_infers_return_type_and_zero_args() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "tick", (), || 7i64)?;
    register_fn!(engine, "add", (i64, i64), |a, b| a + b)?;

    let script = engine.load_str("func go() { add(tick(), 3) }")?;
    assert_eq!(engine.call(&script, "go", vec![])?, Value::I64(10));
    Ok(())
}

#[test]
fn opaque_handle_replaces_manual_signature() -> vitul::Result<()> {
    #[derive(Debug)]
    struct Order { total: f64, vip: bool }

    let mut engine = Engine::new();
    register_fn!(engine, "order_total", (Opaque<Order>) -> f64, |o| o.total)?;
    register_fn!(engine, "is_vip", (Opaque<Order>) -> bool, |o| o.vip)?;
    engine.register_type::<Order>()?;

    let script = engine.load_file("examples/risk.vt")?;
    let order = Value::from(Opaque::new(Order { total: 200.0, vip: true }));
    assert_eq!(
        engine.call(&script, "check", vec![order, Value::F64(1000.0)])?,
        Value::Tuple(vec![Value::F64(160.0), Value::Bool(true)])
    );

    // A handle of a different Rust type must be rejected at the boundary,
    // never downcast-panicked inside a host callback.
    #[derive(Debug)]
    struct Other;
    assert_eq!(
        <Opaque<Order> as ScriptType>::script_ty(),
        Ty::Handle("Order".into())
    );
    let wrong = Opaque::<Order>::from_value(&Value::handle(Other));
    assert!(wrong.is_err(), "handle type mismatch must surface as Err");
    Ok(())
}

#[test]
fn strings_tuples_and_str_literal_results() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "label", (String) -> String, |s| format!("[{}]", s))?;
    register_fn!(engine, "greet", (String) -> &'static str, |_| "hello")?;
    register_fn!(engine, "sum_pair", ((i64, i64)) -> i64, |(a, b)| a + b)?;

    let script = engine.load_str(
        r#"
        func tag(s: str) { label(s) }
        func hi(s: str) { greet(s) }
        func go(a, b) { sum_pair((a, b)) }
        "#,
    )?;
    assert_eq!(engine.call(&script, "tag", vec![Value::from("x")])?, Value::from("[x]"));
    assert_eq!(engine.call(&script, "hi", vec![Value::from("x")])?, Value::from("hello"));
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(3), Value::I64(4)])?,
        Value::I64(7)
    );
    Ok(())
}

#[test]
fn fallible_host_result_is_propagated() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "recip", (f64) -> vitul::Result<f64>, |x| {
        if x == 0.0 { Err(Error::runtime("division by zero in host")) } else { Ok(1.0 / x) }
    })?;
    let script = engine.load_str("func go(x) { recip(x) }")?;

    assert_eq!(engine.call(&script, "go", vec![Value::F64(4.0)])?, Value::F64(0.25));
    let err = engine.call(&script, "go", vec![Value::F64(0.0)]).unwrap_err();
    assert_eq!(err.kind, vitul::ErrorKind::Runtime);
    assert!(err.message.contains("division by zero in host"), "got: {}", err.message);
    Ok(())
}

#[test]
fn duplicate_host_registration_is_rejected() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "same", (), || 1i64)?;
    let err = register_fn!(engine, "same", (), || 2i64).unwrap_err();
    assert_eq!(err.kind, vitul::ErrorKind::Runtime);
    assert!(err.message.contains("already registered"), "got: {}", err.message);
    Ok(())
}

#[test]
fn six_argument_registration_is_supported() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(
        engine,
        "mix",
        (i64, i64, i64, i64, i64, i64) -> i64,
        |a, b, c, d, e, g| a + b + c + d + e + g,
    )?;
    let script = engine.load_str("func go() { mix(1, 2, 3, 4, 5, 6) }")?;
    assert_eq!(engine.call(&script, "go", vec![])?, Value::I64(21));
    Ok(())
}
