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
    both_agree_with_args(src, "f", vec![], expected)
}

fn both_agree_with_args(
    src: &str,
    name: &str,
    args: Vec<Value>,
    expected: Value,
) -> brook::Result<()> {
    let host = HostRegistry::new();
    assert_eq!(
        eval_call(src, name, args.clone(), &host)?,
        expected,
        "eval result"
    );
    assert_eq!(vm_call(src, name, args)?, expected, "vm result");
    Ok(())
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn int_expr(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.below(4) == 0 {
        return match rng.below(3) {
            0 => "n".into(),
            1 => (rng.below(11) as i64 - 5).to_string(),
            _ => "0".into(),
        };
    }
    match rng.below(6) {
        0 => format!("-({})", int_expr(rng, depth - 1)),
        1 => format!(
            "({}) + ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        2 => format!(
            "({}) - ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        3 => format!(
            "({}) * ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        4 => format!(
            "({}) / ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        _ => format!(
            "if {} {{ {} }} else {{ {} }}",
            bool_expr(rng, depth - 1),
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1),
        ),
    }
}

fn bool_expr(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.below(4) == 0 {
        return match rng.below(3) {
            0 => "b".into(),
            1 => "true".into(),
            _ => "false".into(),
        };
    }
    match rng.below(6) {
        0 => format!("!({})", bool_expr(rng, depth - 1)),
        1 => format!(
            "({}) && ({})",
            bool_expr(rng, depth - 1),
            bool_expr(rng, depth - 1)
        ),
        2 => format!(
            "({}) || ({})",
            bool_expr(rng, depth - 1),
            bool_expr(rng, depth - 1)
        ),
        3 => format!(
            "({}) == ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        4 => format!(
            "({}) < ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
        _ => format!(
            "({}) != ({})",
            int_expr(rng, depth - 1),
            int_expr(rng, depth - 1)
        ),
    }
}

fn assert_same_outcome(
    src: &str,
    args: &[Value],
    eval: brook::Result<Value>,
    vm: brook::Result<Value>,
    cached: brook::Result<Value>,
) {
    match (eval, vm, cached) {
        (Ok(eval), Ok(vm), Ok(cached)) => {
            assert_eq!(eval, vm, "eval/VM divergence for {src}");
            assert_eq!(eval, cached, "cache divergence for {src}");
        }
        (Err(_), Err(_), Err(_)) => {}
        (eval, vm, cached) => panic!(
            "outcome divergence for {src} with {args:?}: eval={eval:?}, vm={vm:?}, cached={cached:?}"
        ),
    }
}

#[test]
fn generated_expressions_match_eval_and_cache_roundtrip() -> brook::Result<()> {
    let mut rng = Rng(0x5EED_1234_9876_ABCD);
    let host = HostRegistry::new();
    let engine = Engine::new();
    for case in 0..300 {
        let expr = if case % 2 == 0 {
            int_expr(&mut rng, 4)
        } else {
            bool_expr(&mut rng, 4)
        };
        let src = format!("func f(n: i64, b: bool) {{ {expr} }}");
        let script = engine.load_str(&src)?;
        let cached = engine.script_from_bytes(&engine.script_to_bytes(&script))?;
        for (n, b) in [(0, false), (1, true), (-2, false), (7, true)] {
            let args = vec![Value::I64(n), Value::Bool(b)];
            assert_same_outcome(
                &src,
                &args,
                eval_call(&src, "f", args.clone(), &host),
                engine.call(&script, "f", args.clone()),
                engine.call(&cached, "f", args.clone()),
            );
        }
    }
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
fn named_boolean_condition_keeps_its_initializer() -> brook::Result<()> {
    both_agree(
        "func f() { let c = 1 < 2\n if c { 0 } else { 1 }\n c }",
        Value::Bool(true),
    )
}

#[test]
fn short_circuit_assignment_reads_the_old_target() -> brook::Result<()> {
    both_agree_with_args(
        "func f(a: bool, b: bool) { let x = a\n x = b && x\n x }",
        "f",
        vec![Value::Bool(false), Value::Bool(true)],
        Value::Bool(false),
    )?;
    both_agree_with_args(
        "func f(a: bool, b: bool) { let x = a\n x = b || x\n x }",
        "f",
        vec![Value::Bool(true), Value::Bool(false)],
        Value::Bool(true),
    )
}

#[test]
fn range_end_is_snapshotted_before_iteration() -> brook::Result<()> {
    both_agree(
        "func f() { let end = 3\n let sum = 0\n for i in 0..end { sum = sum + i\n end = 1 }\n sum }",
        Value::I64(3),
    )
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
