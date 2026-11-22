//! Phase 3: `.sbc` bytecode cache — roundtrip, staleness, registry pinning,
//! and corruption handling.

use brook::cache::{self, HostFingerprint};
use brook::ir::{Function, Instr, Op, Program, Regs};
use brook::ty::Ty;
use brook::value::Value;
use brook::{register_fn, Engine, Script};
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
    let d = std::env::temp_dir().join(format!("brook_sbc_{}_{}", std::process::id(), name));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn script_with_host() -> brook::Result<(Engine, Script)> {
    let mut engine = Engine::new();
    register_fn!(engine, "scale", (i64) -> i64, |x| x * 3)?;
    let script = engine.load_str(SRC)?;
    Ok((engine, script))
}

#[test]
fn sbc_roundtrip_preserves_program_and_behavior() -> brook::Result<()> {
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
fn every_opcode_roundtrips_exactly() -> brook::Result<()> {
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
        Op::ContainsTyped { hay: 1, needle: 2, dst: 4, ty: 0 },
        Op::MakeTuple { dst: 4, elems: three() },
        Op::GetTuple { dst: 4, base: 1, index: 1 },
        Op::Ret { src: 4 },
    ];
    assert_eq!(ops.len(), 72, "update this test when adding opcodes");

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
            // `ContainsTyped` above points at type 0.
            types: vec![Ty::I64],
        },
        sigs: HashMap::new(),
        name: "<opcode-test>".into(),
        source_hash: cache::hash_source("opcodes"),
        // Hand-built scripts carry no source; `.sbc` round-trips drop it too.
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
    };
    let bytes = cache::encode(&script, &fp);
    let back = cache::decode(&bytes, &fp)?;
    assert_eq!(format!("{:?}", script.code.funcs), format!("{:?}", back.code.funcs));
    assert_eq!(script.code.strings, back.code.strings);
    Ok(())
}

#[test]
fn load_cached_rebuilds_on_change_and_cache_is_self_contained() -> brook::Result<()> {
    let dir = temp_dir("stale");
    let path = dir.join("rule.bk");
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
fn cache_is_rejected_when_host_registry_changed() -> brook::Result<()> {
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
fn damaged_cache_returns_errors_without_panicking() -> brook::Result<()> {
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
        assert_eq!(err.kind, brook::ErrorKind::Io, "case {i}");
    }
    Ok(())
}

/// D-049 changed f64 `contains` from bitwise to IEEE value equality, so a v7
/// cache must be rebuilt even though its wire layout is unchanged.
#[test]
fn v7_cache_is_rejected_after_float_equality_change() -> brook::Result<()> {
    let (engine, script) = script_with_host()?;
    let fp = engine_host_fp(&engine);
    let mut bytes = cache::encode(&script, &fp);
    bytes[4..6].copy_from_slice(&7u16.to_le_bytes());
    let err = cache::decode(&bytes, &fp).unwrap_err();
    assert!(err.message.contains("version 7 != 8"), "{}", err.message);
    Ok(())
}

// The engine keeps its registry private; rebuild the same fingerprint from the
// public registration API for direct cache::decode calls.
fn engine_host_fp(_engine: &Engine) -> HostFingerprint {
    // This test's script registers exactly one host function: scale(i64)->i64.
    vec![("scale".into(), 0, vec![Ty::I64], Ty::I64)]
}

#[test]
fn compile_cached_needs_no_source_file_on_disk() -> brook::Result<()> {
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

/// D-047/D-049: the element type of `contains` must survive encode -> decode.
/// A v6 cache (which used the untyped builtin) and a v7 cache (which compared
/// f64 by bits) are both rejected by the version gate; a v8 cache must produce
/// exactly the same answers after a round-trip.
#[test]
fn typed_contains_survives_sbc_roundtrip() -> brook::Result<()> {
    let engine = Engine::new();
    let src = r#"func f() {
        let rows = [("a.txt", 3), ("b.log", 5)]
        contains(rows, ("b.log", 5))
    }
    func nested() {
        let xs = [[1], [2]]
        contains(xs, [2])
    }"#;
    let script = engine.load_str(src)?;
    assert!(!script.code.types.is_empty(), "codegen must intern the element types");

    let path = temp_dir("typed_contains").join("prog.sbc");
    engine.save_sbc(&script, &path)?;
    let loaded = engine.load_sbc(&path)?;

    assert_eq!(script.code.types, loaded.code.types);
    assert_eq!(format!("{:?}", script.code.funcs), format!("{:?}", loaded.code.funcs));
    for func in ["f", "nested"] {
        assert_eq!(
            engine.call(&script, func, vec![])?,
            engine.call(&loaded, func, vec![])?,
            "behavior diverged after round-trip for `{func}`"
        );
        assert_eq!(engine.call(&loaded, func, vec![])?, Value::Bool(true));
    }
    Ok(())
}

/// A hand-crafted cache with a type id past the pool must be rejected, not
/// indexed unchecked by the VM.
#[test]
fn sbc_rejects_out_of_range_contains_type_id() -> brook::Result<()> {
    let engine = Engine::new();
    let script = engine.load_str("func f() { let xs = [1] contains(xs, 1) }")?;
    let fp: HostFingerprint = Vec::new();
    let mut bytes = cache::encode(&script, &fp);

    // Tag 76 is `ContainsTyped`; its wire record puts the type id in `imm`
    // right after (tag, line, col, r0, r1, r2) = 2+4+4+6 bytes.
    let needle = b"\x4c\x00"; // 76u16 little-endian
    let pos = bytes.windows(2).position(|w| w == needle).expect("ContainsTyped tag");
    let imm = pos + 2 + 4 + 4 + 6;
    bytes[imm..imm + 8].copy_from_slice(&99u64.to_le_bytes());

    let err = cache::decode(&bytes, &fp).unwrap_err();
    assert!(err.message.contains("type id out of range"), "{}", err.message);
    Ok(())
}

/// Deterministic corruption sweep: every truncated prefix and a few hundred
/// single-/multi-byte mutations must return `Err` (or at worst a still-valid
/// program) without panicking. This is the cheap fuzz guard for the binary
/// decoder; no external fuzzer dependency is needed.
#[test]
fn deterministic_cache_corruption_never_panics() -> brook::Result<()> {
    let (engine, script) = script_with_host()?;
    let fp = engine_host_fp(&engine);
    let bytes = cache::encode(&script, &fp);
    assert!(bytes.len() > 64);

    let mut cases: Vec<Vec<u8>> = Vec::new();

    // Truncation at structural boundaries and a spread of interior offsets.
    let step = (bytes.len() / 64).max(1);
    for n in (0..bytes.len()).step_by(step) {
        cases.push(bytes[..n].to_vec());
    }
    cases.push(bytes.clone());

    // Bit flips with a fixed LCG so failures are reproducible in CI.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for round in 0..256usize {
        let mut mutated = bytes.clone();
        let flips = 1 + (round % 4);
        for _ in 0..flips {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let pos = (seed as usize) % mutated.len();
            mutated[pos] ^= (seed >> 33) as u8 | 1;
        }
        cases.push(mutated);
    }

    // Pure random-looking payloads of various lengths.
    for len in [0usize, 1, 3, 7, 16, 64, 256, bytes.len() - 1, bytes.len() + 8] {
        let mut payload = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            payload.push((seed >> 56) as u8);
        }
        cases.push(payload);
    }

    for (i, case) in cases.iter().enumerate() {
        cache::decode(case, &fp).unwrap_err_or_valid(i);
    }
    Ok(())
}

/// Test-only helper: keeps the loop body terse while still checking that a
/// successful decode produced the expected host fingerprint. A corrupted cache
/// may decode successfully only if the mutation happened to preserve all
/// semantics; that is harmless.
trait CorruptResultExt {
    fn unwrap_err_or_valid(self, case: usize);
}

impl CorruptResultExt for brook::Result<Script> {
    fn unwrap_err_or_valid(self, case: usize) {
        match self {
            Ok(script) => assert_eq!(script.host_fp.len(), 1, "case {case}: unexpected script"),
            Err(err) => assert_eq!(err.kind, brook::ErrorKind::Io, "case {case}: {err}"),
        }
    }
}
