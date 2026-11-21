//! Q15 semantic warts, closed: reserved built-in names and `()` as Unit.
//!
//! * A script function named like a built-in used to compile, but every
//!   internal call hit the built-in implementation — only entry-point calls
//!   reached the user's function. It is now rejected like the host-name
//!   collision it mirrors.
//! * The `()` literal used to produce `Ty::Tuple([])`: it *rendered* as `()`
//!   but did not unify with `Ty::Unit`. The literal, the type annotation and
//!   the reference interpreter now all agree on Unit.

use brook::{Engine, ErrorKind, Value};

fn compile_err(src: &str) -> String {
    match Engine::new().load_str(src) {
        Ok(_) => panic!("expected a compile error for: {}", src),
        Err(e) => {
            assert_eq!(e.kind, ErrorKind::Type, "got: {}", e);
            e.message
        }
    }
}

#[test]
fn script_functions_cannot_shadow_builtins() {
    for name in ["abs", "min", "max", "sqrt", "floor", "pow", "len", "contains", "substr", "to_str", "to_i64", "to_f64"] {
        let src = format!("func {name}(x) {{ x }}");
        let msg = compile_err(&src);
        assert!(msg.contains("reserved built-in"), "{name}: {msg}");
        assert!(msg.contains(name), "{name}: {msg}");
    }

    // The builtins themselves remain callable. (Note `abs(x)` with no integer
    // evidence defaults to the f64 family — Q7 semantics — so the two cases
    // keep their types consistent.)
    let engine = Engine::new();
    let script = engine
        .load_str("func f(x) { abs(x) }\nfunc g(s: str) { len(s) + max(1, 2) }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "f", vec![Value::F64(-3.5)]).unwrap(),
        Value::F64(3.5)
    );
    assert_eq!(
        engine.call(&script, "g", vec![Value::from("abcd")]).unwrap(),
        Value::I64(6)
    );
}

#[test]
fn host_name_collision_message_is_distinct() {
    // Contrast with the builtin wording: the host collision predates Q15 and
    // keeps its own message.
    let mut engine = Engine::new();
    engine
        .register_fn("total", vec![], brook::ty::Ty::I64, |_| Ok(Value::I64(0)))
        .unwrap();
    let err = engine.load_str("func total() { 1 }").unwrap_err();
    assert!(err.message.contains("already defined by the host"), "got: {}", err.message);
}

#[test]
fn unit_literal_is_unit_not_a_zero_tuple() {
    let engine = Engine::new();

    // Trailing `()` is the unit result.
    let script = engine.load_str("func f() { () }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::Unit);

    // A unit binding cannot enter arithmetic (the old Tuple([]) rendering
    // would have shown `()` here too, but via the tuple type).
    let msg = compile_err("func f() { let x = ()\n x + 1 }");
    assert!(msg.contains("()"), "got: {msg}");

    // Equality on Unit is rejected exactly like tuple equality.
    let msg = compile_err("func f() { () == () }");
    assert!(msg.contains("equality"), "got: {msg}");

    // The type annotation `()` is the unit type.
    let script = engine.load_str("func f(x: ()) { x }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![Value::Unit]).unwrap(), Value::Unit);
}
