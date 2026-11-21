//! Operator semantics: logic (`&&` / `||` / `!`), precedence, short-circuiting,
//! and the wording of type errors. Plus `//` and `/* */` comments at the
//! script level (the lexer has its own unit tests for the token level).

use miniscript::{Engine, ErrorKind, Value};

fn eval(src: &str) -> Value {
    let engine = Engine::new();
    let script = engine.load_str(src).expect("script should compile");
    engine.call(&script, "f", vec![]).expect("script should run")
}

fn compile_err(src: &str) -> String {
    let engine = Engine::new();
    match engine.load_str(src) {
        Ok(_) => panic!("expected a compile error for: {}", src),
        Err(e) => {
            assert_eq!(e.kind, ErrorKind::Type, "expected a type error, got: {}", e);
            e.message
        }
    }
}

// ---- truth tables ----

#[test]
fn and_or_truth_tables() {
    assert_eq!(eval("func f() { true && true }"), Value::Bool(true));
    assert_eq!(eval("func f() { true && false }"), Value::Bool(false));
    assert_eq!(eval("func f() { false && true }"), Value::Bool(false));
    assert_eq!(eval("func f() { false && false }"), Value::Bool(false));

    assert_eq!(eval("func f() { true || true }"), Value::Bool(true));
    assert_eq!(eval("func f() { true || false }"), Value::Bool(true));
    assert_eq!(eval("func f() { false || true }"), Value::Bool(true));
    assert_eq!(eval("func f() { false || false }"), Value::Bool(false));
}

#[test]
fn not_operator() {
    assert_eq!(eval("func f() { !true }"), Value::Bool(false));
    assert_eq!(eval("func f() { !false }"), Value::Bool(true));
    assert_eq!(eval("func f() { !!true }"), Value::Bool(true));
    assert_eq!(eval("func f() { !(1 < 2) }"), Value::Bool(false));
}

// ---- short circuit ----

/// The right-hand side must not be evaluated when the left already decides the
/// result. A division by zero is the probe: if the RHS ran, this would fail at
/// runtime instead of returning `false` / `true`.
#[test]
fn and_short_circuits_on_false() {
    assert_eq!(eval("func f() { false && (1 / 0 == 1) }"), Value::Bool(false));
}

#[test]
fn or_short_circuits_on_true() {
    assert_eq!(eval("func f() { true || (1 / 0 == 1) }"), Value::Bool(true));
}

/// Control group for the two tests above: the probe really does fail when it
/// is actually evaluated. Without this, a probe that never runs would make the
/// short-circuit tests pass vacuously.
#[test]
fn probe_expression_really_fails_when_evaluated() {
    let engine = Engine::new();
    let script = engine.load_str("func f() { true && (1 / 0 == 1) }").unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert!(
        err.message.contains("division by zero"),
        "got: {}",
        err.message
    );
}

// ---- precedence ----

#[test]
fn and_binds_tighter_than_or() {
    // Parsed as `true || (false && false)`, not `(true || false) && false`.
    assert_eq!(eval("func f() { true || false && false }"), Value::Bool(true));
    assert_eq!(eval("func f() { false && false || true }"), Value::Bool(true));
}

#[test]
fn comparison_binds_tighter_than_logic() {
    assert_eq!(eval("func f() { 1 < 2 && 3 > 2 }"), Value::Bool(true));
    assert_eq!(eval("func f() { 1 > 2 || 3 > 2 }"), Value::Bool(true));
    assert_eq!(
        eval("func f() { 1 + 1 == 2 && 2 * 3 == 6 }"),
        Value::Bool(true)
    );
}

#[test]
fn logic_is_left_associative() {
    // `a && b && c` == `(a && b) && c`; all four orders agree.
    assert_eq!(eval("func f() { true && true && false }"), Value::Bool(false));
    assert_eq!(
        eval("func f() { false || false || true }"),
        Value::Bool(true)
    );
}

// ---- type errors: wording must name the right side ----

#[test]
fn logic_requires_bool_on_both_sides() {
    let left = compile_err("func f() { 1 && true }");
    assert!(left.contains("left operand"), "got: {}", left);
    assert!(left.contains("expected bool"), "got: {}", left);
    assert!(left.contains("found i64"), "got: {}", left);

    let right = compile_err("func f() { true && 1 }");
    assert!(right.contains("right operand"), "got: {}", right);
    assert!(right.contains("expected bool"), "got: {}", right);
    assert!(right.contains("found i64"), "got: {}", right);

    let str_side = compile_err("func f() { \"x\" || false }");
    assert!(str_side.contains("expected bool"), "got: {}", str_side);
}

#[test]
fn not_requires_bool() {
    let msg = compile_err("func f() { !1 }");
    assert!(msg.contains("unary `!`"), "got: {}", msg);
    assert!(msg.contains("expected bool"), "got: {}", msg);
    assert!(msg.contains("found i64"), "got: {}", msg);
}

/// No truthiness: values are never coerced to bool implicitly.
#[test]
fn no_implicit_truthiness() {
    assert!(compile_err("func f() { if 1 { 2 } }").contains("expected bool"));
    assert!(compile_err("func f() { true || \"x\" }").contains("expected bool"));
}

// ---- comments at script level ----

#[test]
fn line_comments_work_inside_scripts() {
    let engine = Engine::new();
    let script = engine
        .load_str(
            r#"
// file header
func f(n: i64) -> i64 {
    let x = n + 1   // trailing
    // between statements
    x * 2
}
"#,
        )
        .unwrap();
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(1)]).unwrap(),
        Value::I64(4)
    );
}

#[test]
fn block_comments_work_inside_scripts() {
    let engine = Engine::new();
    let script = engine
        .load_str(
            r#"
/*
 * Multi-line header with unicode 中文 and symbols { } " '
 */
func f(n: i64) -> i64 {
    let x = n /* inline */ + 1
    /* a comment
       spanning lines */
    x * 2
}
"#,
        )
        .unwrap();
    assert_eq!(
        engine.call(&script, "f", vec![Value::I64(2)]).unwrap(),
        Value::I64(6)
    );
}

#[test]
fn block_comment_cannot_swallow_the_next_statement() {
    // The comment eats a newline, so the lexer must re-emit one; otherwise
    // these two `let`s would be glued into a single invalid line.
    let engine = Engine::new();
    let script = engine
        .load_str(
            r#"
func f() -> i64 {
    let a = 1 /* one
    two */ let b = 2
    a + b
}
"#,
        )
        .unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(3));
}

#[test]
fn double_slash_in_url_is_not_a_comment() {
    assert_eq!(
        eval(r#"func f() { "http://example.com/a" }"#),
        Value::from("http://example.com/a")
    );
}

#[test]
fn commented_out_code_does_not_run() {
    let engine = Engine::new();
    let script = engine
        .load_str(
            r#"
func f() -> i64 {
    // break
    // let x = 1 / 0
    /* let y = 1 / 0 */
    7
}
"#,
        )
        .unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(7));
}
