//! `abs` / `min` / `max`: numeric specialization (Q7).
//!
//! These three keep an `i64` argument as `i64` instead of forcing the script to
//! write `to_f64(...)` and then `to_i64(...)` again. The specialization is a
//! per-call choice in codegen (`ir::builtin::numeric_id`), not overload syntax
//! and not a new opcode — but it does add three new builtin ids, so the `.sbc`
//! path is covered here too.

use brook::ty::Ty;
use brook::{Engine, Value};

fn call(src: &str, name: &str, args: Vec<Value>) -> Value {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script must compile");
    engine.call(&script, name, args).expect("call must succeed")
}

fn load_err(src: &str) -> String {
    Engine::new().load_str(src).expect_err("expected a compile error").to_string()
}

#[test]
fn abs_keeps_i64() {
    assert_eq!(call("func f() { abs(-5) }", "f", vec![]), Value::I64(5));
    assert_eq!(call("func f() { abs(5) }", "f", vec![]), Value::I64(5));
    // The result stays usable in integer arithmetic without a cast.
    assert_eq!(call("func f() { abs(-5) * 2 }", "f", vec![]), Value::I64(10));
}

#[test]
fn abs_keeps_f64() {
    assert_eq!(call("func f() { abs(-1.5) }", "f", vec![]), Value::F64(1.5));
}

#[test]
fn min_max_keep_i64() {
    assert_eq!(call("func f() { min(3, 7) }", "f", vec![]), Value::I64(3));
    assert_eq!(call("func f() { max(3, 7) }", "f", vec![]), Value::I64(7));
    assert_eq!(call("func f() { min(-3, -7) }", "f", vec![]), Value::I64(-7));
}

#[test]
fn min_max_keep_f64() {
    assert_eq!(call("func f() { min(3.5, 7.25) }", "f", vec![]), Value::F64(3.5));
    assert_eq!(call("func f() { max(3.5, 7.25) }", "f", vec![]), Value::F64(7.25));
}

#[test]
fn declared_signature_reports_the_specialized_type() {
    let engine = Engine::new();
    let script = engine.load_str("func i() { abs(-1) } func r() { abs(-1.0) }").unwrap();
    assert_eq!(script.sigs.get("i").unwrap().ret, Ty::I64);
    assert_eq!(script.sigs.get("r").unwrap().ret, Ty::F64);
}

#[test]
fn annotated_parameter_selects_the_integer_form() {
    let engine = Engine::new();
    let script = engine.load_str("func f(x: i64) { abs(x) }").unwrap();
    assert_eq!(script.sigs.get("f").unwrap().ret, Ty::I64);
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(-9)]).unwrap(),
        Value::I64(9)
    );
}

#[test]
fn mixed_argument_types_are_rejected() {
    // No implicit widening: the language does not mix i64 and f64 in `+`
    // either, so min/max refuse it as well rather than silently picking one.
    let msg = load_err("func f() { min(1, 2.0) }");
    assert!(msg.contains("min: type mismatch: expected f64, found i64"), "{}", msg);
}

#[test]
fn non_numeric_arguments_are_rejected() {
    let msg = load_err("func f(a: str) { abs(a) }");
    assert!(msg.contains("abs: type mismatch: expected f64, found str"), "{}", msg);
}

#[test]
fn sqrt_stays_f64_only() {
    // Deliberate: sqrt of an integer has no exact i64 result, so it keeps
    // requiring f64 instead of silently truncating.
    let msg = load_err("func f() { sqrt(4) }");
    assert!(msg.contains("sqrt: type mismatch: expected f64, found i64"), "{}", msg);
}

#[test]
fn abs_of_i64_min_is_an_error_not_a_panic() {
    // `i64::MIN.abs()` panics in Rust; the script must get an error instead.
    let engine = Engine::new();
    let script = engine.load_str("func f(x: i64) { abs(x) }").unwrap();
    let err = engine
        .call(&script, "f", vec![Value::I64(i64::MIN)])
        .expect_err("abs(i64::MIN) must fail");
    assert!(err.to_string().contains("i64 abs overflow"), "{}", err);
    assert_eq!(err.span.map(|s| s.line), Some(1));
}

#[test]
fn specialization_survives_the_sbc_cache() {
    let engine = Engine::new();
    let script = engine.load_str("func f(x: i64) { max(abs(x), 3) }").unwrap();
    let bytes = engine.script_to_bytes(&script);
    // Decoding re-runs `validate`, which rejects unknown builtin ids — this
    // fails if the new ids were not registered with `builtin::name`.
    let restored = engine.script_from_bytes(&bytes).expect("cache must decode");
    assert_eq!(
        engine.call(&restored, "f", vec![Value::I64(-7)]).unwrap(),
        Value::I64(7)
    );
}

#[test]
fn wrong_arity_mentions_how_many_are_expected() {
    let msg = load_err("func f() { max(1) }");
    assert!(msg.contains("`max` expects 2 argument(s), got 1"), "{}", msg);
}

#[test]
fn integer_literal_pulls_an_inferred_parameter_to_i64() {
    // Consistent with `x + 1`: an integer literal is evidence for i64, so the
    // whole call (and the parameter) resolves that way.
    let engine = Engine::new();
    let script = engine.load_str("func f(x) { min(x, 1) }").unwrap();
    let sig = script.sigs.get("f").unwrap();
    assert_eq!(sig.params[0], Ty::I64);
    assert_eq!(sig.ret, Ty::I64);
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(7)]).unwrap(),
        Value::I64(1)
    );
}

#[test]
fn no_numeric_evidence_defaults_to_f64() {
    // Nothing in `max(a, b)` says integer, so the family stays f64 — the
    // behaviour every existing script was compiled against.
    let engine = Engine::new();
    let script = engine.load_str("func f(a, b) { max(a, b) }").unwrap();
    assert_eq!(script.sigs.get("f").unwrap().params[0], Ty::F64);
    assert_eq!(
        engine.call(&script, "f", vec![Value::F64(1.5), Value::F64(2.5)]).unwrap(),
        Value::F64(2.5)
    );
}
