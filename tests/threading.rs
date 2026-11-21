//! Threading and hot-reload semantics.
//!
//! The engine is deliberately single-threaded: `Engine` and `Script` are
//! `Rc`-backed and therefore `!Send` / `!Sync`, so sharing one across threads
//! does not compile. What a multi-threaded host (Android JNI callbacks arrive
//! on arbitrary threads) actually does is build one engine per thread. These
//! tests pin that model down: no hidden global state, and reload never leaves a
//! script half-old/half-new.

use brook::{register_fn, Engine, Value};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("brook_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn one_engine_per_thread_is_independent() {
    // Each thread registers a host function with a different closure state. If
    // anything were shared through a global (or a static registry), the
    // multipliers would bleed between threads.
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for k in 1..=8i64 {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let mut engine = Engine::new();
            register_fn!(engine, "scale", (i64), move |x| x * k).unwrap();
            // Same source in every thread: the compiled program must not be
            // shared state either.
            let script = engine
                .load_str("func f(x) { scale(x) + scale(1) }")
                .expect("compile");
            let mut results = Vec::new();
            for i in 0..50 {
                // `Value` is !Send (it may hold a host `Rc`), so results cross
                // the channel as plain i64 — the same restriction the engine
                // itself has.
                let v = engine.call(&script, "f", vec![Value::I64(i)]).unwrap();
                results.push(v.as_i64().unwrap());
            }
            tx.send((k, results)).unwrap();
        }));
    }
    drop(tx);

    for (k, results) in rx.iter() {
        for (i, v) in results.into_iter().enumerate() {
            // scale(i) + scale(1) = i*k + k
            assert_eq!(v, i as i64 * k + k, "thread k={} i={}", k, i);
        }
    }
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn concurrent_paths_can_compile_the_same_source() {
    // Compilation is pure (`&self`), so two live engines in different threads
    // can compile identical text without ordering effects.
    let src = "func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }";
    let handles: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(move || {
                let engine = Engine::new();
                let script = engine.load_str(src).unwrap();
                engine.call(&script, "fib", vec![Value::I64(15)]).unwrap().as_i64().unwrap()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 610);
    }
}

#[test]
fn reload_replaces_the_implementation_for_the_next_call() {
    let dir = temp_dir("reload_next");
    let path = dir.join("v.bk");
    std::fs::write(&path, "func go(x) { x + 1 }").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path).unwrap();
    assert_eq!(engine.call(&script, "go", vec![Value::I64(1)]).unwrap(), Value::I64(2));

    std::fs::write(&path, "func go(x) { x + 100 }").unwrap();
    engine.reload(&mut script, &path).unwrap();
    assert_eq!(engine.call(&script, "go", vec![Value::I64(1)]).unwrap(), Value::I64(101));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_clone_keeps_the_bytecode_it_was_taken_from() {
    // The in-flight guarantee: a call borrows its `&Script`, so reload cannot
    // run concurrently in safe Rust. A clone (an `Rc` of the pieces the caller
    // kept) must therefore keep evaluating the old function.
    let dir = temp_dir("reload_clone");
    let path = dir.join("v.bk");
    std::fs::write(&path, "func go(x) { x + 1 }").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path).unwrap();
    let old = script.clone();

    std::fs::write(&path, "func go(x) { x * 1000 }").unwrap();
    engine.reload(&mut script, &path).unwrap();

    assert_eq!(engine.call(&script, "go", vec![Value::I64(3)]).unwrap(), Value::I64(3000));
    assert_eq!(engine.call(&old, "go", vec![Value::I64(3)]).unwrap(), Value::I64(4));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_failed_reload_leaves_the_old_script_usable() {
    let dir = temp_dir("reload_fail");
    let path = dir.join("v.bk");
    std::fs::write(&path, "func go(x) { x + 1 }").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path).unwrap();

    std::fs::write(&path, "func go(x) { x + ").unwrap();
    let err = engine.reload(&mut script, &path).expect_err("broken source must fail");
    assert!(err.to_string().contains("parse"), "got: {}", err);

    // The old bytecode and its signature survived the failed reload.
    assert_eq!(engine.call(&script, "go", vec![Value::I64(41)]).unwrap(), Value::I64(42));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reload_refreshes_the_source_used_for_diagnostics() {
    let dir = temp_dir("reload_diag");
    let path = dir.join("v.bk");
    std::fs::write(&path, "func go(x) { x + 1 }").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path).unwrap();

    // New version: same shape, but the failing division sits on line 3.
    std::fs::write(&path, "func go(x) {\n  let y = x\n  y / x\n}").unwrap();
    engine.reload(&mut script, &path).unwrap();

    let err = engine.call(&script, "go", vec![Value::I64(0)]).expect_err("div by zero");
    let rendered = script.report(&err);
    assert!(rendered.contains(":3:"), "report should point at the new line 3:\n{}", rendered);
    assert!(
        rendered.contains("y / x"),
        "report should quote the reloaded source, not the old one:\n{}",
        rendered
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn host_handles_stay_thread_local() {
    // A host type with `Rc` state cannot be sent between threads, which is the
    // same reason `Engine` is !Send: handles are host-owned and never copied
    // across the boundary. This is a compile-time property, asserted here by
    // construction (the value stays inside its thread).
    #[derive(Clone)]
    struct Counter(Rc<std::cell::Cell<i64>>);

    let mut engine = Engine::new();
    engine.register_type::<Counter>().unwrap();
    let counter = Counter(Rc::new(std::cell::Cell::new(0)));
    let c = counter.clone();
    register_fn!(engine, "bump", (i64), move |n| {
        c.0.set(c.0.get() + n);
        c.0.get()
    })
    .unwrap();

    let script = engine.load_str("func f(x) { bump(x) }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![Value::I64(2)]).unwrap(), Value::I64(2));
    assert_eq!(engine.call(&script, "f", vec![Value::I64(3)]).unwrap(), Value::I64(5));
    assert_eq!(counter.0.get(), 5);
}
