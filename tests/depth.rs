//! Nesting-depth guard.
//!
//! Parsing, type checking, codegen and even dropping the AST are all recursive,
//! so a deeply nested source used to abort the process with a stack overflow
//! (`STATUS_STACK_OVERFLOW`, uncatchable). The parser now rejects anything
//! deeper than `MAX_NESTING`, and these tests pin both sides of that boundary.
//!
//! Every case here is sized well past the limit on purpose: before the guard a
//! single one of them took the whole test binary down.

use vitul::parser::MAX_NESTING;
use vitul::Engine;

fn err_of(src: &str) -> String {
    match Engine::new().load_str(src) {
        Ok(_) => panic!("expected a nesting error, but the source compiled"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn deeply_nested_parentheses_are_rejected() {
    let src = format!("func f(x) {{ {}x{} }}", "(".repeat(2_000), ")".repeat(2_000));
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
    // The guard reports the stage it belongs to, so callers can distinguish a
    // too-deep script from a type error.
    assert!(msg.starts_with("[parse] error"), "{}", msg);
}

#[test]
fn long_operator_chain_is_rejected() {
    // Left-associative chains build a left-deep tree from an iterative parser
    // loop, so this is the case the old parser happily produced and the type
    // checker then blew the stack on.
    let src = format!("func f() {{ {}1 }}", "1+".repeat(20_000));
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn long_unary_chain_is_rejected() {
    let src = format!("func f(x) {{ {}x }}", "-".repeat(20_000));
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn deeply_nested_blocks_are_rejected() {
    let src = format!("func f() {{ {}{} }}", "{".repeat(2_000), "}".repeat(2_000));
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn deeply_nested_if_is_rejected() {
    let src = format!(
        "func f(x) {{ {}x{} }}",
        "if x { ".repeat(1_000),
        " }".repeat(1_000)
    );
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn long_else_if_ladder_is_rejected() {
    // `else if` recurses without a block, so it needs its own guard.
    let mut src = String::from("func f(x) { if x { return 1 }");
    for _ in 0..1_000 {
        src.push_str(" else if x { return 2 }");
    }
    src.push_str(" }");
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn long_else_if_expression_ladder_is_rejected() {
    // The if-EXPRESSION form recurses through `parse_primary`, not
    // `parse_if_stmt`, so it needs its own depth entry. This path used to
    // build an unbounded recursion that overflowed the stack even though
    // the statement ladder above was already guarded.
    let mut src = String::from("func f(x) { let v = if x { 1 }");
    for _ in 0..1_000 {
        src.push_str(" else if x { 1 }");
    }
    src.push_str(" else { 2 }\n    v }");
    let msg = err_of(&src);
    assert!(msg.contains("nesting too deep"), "{}", msg);
}

#[test]
fn everyday_nesting_still_compiles() {
    let engine = Engine::new();

    // 60 levels of parentheses around a value.
    let src = format!(
        "func f(x) {{ {}x{} }}",
        "(".repeat(60),
        ")".repeat(60)
    );
    let script = engine.load_str(&src).expect("60 levels must be accepted");
    assert_eq!(
        engine.call(&script, "f", vec![vitul::Value::I64(7)]).unwrap(),
        vitul::Value::I64(7)
    );

    // 60-term sum, evaluated to make sure the folding is still correct.
    let src = format!("func f() {{ {}1 }}", "1+".repeat(60));
    let script = engine.load_str(&src).expect("60-term chain must be accepted");
    assert_eq!(
        engine.call(&script, "f", vec![]).unwrap(),
        vitul::Value::I64(61)
    );

    // 20 nested ifs: readable code, nowhere near the limit.
    let src = format!(
        "func f(x) {{ {}x{} }}",
        "if x { ".repeat(20),
        " }".repeat(20)
    );
    engine.load_str(&src).expect("20 nested ifs must be accepted");

    // 20-level else-if chain as an *expression*: the guard counts every
    // if-expression (this path used to be unguarded; see the regression
    // test above), and 20 must stay far below the limit.
    let mut src = String::from("func f(x: bool) { let v = if x { 1 }");
    for _ in 0..20 {
        src.push_str(" else if x { 1 }");
    }
    src.push_str(" else { 2 }\n    v }");
    let script = engine
        .load_str(&src)
        .expect("20-level if-expression ladder must be accepted");
    assert_eq!(
        engine.call(&script, "f", vec![vitul::Value::Bool(false)]).unwrap(),
        vitul::Value::I64(2)
    );
}

#[test]
fn limit_is_reported_with_a_source_location() {
    let src = format!("func f(x) {{ {}x{} }}", "(".repeat(MAX_NESTING + 5), ")".repeat(MAX_NESTING + 5));
    let err = Engine::new().load_str(&src).unwrap_err();
    let span = err.span.expect("parser errors carry a span");
    assert_eq!(span.line, 1);
    assert!(span.col > 1, "column should point into the expression, got {}", span.col);
    // `report` must render it without needing anything else from the caller.
    let rendered = err.report(&src, "deep.vt");
    assert!(rendered.contains("deep.vt:1:"), "{}", rendered);
    assert!(rendered.contains("nesting too deep"), "{}", rendered);
}
