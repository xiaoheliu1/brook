//! Zero-dependency fuzz smoke: deterministic mutation and generation loops
//! that must never panic the compiler front end or the cache decoder.
//!
//! Full coverage fuzzing (cargo-fuzz + libFuzzer) needs a POSIX target; these
//! loops run anywhere `cargo test` runs and catch the common failure classes:
//! panics on malformed `.sbc` bytes, panics on adversarial source, and
//! unbounded recursion escaping `MAX_NESTING` (including the if-expression
//! `else if` path that once slipped past the guard). Seeds are fixed so any
//! failure reproduces exactly.

use miniscript::Engine;

/// xorshift64* — deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn valid_sbc() -> Vec<u8> {
    let engine = Engine::new();
    let script = engine
        .load_str(
            "func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }\n\
             func sum(n) { let t = 0\n for i in 0..n { t = t + i }\n t }\n\
             func text(a: str, b: str) { a + b }",
        )
        .unwrap();
    engine.script_to_bytes(&script)
}

/// Every byte-level mutation of a valid cache must decode to `Ok` or `Err` —
/// never panic, never abort, never read out of bounds. This is the safety net
/// under the `.sbc` "corruption returns Err" guarantee: decode runs the wire
/// validators, and on `Ok` the whole program has passed `validate`.
#[test]
fn mutated_cache_bytes_never_panic() {
    let original = valid_sbc();
    let engine = Engine::new();
    let mut rng = Rng::new(0xDEAD_BEEF_CAFE_0001);

    for round in 0..5000u32 {
        let mut bytes = original.clone();
        match rng.below(4) {
            0 => {
                // Flip a few random bytes.
                for _ in 0..(1 + rng.below(4)) {
                    let pos = rng.below(bytes.len());
                    bytes[pos] ^= (1 + rng.next() % 255) as u8;
                }
            }
            1 => {
                // Truncate at a random length.
                let len = rng.below(bytes.len() + 1);
                bytes.truncate(len);
            }
            2 => {
                // Splice a garbage chunk in.
                let pos = rng.below(bytes.len() + 1);
                let tail = bytes.split_off(pos);
                for _ in 0..(1 + rng.below(16)) {
                    bytes.push((rng.next() & 0xFF) as u8);
                }
                bytes.extend(tail);
            }
            _ => {
                // Overwrite a chunk in place.
                let pos = rng.below(bytes.len());
                let n = (1 + rng.below(16)).min(bytes.len() - pos);
                for i in 0..n {
                    bytes[pos + i] = (rng.next() & 0xFF) as u8;
                }
            }
        }
        let _ = engine.script_from_bytes(&bytes);
        let _ = round;
    }
}

/// Header-level mutations: magic and version must be gated (wrong magic /
/// version always rejected) even before structural validation.
#[test]
fn cache_header_is_gated() {
    let original = valid_sbc();
    let engine = Engine::new();

    // Wrong magic.
    let mut bytes = original.clone();
    bytes[0] = b'X';
    assert!(engine.script_from_bytes(&bytes).is_err());

    // Wrong version (any of the two little-endian bytes).
    let mut bytes = original.clone();
    bytes[4] = 0xFE;
    assert!(engine.script_from_bytes(&bytes).is_err());

    // Truncated header.
    for len in 0..8 {
        assert!(engine.script_from_bytes(&original[..len]).is_err());
    }
}

/// Random token soup must always compile to `Ok` or a located error — never
/// panic the lexer, parser, checker or codegen, never overflow the stack.
#[test]
fn generated_source_never_panics_the_compiler() {
    const TOKENS: &[&str] = &[
        "func", "let", "if", "else", "while", "for", "in", "return", "break", "continue",
        "true", "false", "x", "y", "n", "f", "g", "abs", "len", "to_str", "min",
        "0", "1", "2", "42", "-7", "0.5", "\"s\"", "\"中文\"",
        "+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "&&", "||", "!",
        "(", ")", "{", "}", ",", ".", "..", ":", "=", "->", "\n",
    ];
    let engine = Engine::new();
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);

    for _ in 0..3000 {
        let count = 5 + rng.below(60);
        let mut src = String::new();
        for _ in 0..count {
            src.push_str(TOKENS[rng.below(TOKENS.len())]);
            if rng.below(4) == 0 {
                src.push(' ');
            }
        }
        let _ = engine.load_str(&src);
    }
}

/// Depth-stress generators, randomized over kind and length. Small rungs
/// compile; deep rungs must be rejected with the nesting error — the one
/// failure mode that used to be an uncatchable process abort.
#[test]
fn deep_nesting_variants_stay_bounded() {
    let engine = Engine::new();
    let mut rng = Rng::new(0xBAD_C0DE_0000_0042);

    for _ in 0..200 {
        let n = 10 + rng.below(3000);
        let src = match rng.below(5) {
            0 => format!("func f(x) {{ {}x{} }}", "(".repeat(n), ")".repeat(n)),
            1 => format!("func f(x) {{ {}x }}", "-".repeat(n)),
            2 => format!("func f() {{ {}1 }}", "1+".repeat(n)),
            3 => format!("func f() {{ {}{} }}", "{".repeat(n), "}".repeat(n)),
            _ => {
                // Expression-position else-if ladder (the path that used to
                // escape MAX_NESTING entirely).
                let mut s = String::from("func f(x) { let v = if x { 1 }");
                for _ in 0..n {
                    s.push_str(" else if x { 1 }");
                }
                s.push_str(" else { 2 }\n    v }");
                s
            }
        };
        match engine.load_str(&src) {
            Ok(_) => {}
            Err(e) => assert!(
                e.message.contains("nesting too deep"),
                "deep nesting must report the guard, got: {}",
                e.message
            ),
        }
    }
}
