//! Phase 3: `.sbc` bytecode cache — roundtrip, staleness, registry pinning,
//! and corruption handling.

use miniscript::cache::{self, HostFingerprint};
use miniscript::ir::{Function, Instr, Op, Program, Regs};
use miniscript::ty::Ty;
use miniscript::value::Value;
use miniscript::{register_fn, Engine, Script};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

const SRC: &str = r#"
func sum_to(n) {
    let total = 0
    for i in 0..n { total = total + i }
    total
}
func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }
func calc(x) { to_i64(sqrt(to_f64(x))) }
func text(a: str, b: str) { a + b }
func tuple_go(a, b) {
    let p = (a, b + 1)
    p.0 + p.1
}
func branch(x, y) { if x == y { 1 } else if x > y { 2 } else { 3 } }
func logic(a, b) { (a && !b) || (b && 1 / 2 == 0) }
func fmath(x) { min(max(x, 1.0), 9.0) * 0.5 }
func strcmp(a: str, b: str) { if a < b { "lt" } else { "ge" } }
func host(x) { scale(x) }
func unit() { }
"#;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("miniscript_sbc_{}_{}", std::process::id(), name));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn script_with_host() -> miniscript::Result<(Engine, Script)> {
    let mut engine = Engine::new();
    register_fn!(engine, "scale", (i64) -> i64, |x| x * 3)?;
    let script = engine.load_str(SRC)?;
    Ok((engine, script))
}

#[test]
fn sbc_roundtrip_preserves_program_and_behavior() -> miniscript::Result<()> {
    let (engine, script) = script_with_host()?;
    let path = temp_dir("roundtrip").join("prog.sbc");
    engine.save_sbc(&script, &path)?;
    let loaded = engine.load_sbc(&path)?;

    assert_eq!(format!("{:?}", script.code.funcs), format!("{:?}", loaded.code.funcs));
    assert_eq!(script.code.strings, loaded.code.strings);
    assert_eq!(script.code.ids, loaded.code.ids);
    assert_eq!(script.sigs.len(), loaded.sigs.len());
    assert_eq!(script.source_hash, loaded.source_hash);
    assert_eq!(script.name, loaded.name);

    let cases: Vec<(&str, Vec<Value>)> = vec![
        ("sum_to", vec![Value::I64(10)]),
        ("fib", vec![Value::I64(10)]),
        ("calc", vec![Value::I64(8)]),
        ("text", vec![Value::from("a"), Value::from("b")]),
        ("tuple_go", vec![Value::I64(3), Value::I64(4)]),
        ("branch", vec![Value::I64(3), Value::I64(3)]),
        ("logic", vec![Value::Bool(true), Value::Bool(false)]),
        ("fmath", vec![Value::F64(5.0)]),
        ("strcmp", vec![Value::from("a"), Value::from("b")]),
        ("host", vec![Value::I64(7)]),
    ];
    for (name, args) in cases {
        let a = engine.call(&script, name, args.clone())?;
        let b = engine.call(&loaded, name, args)?;
        assert_eq!(a, b, "mismatch for {name}");
    }
    Ok(())
}

/// Every opcode in the instruction set must survive encode -> decode with its
/// fields intact. This is the guard against a mistyped tag in the opcode table.
#[test]
fn every_opcode_roundtrips_exactly() -> miniscript::Result<()> {
    let three = || Regs::new(&[1, 2, 3]);
    let ops: Vec<Op> = vec![
        Op::ConstI64 { dst: 1, val: -42 },
        Op::ConstF64 { dst: 1, val: 3.25 },
        Op::ConstBool { dst: 1, val: true },
        Op::ConstStr { dst: 1, id: 0 },
        Op::ConstUnit { dst: 1 },
        Op::Mov { dst: 1, src: 2 },
        Op::AddI64 { dst: 1, a: 2, b: 3 },
        Op::SubI64 { dst: 1, a: 2, b: 3 },
        Op::MulI64 { dst: 1, a: 2, b: 3 },
        Op::DivI64 { dst: 1, a: 2, b: 3 },
        Op::ModI64 { dst: 1, a: 2, b: 3 },
        Op::NegI64 { dst: 1, src: 2 },
        Op::AddI64Imm { dst: 1, src: 2, imm: -7 },
        Op::AddF64 { dst: 1, a: 2, b: 3 },
        Op::SubF64 { dst: 1, a: 2, b: 3 },
        Op::MulF64 { dst: 1, a: 2, b: 3 },
        Op::DivF64 { dst: 1, a: 2, b: 3 },
        Op::ModF64 { dst: 1, a: 2, b: 3 },
        Op::NegF64 { dst: 1, src: 2 },
        Op::EqI64 { dst: 1, a: 2, b: 3 },
        Op::NeI64 { dst: 1, a: 2, b: 3 },
        Op::LtI64 { dst: 1, a: 2, b: 3 },
        Op::LeI64 { dst: 1, a: 2, b: 3 },
        Op::GtI64 { dst: 1, a: 2, b: 3 },
        Op::GeI64 { dst: 1, a: 2, b: 3 },
        Op::EqF64 { dst: 1, a: 2, b: 3 },
        Op::NeF64 { dst: 1, a: 2, b: 3 },
        Op::LtF64 { dst: 1, a: 2, b: 3 },
        Op::LeF64 { dst: 1, a: 2, b: 3 },
        Op::GtF64 { dst: 1, a: 2, b: 3 },
        Op::GeF64 { dst: 1, a: 2, b: 3 },
        Op::EqBool { dst: 1, a: 2, b: 3 },
        Op::NeBool { dst: 1, a: 2, b: 3 },
        Op::EqStr { dst: 1, a: 2, b: 3 },
        Op::NeStr { dst: 1, a: 2, b: 3 },
        Op::LtStr { dst: 1, a: 2, b: 3 },
        Op::LeStr { dst: 1, a: 2, b: 3 },
        Op::GtStr { dst: 1, a: 2, b: 3 },
        Op::GeStr { dst: 1, a: 2, b: 3 },
        Op::EqHandle { dst: 1, a: 2, b: 3 },
        Op::NeHandle { dst: 1, a: 2, b: 3 },
        Op::Not { dst: 1, src: 2 },
        Op::AndBool { dst: 1, a: 2, b: 3 },
        Op::OrBool { dst: 1, a: 2, b: 3 },
        Op::Concat { dst: 1, a: 2, b: 3 },
        Op::Jump { target: 3 },
        Op::JumpIfFalse { cond: 1, target: 3 },
        Op::JmpIfEqI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfNeI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfLtI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfLeI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfGtI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfGeI64 { a: 1, b: 2, target: 3 },
        Op::JmpIfEqF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfNeF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfLtF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfLeF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfGtF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfGeF64 { a: 1, b: 2, target: 3 },
        Op::JmpIfEqI64Imm { a: 1, imm: -7, target: 3 },
        Op::JmpIfNeI64Imm { a: 1, imm: -7, target: 3 },
        Op::JmpIfLtI64Imm { a: 1, imm: -7, target: 3 },
        Op::JmpIfLeI64Imm { a: 1, imm: -7, target: 3 },
        Op::JmpIfGtI64Imm { a: 1, imm: -7, target: 3 },
        Op::JmpIfGeI64Imm { a: 1, imm: -7, target: 3 },
        Op::CallScript { func: 0, args: three(), dst: 4 },
        Op::CallHost { func: 0, args: three(), dst: 4 },
        Op::CallBuiltin { func: 0, args: three(), dst: 4 },
        Op::MakeTuple { dst: 4, elems: three() },
        Op::GetTuple { dst: 4, base: 1, index: 1 },
        Op::Ret { src: 4 },
    ];
    assert_eq!(ops.len(), 71, "update this test when adding opcodes");

    let mut ids = HashMap::new();
    ids.insert("ops".to_string(), 0u16);
    let fp: HostFingerprint = vec![("host".into(), 0, vec![], Ty::Unit)];
    let code = ops.iter().map(|&op| Instr { op, line: 7, col: 9 }).collect();
    let script = Script {
        code: Program {
            funcs: vec![Function {
                name: "ops".into(), code, nregs: 8, params: vec![], ret: Ty::Unit,
            }],
            ids,
            strings: vec![Rc::from("constant")],
        },
        sigs: HashMap::new(),
        name: "<opcode-test>".into(),
        source_hash: cache::hash_source("opcodes"),
        // Hand-built scripts carry no source; `.sbc` round-trips drop it too.
        source: Rc::from(""),
    };
    let bytes = cache::encode(&script, &fp);
    let back = cache::decode(&bytes, &fp)?;
    assert_eq!(format!("{:?}", script.code.funcs), format!("{:?}", back.code.funcs));
    assert_eq!(script.code.strings, back.code.strings);
    Ok(())
}

#[test]
fn load_cached_rebuilds_on_change_and_cache_is_self_contained() -> miniscript::Result<()> {
    let dir = temp_dir("stale");
    let path = dir.join("rule.ms");
    let sbc = path.with_extension("sbc");
    let engine = Engine::new();

    std::fs::write(&path, "func price(x) { x * 2 }\n").unwrap();
    let mut script = engine.load_cached(&path)?;
    assert_eq!(engine.call(&script, "price", vec![Value::I64(10)])?, Value::I64(20));
    assert!(sbc.exists(), "first load should write a cache");

    // Corrupt the cache: loading must fall back to a normal compile, not fail.
    std::fs::write(&sbc, b"garbage").unwrap();
    script = engine.load_cached(&path)?;
    assert_eq!(engine.call(&script, "price", vec![Value::I64(10)])?, Value::I64(20));

    // Change the source: the hash mismatch must force a rebuild + refresh.
    std::fs::write(&path, "func price(x) { x * 3 }\n").unwrap();
    engine.reload_cached(&mut script, &path)?;
    assert_eq!(engine.call(&script, "price", vec![Value::I64(10)])?, Value::I64(30));

    // The .sbc alone is enough to load (no source file needed).
    let standalone = dir.join("standalone.sbc");
    std::fs::copy(&sbc, &standalone).unwrap();
    std::fs::remove_file(&path).unwrap();
    let from_cache = engine.load_sbc(&standalone)?;
    assert_eq!(engine.call(&from_cache, "price", vec![Value::I64(10)])?, Value::I64(30));
    Ok(())
}

#[test]
fn cache_is_rejected_when_host_registry_changed() -> miniscript::Result<()> {
    let path = temp_dir("fingerprint").join("host.sbc");
    let mut e1 = Engine::new();
    register_fn!(e1, "f", (i64) -> i64, |x| x + 1)?;
    let script = e1.load_str("func go() { f(1) }")?;
    e1.save_sbc(&script, &path)?;
    assert_eq!(e1.call(&script, "go", vec![])?, Value::I64(2));

    // Same name, different signature.
    let mut e2 = Engine::new();
    register_fn!(e2, "f", (f64) -> f64, |x| x + 1.0)?;
    let err = e2.load_sbc(&path).unwrap_err();
    assert!(err.message.contains("host registry changed"), "got: {}", err.message);

    // Missing host function (different registry set) is also rejected.
    let e3 = Engine::new();
    let err = e3.load_sbc(&path).unwrap_err();
    assert!(err.message.contains("host registry changed"), "got: {}", err.message);

    // Identical registry accepts the cache.
    let mut e4 = Engine::new();
    register_fn!(e4, "f", (i64) -> i64, |x| x + 1)?;
    let loaded = e4.load_sbc(&path)?;
    assert_eq!(e4.call(&loaded, "go", vec![])?, Value::I64(2));
    Ok(())
}

#[test]
fn damaged_cache_returns_errors_without_panicking() -> miniscript::Result<()> {
    let (engine, script) = script_with_host()?;
    let bytes = cache::encode(&script, &engine_host_fp(&engine));
    let fp = engine_host_fp(&engine);

    let mut bad_magic = bytes.clone();
    bad_magic[0] = b'X';
    let mut bad_version = bytes.clone();
    bad_version[4] = 99;
    let mut trailing = bytes.clone();
    trailing.push(0);

    let cases: Vec<Vec<u8>> = vec![
        vec![],
        bytes[..3].to_vec(),
        bytes[..bytes.len() / 2].to_vec(),
        bad_magic,
        bad_version,
        trailing,
    ];
    for (i, case) in cases.iter().enumerate() {
        let err = cache::decode(case, &fp).unwrap_err();
        assert_eq!(err.kind, miniscript::ErrorKind::Io, "case {i}");
    }
    Ok(())
}

// The engine keeps its registry private; rebuild the same fingerprint from the
// public registration API for direct cache::decode calls.
fn engine_host_fp(_engine: &Engine) -> HostFingerprint {
    // This test's script registers exactly one host function: scale(i64)->i64.
    vec![("scale".into(), 0, vec![Ty::I64], Ty::I64)]
}

#[test]
fn compile_cached_needs_no_source_file_on_disk() -> miniscript::Result<()> {
    // Android-style flow: source arrives as an in-memory asset/network payload.
    let dir = temp_dir("compile_cached");
    let cache = dir.join("rules.sbc");
    let engine = Engine::new();

    let src = "func price(x) { x * 2 }";
    let first = engine.compile_cached(src, "<assets>", &cache)?;
    assert_eq!(engine.call(&first, "price", vec![Value::I64(10)])?, Value::I64(20));
    assert!(cache.exists());

    // Second load with the same bytes hits the cache — no source file exists.
    // The caller-supplied name still wins: diagnostics must reflect this load,
    // not the name baked into the cached bytes.
    let second = engine.compile_cached(src, "<memory:price-v2>", &cache)?;
    assert_eq!(second.name, "<memory:price-v2>");
    assert_eq!(format!("{:?}", first.code.funcs), format!("{:?}", second.code.funcs));
    assert_eq!(engine.call(&second, "price", vec![Value::I64(10)])?, Value::I64(20));

    // Changed source invalidates the hash and forces a rebuild.
    let third = engine.compile_cached("func price(x) { x * 7 }", "<assets>", &cache)?;
    assert_eq!(engine.call(&third, "price", vec![Value::I64(10)])?, Value::I64(70));
    Ok(())
}
