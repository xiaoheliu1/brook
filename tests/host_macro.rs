//! Phase 3: typed host registration via `register_fn!`.
//!
//! These tests exist to prove that the macro-derived signature is actually
//! used by the type checker and the VM — a handwritten `Vec<Ty>` that drifts
//! from its callback used to cause silent miscomputation.

use brook::host::{FromValue, ScriptType};
use brook::ty::Ty;
use brook::value::Value;
use brook::{register_fn, Engine, Error, Opaque};

#[test]
fn typed_scalar_signature_flows_through_typechecker() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "scale", (i64, f64) -> f64, |n, k| n as f64 * k)?;

    let script = engine.load_str("func go(n) { scale(n, 1.5) }")?;
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(4)])?,
        Value::F64(6.0)
    );

    // The i64 parameter is not coercible from a string: the signature came
    // from Rust types, so the mismatch is a compile error, not a runtime bug.
    let err = engine
        .load_str("func bad() { scale(\"x\", 1.5) }")
        .unwrap_err();
    assert_eq!(err.kind, brook::ErrorKind::Type);
    Ok(())
}

#[test]
fn short_form_infers_return_type_and_zero_args() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "tick", (), || 7i64)?;
    register_fn!(engine, "add", (i64, i64), |a, b| a + b)?;

    let script = engine.load_str("func go() { add(tick(), 3) }")?;
    assert_eq!(engine.call(&script, "go", vec![])?, Value::I64(10));
    Ok(())
}

#[test]
fn opaque_handle_replaces_manual_signature() -> brook::Result<()> {
    #[derive(Debug)]
    struct Order {
        total: f64,
        vip: bool,
    }

    let mut engine = Engine::new();
    register_fn!(engine, "order_total", (Opaque<Order>) -> f64, |o| o.total)?;
    register_fn!(engine, "is_vip", (Opaque<Order>) -> bool, |o| o.vip)?;
    engine.register_type::<Order>()?;

    let script = engine.load_file("examples/risk.bk")?;
    let order = Value::from(Opaque::new(Order {
        total: 200.0,
        vip: true,
    }));
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
fn strings_tuples_and_str_literal_results() -> brook::Result<()> {
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
    assert_eq!(
        engine.call(&script, "tag", vec![Value::from("x")])?,
        Value::from("[x]")
    );
    assert_eq!(
        engine.call(&script, "hi", vec![Value::from("x")])?,
        Value::from("hello")
    );
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(3), Value::I64(4)])?,
        Value::I64(7)
    );
    Ok(())
}

#[test]
fn fallible_host_result_is_propagated() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "recip", (f64) -> brook::Result<f64>, |x| {
        if x == 0.0 { Err(Error::runtime("division by zero in host")) } else { Ok(1.0 / x) }
    })?;
    let script = engine.load_str("func go(x) { recip(x) }")?;

    assert_eq!(
        engine.call(&script, "go", vec![Value::F64(4.0)])?,
        Value::F64(0.25)
    );
    let err = engine
        .call(&script, "go", vec![Value::F64(0.0)])
        .unwrap_err();
    assert_eq!(err.kind, brook::ErrorKind::Runtime);
    assert!(
        err.message.contains("division by zero in host"),
        "got: {}",
        err.message
    );
    Ok(())
}

#[test]
fn duplicate_host_registration_is_rejected() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "same", (), || 1i64)?;
    let err = register_fn!(engine, "same", (), || 2i64).unwrap_err();
    assert_eq!(err.kind, brook::ErrorKind::Runtime);
    assert!(
        err.message.contains("already registered"),
        "got: {}",
        err.message
    );
    Ok(())
}

#[test]
fn six_argument_registration_is_supported() -> brook::Result<()> {
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

/// The language accepts up to `MAX_OPERANDS` = 8 arguments; typed host
/// registration must reach the same ceiling (both the `-> R` and inferred
/// forms of the macro).
#[test]
fn seven_and_eight_argument_registration_are_supported() -> brook::Result<()> {
    let mut engine = Engine::new();
    register_fn!(
        engine,
        "seven",
        (i64, i64, i64, i64, i64, i64, i64) -> i64,
        |a, b, c, d, e, g, h| a + b + c + d + e + g + h,
    )?;
    register_fn!(
        engine,
        "eight",
        (i64, i64, i64, i64, i64, i64, i64, i64) -> i64,
        |a, b, c, d, e, g, h, i| a + b + c + d + e + g + h + i,
    )?;
    register_fn!(
        engine,
        "inferred7",
        (i64, i64, i64, i64, i64, i64, i64),
        |a, b, c, d, e, g, h| a + b + c + d + e + g + h,
    )?;
    let script = engine.load_str(
        "func go() { seven(1,2,3,4,5,6,7) + eight(1,1,1,1,1,1,1,1) + inferred7(1,1,1,1,1,1,1) }",
    )?;
    assert_eq!(engine.call(&script, "go", vec![])?, Value::I64(28 + 8 + 7));
    Ok(())
}
