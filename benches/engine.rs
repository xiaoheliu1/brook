use criterion::{black_box, criterion_group, criterion_main, Criterion};
use vitul::engine::Engine;
use vitul::value::Value;

/// Reference Rust implementation. `#[inline(never)]` + black_box keep LLVM
/// from constant-folding the recursion into closed form, giving a fair
/// "hand-written Rust" baseline.
#[inline(never)]
fn rust_fib(n: i64) -> i64 {
    black_box(n);
    if n < 2 {
        return n;
    }
    rust_fib(n - 1) + rust_fib(n - 2)
}

fn criterion_benchmark(c: &mut Criterion) {
    let engine = Engine::new();
    let script = engine.load_file("examples/fib.vt").expect("load fib.vt");

    // Sanity: VM produces the same answer as hand-written Rust.
    assert_eq!(
        engine.call(&script, "fib", vec![Value::I64(15)]).unwrap(),
        Value::I64(rust_fib(15))
    );

    let n = 18;

    c.bench_function("1_rust fib(18)", |b| {
        b.iter(|| rust_fib(black_box(n)))
    });

    c.bench_function("2_vitul VM fib(18)", |b| {
        b.iter(|| {
            engine
                .call(&script, "fib", vec![Value::I64(n)])
                .unwrap()
        })
    });

    // Loop-heavy (non-recursive) workload — more representative of rules/data
    // scripts than fib's call-thrashing pattern.
    let loop_script = engine
        .load_str("func sum_to(n) { let t = 0\n for i in 0..n { t = t + i }\n t }")
        .expect("load loop script");
    let nloop = 200;
    #[inline(never)]
    fn rust_sum(n: i64) -> i64 {
        let mut t = 0i64;
        let mut i = 0i64;
        while i < n {
            // black_box on the accumulator prevents LLVM from collapsing the
            // arithmetic series into a closed-form expression.
            t = black_box(t).wrapping_add(i);
            i += 1;
        }
        black_box(t)
    }
    assert_eq!(
        engine.call(&loop_script, "sum_to", vec![Value::I64(nloop)]).unwrap(),
        Value::I64(rust_sum(nloop))
    );
    c.bench_function("3_rust sum_to(200)", |b| b.iter(|| rust_sum(black_box(nloop))));
    c.bench_function("4_vitul VM sum_to(200)", |b| {
        b.iter(|| engine.call(&loop_script, "sum_to", vec![Value::I64(nloop)]).unwrap())
    });

    let src = std::fs::read_to_string("examples/fib.vt").unwrap();
    c.bench_function("5_vitul compile fib.vt", |b| {
        b.iter(|| engine.load_str(&src).unwrap())
    });

    // `.sbc` cache: full compile from a file vs a cache hit (and pure decode).
    let dir = std::env::temp_dir().join(format!("vitul_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("fib.vt");
    std::fs::write(&file, &src).unwrap();
    engine.load_cached(&file).expect("populate cache");
    let sbc = file.with_extension("sbc");
    assert!(sbc.exists());
    c.bench_function("6_vitul load_file (full compile)", |b| {
        b.iter(|| engine.load_file(&file).unwrap())
    });
    c.bench_function("7_vitul load_cached (sbc hit)", |b| {
        b.iter(|| engine.load_cached(&file).unwrap())
    });
    c.bench_function("8_vitul load_sbc (decode only)", |b| {
        b.iter(|| engine.load_sbc(&sbc).unwrap())
    });

    // Pure in-memory comparison: no filesystem overhead in either path. This
    // isolates whether decoding bytecode is actually cheaper than compiling.
    let sbc_bytes = std::fs::read(&sbc).unwrap();
    c.bench_function("9_vitul compile (memory)", |b| {
        b.iter(|| engine.load_str(black_box(&src)).unwrap())
    });
    c.bench_function("10_vitul decode sbc (memory)", |b| {
        b.iter(|| engine.script_from_bytes(black_box(&sbc_bytes)).unwrap())
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
