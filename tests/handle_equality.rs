//! Opaque-handle equality is object identity, not heap-slot identity.
//!
//! Regression: `Op::EqHandle` compared VM heap indices. Every handle
//! argument — and every host-call result — occupies its own heap entry, so
//! the *same* host object arriving through two slots (two arguments, or an
//! extra copy inside the script) compared unequal. `Handle`'s `PartialEq`
//! (`Rc` pointer identity plus the registered type name) is the authority;
//! the VM now defers to it.

use vitul::{Engine, Handle, Value};

/// Handles are opaque: the script can never read the payload, so the field
/// is write-only by design.
#[allow(dead_code)]
struct Order {
    id: i64,
}

#[test]
fn same_object_in_two_arguments_compares_equal() {
    let engine = Engine::new();
    let script = engine
        .load_str("func same(a: Order, b: Order) { a == b }")
        .unwrap();
    let h = Handle::new(Order { id: 7 });
    let r = engine
        .call(
            &script,
            "same",
            vec![Value::Handle(h.clone()), Value::Handle(h.clone())],
        )
        .unwrap();
    assert_eq!(r, Value::Bool(true));
}

#[test]
fn same_object_in_two_arguments_is_not_unequal() {
    let engine = Engine::new();
    let script = engine
        .load_str("func diff(a: Order, b: Order) { a != b }")
        .unwrap();
    let h = Handle::new(Order { id: 7 });
    let r = engine
        .call(
            &script,
            "diff",
            vec![Value::Handle(h.clone()), Value::Handle(h.clone())],
        )
        .unwrap();
    assert_eq!(r, Value::Bool(false));
}

#[test]
fn distinct_objects_compare_unequal() {
    let engine = Engine::new();
    let script = engine
        .load_str("func same(a: Order, b: Order) { a == b }")
        .unwrap();
    let r = engine
        .call(
            &script,
            "same",
            vec![
                Value::Handle(Handle::new(Order { id: 1 })),
                Value::Handle(Handle::new(Order { id: 2 })),
            ],
        )
        .unwrap();
    assert_eq!(r, Value::Bool(false));
}

#[test]
fn identity_survives_a_copy_inside_the_script() {
    // `let copy = a` binds the same register; identity must hold across it.
    let engine = Engine::new();
    let script = engine
        .load_str("func wrap(a: Order) {\n    let copy = a\n    copy == a\n}")
        .unwrap();
    let r = engine
        .call(
            &script,
            "wrap",
            vec![Value::Handle(Handle::new(Order { id: 3 }))],
        )
        .unwrap();
    assert_eq!(r, Value::Bool(true));
}
