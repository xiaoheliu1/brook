//! Android smoke-test binary.
//!
//! Cross-compile for an Android ABI, `adb push` it (plus an optional .bk
//! script), and run it from `adb shell`. It exercises the whole pipeline on the
//! target device through both FFI layers:
//!
//! 1. The Rust API end-to-end: typed host callback + opaque handle + tuple.
//! 2. The retained C ABI: `ms_script_create_cached` (bytecode cache), tagged
//!    multi-argument calls, string results, a located error with a multi-line
//!    report, hot reload, and `ms_script_free`.
//!
//! Usage: `android_cli [path/to/script.bk]` (defaults to the built-in rule).

use brook::ffi::{
    ms_last_error, ms_last_report, ms_script_call, ms_script_create_cached, ms_script_free,
    ms_script_reload, MsVal, MsValOut, MS_ERR_BUFFER, MS_ERR_NULL, MS_ERR_RUNTIME, MS_OK,
    MS_VAL_F64, MS_VAL_I64, MS_VAL_STR,
};
use brook::{register_fn, Engine, Opaque, Value};
use std::ffi::{c_char, CString};

#[derive(Debug)]
struct Order {
    total: f64,
    vip: bool,
}

const DEFAULT_SCRIPT: &str = r#"
// Order risk rule; the host registers order_total(Order) -> f64 and
// is_vip(Order) -> bool before loading this source.
func discount(amount, vip) {
    if vip { amount * 0.8 } else { amount }
}
func check(order, limit) {
    let total = order_total(order)
    let vip = is_vip(order)
    let d = discount(total, vip)
    if d > limit { (0.0, false) } else { (d, true) }
}
"#;

// Scalar-only sources for the retained C ABI (handles never cross that
// boundary by design). `add3` gets a hotter body after the reload demo.
const HANDLE_SCRIPT: &str = r#"
func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }
func add3(a, b, c) { a + b + c }
func describe(score: f64) { "score=" + to_str(to_i64(score)) }
"#;

const HANDLE_SCRIPT_V2: &str = r#"
func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }
func add3(a, b, c) { a + b + c + 1 }
func describe(score: f64) { "score=" + to_str(to_i64(score)) }
"#;

fn val_i64(n: i64) -> MsVal {
    MsVal {
        tag: MS_VAL_I64,
        i: n,
        f: 0.0,
        s: std::ptr::null(),
    }
}

fn val_f64(x: f64) -> MsVal {
    MsVal {
        tag: MS_VAL_F64,
        i: 0,
        f: x,
        s: std::ptr::null(),
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("no interior NUL")
}

fn last_error() -> String {
    let mut buf = [0u8; 1024];
    let n = unsafe { ms_last_error(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    String::from_utf8_lossy(&buf[..n]).to_string()
}

fn last_report() -> String {
    let mut buf = [0u8; 2048];
    let n = unsafe { ms_last_report(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    String::from_utf8_lossy(&buf[..n]).to_string()
}

fn main() -> brook::Result<()> {
    // ---- 1. Rust API end-to-end (typed host callback + opaque handle) ----
    let mut engine = Engine::new();
    register_fn!(engine, "order_total", (Opaque<Order>) -> f64, |o| o.total)?;
    register_fn!(engine, "is_vip", (Opaque<Order>) -> bool, |o| o.vip)?;

    let source = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| brook::Error::io(format!("{}: {}", path, e)))?,
        None => DEFAULT_SCRIPT.to_string(),
    };
    let script = engine.load_str(&source)?;
    let order = Value::from(Opaque::new(Order {
        total: 200.0,
        vip: true,
    }));
    let result = engine.call(&script, "check", vec![order, Value::F64(1000.0)])?;
    println!(
        "brook {} / {}: check(Order{{200,vip}}, 1000) = {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        result
    );

    // ---- 2. Retained C ABI: create_cached -> call -> reload -> free ----
    unsafe { handle_api_demo()? };

    Ok(())
}

unsafe fn handle_api_demo() -> brook::Result<()> {
    let cache = std::env::temp_dir().join("ms_cli_cache.sbc");
    let src = cstr(HANDLE_SCRIPT);
    let name = cstr("handle_demo");
    let cache_c = cstr(&cache.to_string_lossy());

    let mut ptr: *mut brook::ffi::MsScript = std::ptr::null_mut();
    let code = ms_script_create_cached(src.as_ptr(), name.as_ptr(), cache_c.as_ptr(), &mut ptr);
    assert_eq!(code, MS_OK, "create_cached failed: {}", last_error());

    // Multi-argument i64 call.
    let args = [val_i64(1), val_i64(2), val_i64(3)];
    let mut out = MsValOut {
        tag: 0,
        i: 0,
        f: 0.0,
        len: 0,
    };
    let func = cstr("add3");
    let code = ms_script_call(
        ptr,
        func.as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(code, MS_OK, "add3 failed: {}", last_error());
    assert_eq!((out.tag, out.i), (MS_VAL_I64, 6));
    println!("handle: add3(1,2,3) = {}", out.i);

    // Recursion on the retained bytecode.
    let args = [val_i64(15)];
    let code = ms_script_call(
        ptr,
        cstr("fib").as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 610);
    println!("handle: fib(15) = {}", out.i);

    // String result (buffer protocol: grow + retry on MS_ERR_BUFFER).
    let args = [val_f64(98.6)];
    let mut buf = [0u8; 8]; // deliberately too small
    let mut code = ms_script_call(
        ptr,
        cstr("describe").as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        buf.as_mut_ptr().cast::<c_char>(),
        buf.len(),
    );
    assert_eq!(code, MS_ERR_BUFFER, "small buffer must be rejected");
    assert_eq!(out.len, 8, "required size reported: score=98");
    let mut grown = vec![0u8; out.len + 1];
    code = ms_script_call(
        ptr,
        cstr("describe").as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        grown.as_mut_ptr().cast::<c_char>(),
        grown.len(),
    );
    assert_eq!(code, MS_OK);
    assert_eq!(out.tag, MS_VAL_STR);
    let text = String::from_utf8_lossy(&grown[..out.len]).to_string();
    assert_eq!(text, "score=98");
    println!("handle: describe(98.6) = \"{}\"", text);

    // Error path: located message + multi-line report against the source.
    let code = ms_script_call(
        ptr,
        cstr("missing").as_ptr(),
        std::ptr::null(),
        0,
        &mut out,
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(code, MS_ERR_RUNTIME);
    println!("handle: missing() -> [{}]", last_error());
    println!("handle: report ---\n{}", last_report());

    // Hot reload: new body takes effect without recreating the handle.
    let v2 = cstr(HANDLE_SCRIPT_V2);
    let code = ms_script_reload(ptr, v2.as_ptr(), name.as_ptr());
    assert_eq!(code, MS_OK, "reload failed: {}", last_error());
    let args = [val_i64(1), val_i64(2), val_i64(3)];
    let code = ms_script_call(
        ptr,
        cstr("add3").as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 7, "reloaded add3 must return 7");
    println!("handle: after reload add3(1,2,3) = {}", out.i);

    assert_eq!(ms_script_free(ptr), MS_OK);
    assert_eq!(ms_script_free(std::ptr::null_mut()), MS_ERR_NULL);
    println!("handle: free + null-free OK — retained C ABI verified");
    Ok(())
}
