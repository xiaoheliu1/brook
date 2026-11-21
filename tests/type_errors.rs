//! Type errors must name the *expected* type first and the *actual* type
//! second: `expected <expected>, found <actual>`.
//!
//! This is easy to get backwards because `unify(a, b)` is symmetric — swapping
//! the arguments changes nothing but the error text. These tests pin the
//! direction for every place that reports a mismatch, so a future refactor
//! cannot quietly flip it again.

use brook::{Engine, ErrorKind};

fn type_err(src: &str) -> String {
    let engine = Engine::new();
    match engine.load_str(src) {
        Ok(_) => panic!("expected a compile error for: {}", src),
        Err(e) => {
            assert_eq!(e.kind, ErrorKind::Type, "expected a type error, got: {}", e);
            e.message
        }
    }
}

fn assert_mismatch(msg: &str, expected: &str, found: &str) {
    let want = format!("expected {}, found {}", expected, found);
    assert!(
        msg.contains(&want),
        "expected the message to contain `{}`, got: {}",
        want,
        msg
    );
}

#[test]
fn let_annotation_reports_expected_first() {
    let msg = type_err(r#"func f() { let x: i64 = "a" }"#);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn assignment_reports_expected_first() {
    let msg = type_err(r#"func f() { let x = 1  x = "a" }"#);
    assert!(msg.contains("assignment to `x`"), "got: {}", msg);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn for_range_reports_expected_first() {
    let msg = type_err(r#"func f() { for i in "a"..3 { } }"#);
    assert!(msg.contains("for range start"), "got: {}", msg);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn function_return_reports_expected_first() {
    let msg = type_err(r#"func f() -> i64 { "a" }"#);
    assert!(msg.contains("function `f`"), "got: {}", msg);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn return_statement_reports_expected_first() {
    let msg = type_err(r#"func f() -> i64 { return "a" }"#);
    assert!(msg.contains("return"), "got: {}", msg);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn script_call_arguments_report_expected_first() {
    let msg = type_err(
        r#"
func g(n: i64) -> i64 { n }
func f() { g("a") }
"#,
    );
    assert!(msg.contains("g:"), "got: {}", msg);
    assert_mismatch(&msg, "i64", "str");
}

#[test]
fn builtin_arguments_report_expected_first() {
    let msg = type_err(r#"func f() { len(1) }"#);
    assert!(msg.contains("len"), "got: {}", msg);
    assert_mismatch(&msg, "str", "i64");

    let msg2 = type_err(r#"func f() { to_i64(true) }"#);
    assert!(msg2.contains("to_i64"), "got: {}", msg2);
    assert_mismatch(&msg2, "f64", "bool");
}
