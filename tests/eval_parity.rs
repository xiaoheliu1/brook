//! Parity between the reference AST interpreter (`eval`) and the bytecode VM.
//!
//! The reference interpreter used to diverge from the VM on legal scripts:
//! bool/str/handle comparisons and even string `+` fell into its float path
//! and errored with a misleading "expected f64", and host errors were
//! flattened to `runtime`. These tests run the same script through both
//! engines and pin the interpreter to the VM's semantics.

use std::collections::HashMap;

use brook::ast;
use brook::engine::HostRegistry;
use brook::eval::Interpreter;
use brook::lexer::Lexer;
use brook::parser::Parser;
use brook::typecheck::Checker;
use brook::value::Value;
use brook::{Engine, ErrorKind, Handle};

/// Run `src`'s `name` through the reference interpreter.
fn eval_call(src: &str, name: &str, args: Vec<Value>, host: &HostRegistry) -> brook::Result<Value> {
    let tokens = Lexer::new(src).tokenize()?;
    let program = Parser::new(tokens).parse()?;
    let sigs = Checker::new(&host.signatures()).check(&program)?;
    let funcs: HashMap<String, ast::FuncDecl> = program
        .funcs
        .into_iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let mut it = Interpreter::new(&funcs, host, &sigs, 1_000_000, 64);
    it.call(name, args)
}

/// Run the same script through the production VM.
fn vm_call(src: &str, name: &str, args: Vec<Value>) -> brook::Result<Value> {
    let engine = Engine::new();
    let script = engine.load_str(src)?;
    engine.call(&script, name, args)
}

fn both_agree(src: &str, expected: Value) -> brook::Result<()> {
    let host = HostRegistry::new();
    assert_eq!(eval_call(src, "f", vec![], &host)?, expected, "eval result");
    assert_eq!(vm_call(src, "f", vec![])?, expected, "vm result");
    Ok(())
}

#[test]
fn bool_equality_matches_the_vm() -> brook::Result<()> {
    both_agree("func f() { true == false }", Value::Bool(false))?;
    both_agree("func f() { true != false }", Value::Bool(true))?;
    Ok(())
}

#[test]
fn string_concat_and_compare_match_the_vm() -> brook::Result<()> {
    both_agree("func f() { \"ab\" + \"cd\" }", Value::from("abcd"))?;
    both_agree("func f() { \"abc\" < \"abd\" }", Value::Bool(true))?;
    both_agree("func f() { \"abc\" == \"abc\" }", Value::Bool(true))?;
    both_agree("func f() { \"abc\" >= \"abd\" }", Value::Bool(false))?;
    Ok(())
}

/// Handles are opaque: the script can never read the payload.
#[allow(dead_code)]
struct Order {
    id: i64,
}

#[test]
fn handle_identity_matches_the_vm() -> brook::Result<()> {
    let src = "func f(a: Order, b: Order) { a == b }";
    let h = Handle::new(Order { id: 7 });
    let args = vec![Value::Handle(h.clone()), Value::Handle(h.clone())];

    let host = HostRegistry::new();
    assert_eq!(eval_call(src, "f", args.clone(), &host)?, Value::Bool(true));
    assert_eq!(vm_call(src, "f", args)?, Value::Bool(true));
    Ok(())
}

#[test]
fn host_error_kind_is_preserved() -> brook::Result<()> {
    let src = "func f() { boom() }";
    let err = {
        let mut host = HostRegistry::new();
        host.register(
            "boom",
            vec![],
            brook::ty::Ty::I64,
            Box::new(|_| Err(brook::Error::io("disk on fire"))),
        )?;
        eval_call(src, "f", vec![], &host).unwrap_err()
    };
    assert_eq!(err.kind, ErrorKind::Io, "eval keeps the host's error kind");
    assert!(
        err.message.contains("host function `boom`"),
        "{}",
        err.message
    );
    assert!(err.message.contains("disk on fire"), "{}", err.message);

    let mut engine = Engine::new();
    engine.register_fn("boom", vec![], brook::ty::Ty::I64, |_| {
        Err(brook::Error::io("disk on fire"))
    })?;
    let script = engine.load_str(src)?;
    let vm_err = engine.call(&script, "f", vec![]).unwrap_err();
    assert_eq!(vm_err.kind, ErrorKind::Io, "vm keeps the host's error kind");
    Ok(())
}
