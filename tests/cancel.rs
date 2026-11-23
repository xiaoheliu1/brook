//! Host cancellation hook: a script that never calls a host function must
//! still be stoppable. This is the engine-side half of the Android stop
//! button; the other half (wiring the flag to the hook) lives in the host.
//!
//! The checks here are all pure Rust: no JNI, no clock, no real waiting. A
//! deterministic `Cell` counter stands in for "the user pressed stop on the
//! Nth probe" so the tests never depend on timing.

use brook::{Engine, ErrorKind, Value};
use std::cell::Cell;
use std::rc::Rc;

/// A tight, allocation-free infinite loop: it calls no host function, so only
/// the VM's own cancellation probe can interrupt it.
const SPIN: &str = "func spin() { while true { 1 } }";
const CANCEL_CHECK_EVERY: u64 = 65_536;

#[test]
fn hook_stops_a_pure_compute_infinite_loop() {
    let probes = Rc::new(Cell::new(0u32));
    let p = probes.clone();
    // Stop on the third probe. Without the hook the run would burn the full
    // gas budget (10M steps) before failing; 3 probes is ~200K steps.
    let engine = Engine::new().set_cancel_hook(move || {
        p.set(p.get() + 1);
        p.get() >= 3
    });
    let script = engine.load_str(SPIN).unwrap();

    let err = engine.call(&script, "spin", vec![]).unwrap_err();
    assert!(err.is_cancelled(), "expected a cancellation, got: {err}");
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(err.message, brook::error::CANCELLED_MESSAGE);
    assert_eq!(probes.get(), 3, "exactly three probes before the stop");
    assert!(
        err.span.is_some(),
        "stop must point at the running instruction"
    );
}

#[test]
fn stop_latency_is_bounded_by_the_probe_interval() {
    let probe_count = Rc::new(Cell::new(0u32));
    let p = probe_count.clone();
    let engine = Engine::new().set_cancel_hook(move || {
        p.set(p.get() + 1);
        false
    });
    // ~655K loop iterations, each ~3 VM instructions (compare, add, jump).
    // At one probe per 65536 *instructions* this is a few dozen calls — the
    // point is that it is not one JNI round-trip per instruction (which would
    // be millions), and not zero either.
    let src = "func work(n) { let i = 0\n while i < n { i = i + 1 }\n i }";
    let script = engine.load_str(src).unwrap();
    let n = CANCEL_CHECK_EVERY * 10;
    let result = engine
        .call(&script, "work", vec![Value::I64(n as i64)])
        .unwrap();
    assert_eq!(result, Value::I64(n as i64));
    let calls = probe_count.get();
    assert!(
        (5..=100).contains(&calls),
        "expected a few dozen probes for ~2M instructions, got {calls}"
    );
}

#[test]
fn gas_limit_still_works_with_a_hook_that_never_stops() {
    let engine = Engine::new()
        .with_limits(1_000, 64)
        .set_cancel_hook(|| false);
    let script = engine.load_str(SPIN).unwrap();
    let err = engine.call(&script, "spin", vec![]).unwrap_err();
    assert!(!err.is_cancelled());
    assert!(
        err.message.contains("gas") || err.message.contains("step"),
        "{}",
        err.message
    );
}

#[test]
fn run_after_a_stop_fresh_state_no_sticky_cancellation() {
    let stop = Rc::new(Cell::new(false));
    let s = stop.clone();
    let engine = Engine::new().set_cancel_hook(move || s.get());
    let spinner = engine.load_str(SPIN).unwrap();
    let plain = engine.load_str("func answer() { 42 }").unwrap();

    stop.set(true);
    assert!(engine
        .call(&spinner, "spin", vec![])
        .unwrap_err()
        .is_cancelled());
    // The flag belongs to the caller, not to the engine: clearing it lets the
    // very same engine run the next script without any reset call.
    stop.set(false);
    assert_eq!(
        engine.call(&plain, "answer", vec![]).unwrap(),
        Value::I64(42)
    );
    stop.set(true);
    assert!(engine
        .call(&spinner, "spin", vec![])
        .unwrap_err()
        .is_cancelled());
}

#[test]
fn stop_error_renders_with_caret_and_call_stack() {
    let engine = Engine::new().set_cancel_hook(|| true);
    let script = engine
        .load_str("func inner() { while true { 1 } }\nfunc outer() { inner() }")
        .unwrap();
    let err = engine.call(&script, "outer", vec![]).unwrap_err();
    assert!(err.is_cancelled());
    let report = script.report(&err);
    assert!(report.contains("script stopped by host"), "{}", report);
    assert!(
        report.contains("inner"),
        "call stack must name the frame: {}",
        report
    );
    assert!(
        report.contains("outer"),
        "call stack must name the caller: {}",
        report
    );
}

#[test]
fn no_hook_keeps_every_behavior_unchanged() {
    let engine = Engine::new().with_limits(1_000, 64);
    let script = engine.load_str(SPIN).unwrap();
    let err = engine.call(&script, "spin", vec![]).unwrap_err();
    assert!(!err.is_cancelled());
    assert!(err.message.contains("step"), "{}", err.message);

    let ok = engine.load_str("func add(a, b) { a + b }").unwrap();
    assert_eq!(
        engine
            .call(&ok, "add", vec![Value::I64(20), Value::I64(22)])
            .unwrap(),
        Value::I64(42)
    );
}

#[test]
fn host_function_error_aborts_the_whole_run() {
    // The VM propagates host errors with `?` at CallHost; a script cannot
    // swallow one. This pins the "wait_ms was cancelled -> whole script stops"
    // contract the Android host relies on after the engine swap.
    let mut engine = Engine::new();
    engine
        .register_fn("boom", vec![], brook::ty::Ty::I64, |_| {
            Err(brook::Error::io("disk on fire"))
        })
        .unwrap();
    let script = engine.load_str("func f() { let x = boom()\n 99 }").unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Io);
    assert!(err.message.contains("disk on fire"), "{}", err.message);
}
