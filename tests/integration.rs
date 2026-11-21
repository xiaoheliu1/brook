use miniscript::engine::Engine;
use miniscript::error::Result;
use miniscript::ty::Ty;
use miniscript::value::Value;

#[test]
fn end_to_end_arithmetic_and_if() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func add(a, b) { a + b }
        func classify(n) {
            if n > 10 { "big" } else { "small" }
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "add", vec![Value::I64(2), Value::I64(3)])?, Value::I64(5));
    assert_eq!(
        engine.call(&script, "classify", vec![Value::I64(42)])?,
        Value::Str("big".into())
    );
    Ok(())
}

#[test]
fn end_to_end_recursion() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_file("examples/fib.ms")?;
    assert_eq!(engine.call(&script, "fib", vec![Value::I64(10)])?, Value::I64(55));
    Ok(())
}

#[test]
fn end_to_end_loops_and_let() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func sum_to(n) {
            let total = 0
            for i in 0..n {
                total = total + i
            }
            total
        }
        func countdown(n) {
            let x = n
            while x > 0 {
                x = x - 1
            }
            x
        }
        "#,
    )?;
    assert_eq!(engine.call(&script, "sum_to", vec![Value::I64(5)])?, Value::I64(10));
    assert_eq!(engine.call(&script, "countdown", vec![Value::I64(5)])?, Value::I64(0));
    Ok(())
}

#[test]
fn end_to_end_tuple_and_index() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func pair(x) { (x, x + 1) }
        func first(p) { p.0 }
        "#,
    )?;
    assert_eq!(
        engine.call(&script, "pair", vec![Value::I64(7)])?,
        Value::Tuple(vec![Value::I64(7), Value::I64(8)])
    );
    let p = engine.call(&script, "pair", vec![Value::I64(7)])?;
    assert_eq!(engine.call(&script, "first", vec![p])?, Value::I64(7));
    Ok(())
}

#[test]
fn end_to_end_host_function_and_handle() -> Result<()> {
    #[derive(Debug)]
    struct Order { total: f64, vip: bool }

    let mut engine = Engine::new();
    engine.register_fn(
        "order_total",
        vec![Ty::Handle("Order".into())],
        Ty::F64,
        |args| Ok(Value::F64(args[0].as_handle::<Order>()?.total)),
    )?;
    engine.register_fn(
        "is_vip",
        vec![Ty::Handle("Order".into())],
        Ty::Bool,
        |args| Ok(Value::Bool(args[0].as_handle::<Order>()?.vip)),
    )?;
    engine.register_type::<Order>()?;

    let script = engine.load_file("examples/risk.ms")?;
    let order = Value::handle(Order { total: 200.0, vip: true });
    let result = engine.call(&script, "check", vec![order, Value::F64(1000.0)])?;
    assert_eq!(
        result,
        Value::Tuple(vec![Value::F64(160.0), Value::Bool(true)])
    );
    Ok(())
}

#[test]
fn hot_reload_swaps_implementation() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("miniscript_reload_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rule.ms");
    std::fs::write(&path, "func price(x) { x * 2 }\n").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path)?;
    assert_eq!(engine.call(&script, "price", vec![Value::I64(10)])?, Value::I64(20));

    std::fs::write(&path, "func price(x) { x * 3 }\n").unwrap();
    engine.reload(&mut script, &path)?;
    assert_eq!(engine.call(&script, "price", vec![Value::I64(10)])?, Value::I64(30));

    std::fs::remove_dir_all(&dir).unwrap();
    Ok(())
}

#[test]
fn overflow_is_an_error_not_a_panic() -> Result<()> {
    let engine = Engine::new().with_limits(1_000_000, 64);
    let script = engine.load_str("func boom() { 9223372036854775807 + 1 }")?;
    let err = engine.call(&script, "boom", vec![]).unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Runtime);
    assert!(err.message.contains("overflow"));
    Ok(())
}

#[test]
fn type_conflict_is_rejected() {
    let engine = Engine::new();
    let err = engine.load_str("func bad() { let x = 1\nx = \"str\"\nx }").unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
}

// ---- phase 2 VM-specific tests ----

#[test]
fn vm_string_comparison() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func cmp(a: str, b: str) { if a < b { -1 } else if a > b { 1 } else { 0 } }
        func eq(a: str, b: str) { a == b }
        "#,
    )?;
    assert_eq!(
        engine.call(&script, "cmp", vec!["apple".into(), "banana".into()])?,
        Value::I64(-1)
    );
    assert_eq!(
        engine.call(&script, "cmp", vec!["z".into(), "a".into()])?,
        Value::I64(1)
    );
    assert_eq!(
        engine.call(&script, "eq", vec!["same".into(), "same".into()])?,
        Value::Bool(true)
    );
    Ok(())
}

#[test]
fn vm_short_circuit_avoids_division_by_zero() -> Result<()> {
    let engine = Engine::new();
    // `false && (1/0 > 0)` must NOT evaluate the right side.
    let script = engine.load_str(
        "func safe() { false && 1 / 0 > 0 }",
    )?;
    assert_eq!(engine.call(&script, "safe", vec![])?, Value::Bool(false));
    // True || rhs likewise short-circuits.
    let script2 = engine.load_str("func safe2() { true || 1 / 0 > 0 }")?;
    assert_eq!(engine.call(&script2, "safe2", vec![])?, Value::Bool(true));
    Ok(())
}

#[test]
fn vm_gas_limit_stops_infinite_loop() -> Result<()> {
    let engine = Engine::new().with_limits(5_000, 64);
    let script = engine.load_str(
        "func spin() { let x = 0\n while x >= 0 { x = x + 1 }\n x }",
    )?;
    let err = engine.call(&script, "spin", vec![]).unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Runtime);
    assert!(err.message.contains("gas") || err.message.contains("step"));
    Ok(())
}

#[test]
fn vm_inferred_type_mismatch_is_reported_not_silent() -> Result<()> {
    // Without a str seed, params default to i64. Passing str from the host
    // must produce a clear error, never a silently wrong comparison.
    let engine = Engine::new();
    let script = engine.load_str("func cmp(a, b) { if a < b { -1 } else { 1 } }")?;
    let err = engine
        .call(&script, "cmp", vec!["z".into(), "a".into()])
        .unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Runtime);
    assert!(err.message.contains("type mismatch"));
    Ok(())
}

#[test]
fn vm_nested_calls_and_builtins() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func add_one(x) { x + 1 }
        func twice(x) { add_one(add_one(x)) }
        func calc(x) { to_i64(sqrt(to_f64(twice(x)))) }
        "#,
    )?;
    // twice(8) = 10, sqrt(10) ≈ 3.16, to_i64 → 3
    assert_eq!(engine.call(&script, "calc", vec![Value::I64(8)])?, Value::I64(3));
    Ok(())
}

#[test]
fn vm_tuple_roundtrip_through_host() -> Result<()> {
    let mut engine = Engine::new();
    engine.register_fn(
        "sum_pair",
        vec![Ty::Tuple(vec![Ty::I64, Ty::I64])],
        Ty::I64,
        |args| match &args[0] {
            Value::Tuple(pair) => Ok(Value::I64(pair[0].as_i64()? + pair[1].as_i64()?)),
            _ => Err(miniscript::Error::runtime("expected tuple")),
        },
    )?;
    let script = engine.load_str("func go(a, b) { sum_pair((a, b)) }")?;
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(3), Value::I64(4)])?,
        Value::I64(7)
    );
    Ok(())
}

// ---- phase 3: safety boundaries ----

#[test]
fn panicking_host_fn_returns_error_instead_of_unwinding() -> Result<()> {
    let mut engine = Engine::new();
    engine.register_fn("boom", vec![], Ty::I64, |_| panic!("host callback exploded"))?;
    let script = engine.load_str("func go() { boom() }")?;

    // The panic must be caught at the call boundary and surfaced as an Err —
    // never propagated into the host's stack.
    let err = engine.call(&script, "go", vec![]).unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Runtime);
    assert!(err.message.contains("internal panic"), "got: {}", err.message);
    Ok(())
}

#[test]
fn division_and_modulo_by_zero_are_runtime_errors() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func div_i() { 1 / 0 }
        func mod_i() { 1 % 0 }
        func div_f() { 1.0 / 0.0 }
        func mod_f() { 1.0 % 0.0 }
        "#,
    )?;
    for name in ["div_i", "mod_i", "div_f", "mod_f"] {
        let err = engine.call(&script, name, vec![]).unwrap_err();
        assert_eq!(err.kind, miniscript::ErrorKind::Runtime, "{}: {:?}", name, err);
        assert!(err.message.contains("by zero"), "{}: {}", name, err.message);
    }
    Ok(())
}

#[test]
fn max_depth_stops_deep_recursion() -> Result<()> {
    let engine = Engine::new().with_limits(1_000_000, 8);
    let script = engine.load_str(
        r#"
        func recurse(n) {
            if n <= 0 { 0 } else { recurse(n - 1) }
        }
        "#,
    )?;
    let err = engine.call(&script, "recurse", vec![Value::I64(100)]).unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Runtime);
    assert!(err.message.contains("call depth limit"), "got: {}", err.message);
    Ok(())
}

#[test]
fn heap_quota_charges_empty_heap_objects() -> Result<()> {
    // Payload-only accounting let scripts allocate unlimited zero-length
    // strings/tuples under any heap limit. Each heap entry must have a floor.
    let engine = Engine::new().with_limits(1_000_000, 64).with_heap_limit(1);
    let script = engine.load_str(
        r#"
        func f(n) {
            let i = 0
            let s = ""
            while i < n {
                s = substr(s, 0, 0)
                i = i + 1
            }
            s
        }
        "#,
    )?;
    let err = engine.call(&script, "f", vec![Value::I64(2)]).unwrap_err();
    assert!(
        err.message.contains("memory limit exceeded"),
        "got: {}",
        err.message
    );
    Ok(())
}

#[test]
fn heap_quota_stops_runaway_allocation() -> Result<()> {
    // 1 KiB budget; each iteration builds a new 512-byte string, so the third
    // allocation must be rejected rather than growing without bound.
    let engine = Engine::new().with_heap_limit(1024);
    let script = engine.load_str(
        r#"
        func pad(s, n) {
            let out = s
            for i in 0..n {
                out = out + s
            }
            out
        }
        func grow() {
            let base = "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789"
            pad(base, 20)
        }
        "#,
    )?;
    let err = engine.call(&script, "grow", vec![]).unwrap_err();
    assert!(
        err.message.contains("memory limit exceeded"),
        "got: {}",
        err.message
    );
    Ok(())
}

#[test]
fn too_many_arguments_is_a_compile_error() {
    let engine = Engine::new();
    // Nine operands exceed the inline operand encoding (MAX_OPERANDS = 8) at
    // the *call site*; the declaration alone is fine.
    let err = engine
        .load_str(
            "func sum9(a,b,c,d,e,g,h,i,j) { a }
             func go() { sum9(1,2,3,4,5,6,7,8,9) }",
        )
        .unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
}

#[test]
fn string_concat_is_not_numeric_addition() -> Result<()> {
    // Regression: `s1 + s2` used to be lowered to an integer add over heap
    // indices, silently returning a wrong string. Concat must produce the
    // actual joined text.
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func join(a: str, b: str) { a + b }
        func twice(s: str) { s + s }
        "#,
    )?;
    assert_eq!(
        engine.call(&script, "join", vec![Value::from("foo"), Value::from("bar")])?,
        Value::from("foobar")
    );
    assert_eq!(
        engine.call(&script, "twice", vec![Value::from("ab")])?,
        Value::from("abab")
    );
    Ok(())
}

#[test]
fn to_str_accepts_scalars_and_rejects_heap_values() -> Result<()> {
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func scalars() { to_str(1) + to_str(2.5) + to_str(true) + to_str("x") }
        "#,
    )?;
    assert_eq!(
        engine.call(&script, "scalars", vec![])?,
        Value::from("12.5truex")
    );

    let err = engine.load_str("func bad() { to_str((1, 2)) }").unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
    assert!(err.message.contains("to_str"), "got: {}", err.message);

    let err = engine
        .load_str("func noop() { let x = 1 }\nfunc bad() { to_str(noop()) }")
        .unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
    assert!(err.message.contains("to_str"), "got: {}", err.message);
    Ok(())
}

#[test]
fn tuple_equality_is_a_compile_error() {
    let engine = Engine::new();
    let err = engine
        .load_str("func f() { (1, 2) == (1, 2) }")
        .unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
    assert!(err.message.contains("equality comparison"), "got: {}", err.message);
}

#[test]
fn unit_equality_is_a_compile_error() {
    let engine = Engine::new();
    let err = engine
        .load_str("func noop() { let x = 1 }\nfunc f() { noop() == noop() }")
        .unwrap_err();
    assert_eq!(err.kind, miniscript::ErrorKind::Type);
    assert!(err.message.contains("equality comparison"), "got: {}", err.message);
}

#[test]
fn divergent_if_branch_keeps_the_value_type() -> Result<()> {
    // Regression: a `return` in the then-branch made codegen report Unit,
    // so `x + 1.0` was emitted as an integer add over an F64 bit pattern.
    let engine = Engine::new();
    let script = engine.load_str(
        r#"
        func early(c: bool) -> f64 {
            let x = if c { return 0.0 } else { 2.0 }
            return x + 1.0
        }
        "#,
    )?;
    assert_eq!(
        engine.call(&script, "early", vec![Value::Bool(true)])?,
        Value::F64(0.0)
    );
    assert_eq!(
        engine.call(&script, "early", vec![Value::Bool(false)])?,
        Value::F64(3.0)
    );
    Ok(())
}
