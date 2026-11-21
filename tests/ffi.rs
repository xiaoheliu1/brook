//! C ABI boundary tests (the code that ships inside libbrook.so).
//!
//! Device execution is verified by `scripts/android_build.sh` when a device is
//! attached; these tests exercise the exact same FFI entry points on the host.

use brook::ffi::{
    ms_abi_version, ms_call_i64, ms_last_error, ms_last_error_col, ms_last_error_line,
    ms_last_report, ms_script_call, ms_script_create, ms_script_create_cached, ms_script_free,
    ms_script_reload, MsVal, MsValOut, MS_ERR_BUFFER, MS_ERR_ENGINE, MS_ERR_NULL, MS_ERR_RUNTIME,
    MS_ERR_THREAD, MS_OK, MS_VAL_BOOL, MS_VAL_F64, MS_VAL_I64, MS_VAL_STR,
};
use std::ffi::{c_char, CString};

struct FfiCall {
    code: i32,
    out: i64,
    error: String,
    line: u32,
    col: u32,
}

fn call(source: Option<&str>, func: Option<&str>, arg: i64) -> FfiCall {
    let src = source.map(|s| CString::new(s).unwrap());
    let name = func.map(|s| CString::new(s).unwrap());
    let src_ptr = src.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
    let name_ptr = name.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
    let mut out: i64 = -1;
    let code = unsafe { ms_call_i64(src_ptr, name_ptr, arg, &mut out) };
    let mut buf = [0u8; 256];
    let n = unsafe { ms_last_error(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    FfiCall {
        code,
        out,
        error: String::from_utf8_lossy(&buf[..n]).to_string(),
        line: ms_last_error_line(),
        col: ms_last_error_col(),
    }
}

#[test]
fn ffi_compiles_and_calls_a_script() {
    assert_eq!(ms_abi_version(), 1);
    let r = call(Some("func twice(x) { x + x }"), Some("twice"), 21);
    assert_eq!(r.code, MS_OK, "error: {}", r.error);
    assert_eq!(r.out, 42);
    let r2 = call(Some("func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }"), Some("fib"), 12);
    assert_eq!(r2.code, MS_OK, "error: {}", r2.error);
    assert_eq!(r2.out, 144);
}

#[test]
fn ffi_errors_are_returned_not_unwound() {
    // Call-stage failure: the script compiled, but there is no such function.
    let bad_func = call(Some("func go(x) { x }"), Some("missing"), 1);
    assert_eq!(bad_func.code, MS_ERR_RUNTIME);
    assert!(bad_func.error.contains("unknown function"), "got: {}", bad_func.error);
    assert_eq!((bad_func.line, bad_func.col), (0, 0), "no source location");

    // f64 result cannot be written into an i64 out slot: a contract mismatch,
    // reported as a compile-stage/ABI error rather than a script runtime bug.
    let wrong_ty = call(Some("func go(x) { to_f64(x) }"), Some("go"), 1);
    assert_eq!(wrong_ty.code, MS_ERR_ENGINE);
    assert!(wrong_ty.error.contains("expected i64"), "got: {}", wrong_ty.error);

    let null = call(None, Some("go"), 1);
    assert_eq!(null.code, MS_ERR_NULL);
    assert!(!null.error.is_empty());

    let parse_err = call(Some("func {"), Some("go"), 1);
    assert_eq!(parse_err.code, MS_ERR_ENGINE);
    assert!(!parse_err.error.is_empty());
}

#[test]
fn ffi_distinguishes_compile_errors_from_runtime_errors() {
    // A type error inside the script: nothing runs.
    let type_err = call(Some("func go(x) { let n: i64 = \"s\"  n }"), Some("go"), 1);
    assert_eq!(type_err.code, MS_ERR_ENGINE, "got: {}", type_err.error);
    assert!(type_err.error.contains("found str"), "got: {}", type_err.error);

    // Division by zero: compiled fine, failed while running.
    let div0 = call(Some("func go(x) { 100 / x }"), Some("go"), 0);
    assert_eq!(div0.code, MS_ERR_RUNTIME, "got: {}", div0.error);
    assert!(div0.error.contains("division by zero"), "got: {}", div0.error);
}

#[test]
fn ffi_reports_the_error_location() {
    // Runtime error: the location is the faulting instruction (`/`), not the
    // function header, so a C-side UI can point at the right column.
    let div0 = call(Some("func go(x) { 100 / x }"), Some("go"), 0);
    assert_eq!(div0.line, 1, "got: {}", div0.error);
    assert_eq!(div0.col, 18, "caret should sit on `/`, got: {}", div0.error);

    // Compile error: parser position, still a real line/col.
    let parse_err = call(Some("func go(x) {\n  x +\n}"), Some("go"), 1);
    assert_eq!(parse_err.code, MS_ERR_ENGINE);
    assert_eq!(parse_err.line, 3, "got: {}", parse_err.error);

    // A successful call leaves the previous failure readable (two-step
    // protocol, no implicit clear) — documented behaviour, not an accident.
    let ok = call(Some("func go(x) { x }"), Some("go"), 5);
    assert_eq!(ok.code, MS_OK);
    assert_eq!(ok.line, parse_err.line);
}

#[test]
fn ffi_error_buffer_is_nul_terminated_and_truncated_safely() {
    let _ = call(Some("func go(x) { x }"), Some("missing"), 1);
    let mut buf = [0x7fu8; 8];
    let n = unsafe { ms_last_error(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    assert_eq!(n, 7);
    assert_eq!(buf[7], 0);
    let empty = unsafe { ms_last_error(buf.as_mut_ptr().cast::<c_char>(), 0) };
    assert_eq!(empty, 0);
}

// ---------------------------------------------------------------------------
// Retained-handle layer
// ---------------------------------------------------------------------------

fn val_i64(n: i64) -> MsVal {
    MsVal { tag: MS_VAL_I64, i: n, f: 0.0, s: std::ptr::null() }
}

fn val_f64(x: f64) -> MsVal {
    MsVal { tag: MS_VAL_F64, i: 0, f: x, s: std::ptr::null() }
}

fn val_bool(b: bool) -> MsVal {
    MsVal { tag: MS_VAL_BOOL, i: if b { 1 } else { 0 }, f: 0.0, s: std::ptr::null() }
}

fn val_str(s: &str) -> MsVal {
    let bytes = s.as_bytes();
    MsVal { tag: MS_VAL_STR, i: bytes.len() as i64, f: 0.0, s: bytes.as_ptr().cast::<c_char>() }
}

/// Read the last error / report after an FFI call.
fn last_error_and_report() -> (String, String) {
    let mut buf = [0u8; 1024];
    let n = unsafe { ms_last_error(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    let err = String::from_utf8_lossy(&buf[..n]).to_string();
    let n = unsafe { ms_last_report(buf.as_mut_ptr().cast::<c_char>(), buf.len()) };
    let report = String::from_utf8_lossy(&buf[..n]).to_string();
    (err, report)
}

unsafe fn create_script(src: &str) -> *mut brook::ffi::MsScript {
    let src = CString::new(src).unwrap();
    let name = CString::new("test.bk").unwrap();
    let mut ptr: *mut brook::ffi::MsScript = std::ptr::null_mut();
    let code = ms_script_create(src.as_ptr(), name.as_ptr(), &mut ptr);
    assert_eq!(code, MS_OK, "create failed: {}", last_error_and_report().0);
    ptr
}

/// Call `func` with `args`; returns (code, out, buffer).
unsafe fn call_script(
    ptr: *mut brook::ffi::MsScript,
    func: &str,
    args: &[MsVal],
) -> (i32, MsValOut, Vec<u8>) {
    let func = CString::new(func).unwrap();
    let mut out = MsValOut { tag: 0, i: 0, f: 0.0, len: 0 };
    let mut buf = vec![0x7fu8; 256];
    let code = ms_script_call(
        ptr,
        func.as_ptr(),
        args.as_ptr(),
        args.len(),
        &mut out,
        buf.as_mut_ptr().cast::<c_char>(),
        buf.len(),
    );
    (code, out, buf)
}

#[test]
fn ffi_script_handle_lifecycle() {
    let ptr = unsafe { create_script("func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }\nfunc unit() { }") };

    // Repeated calls on one compilation.
    for _ in 0..3 {
        let (code, out, _) = unsafe { call_script(ptr, "fib", &[val_i64(15)]) };
        assert_eq!(code, MS_OK);
        assert_eq!(out.tag, MS_VAL_I64);
        assert_eq!(out.i, 610);
    }

    // Zero-arg call returning Unit.
    let (code, out, _) = unsafe { call_script(ptr, "unit", &[]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.tag, brook::ffi::MS_VAL_UNIT);

    // Hot reload swaps the bytecode in place.
    let new_src = CString::new("func fib(n) { n * 10 }").unwrap();
    let name = CString::new("test.bk").unwrap();
    let code = unsafe { ms_script_reload(ptr, new_src.as_ptr(), name.as_ptr()) };
    assert_eq!(code, MS_OK, "reload failed: {}", last_error_and_report().0);
    let (code, out, _) = unsafe { call_script(ptr, "fib", &[val_i64(3)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 30, "reloaded logic must take effect");

    // Failed reload keeps the old bytecode usable.
    let bad = CString::new("func {").unwrap();
    let code = unsafe { ms_script_reload(ptr, bad.as_ptr(), std::ptr::null()) };
    assert_eq!(code, MS_ERR_ENGINE);
    let (code, out, _) = unsafe { call_script(ptr, "fib", &[val_i64(3)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 30, "failed reload must not touch the old script");

    // A null-name reload keeps the current name.
    let newer = CString::new("func fib(n) { n * 100 }").unwrap();
    let code = unsafe { ms_script_reload(ptr, newer.as_ptr(), std::ptr::null()) };
    assert_eq!(code, MS_OK);
    let (code, out, _) = unsafe { call_script(ptr, "fib", &[val_i64(1)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 100);

    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);
    assert_eq!(unsafe { ms_script_free(std::ptr::null_mut()) }, MS_ERR_NULL);
}

#[test]
fn ffi_script_call_types() {
    let ptr = unsafe {
        create_script(
            "func half(x: f64) { x * 0.5 }\nfunc flip(b: bool) { !b }\nfunc add3(a, b, c) { a + b + c }",
        )
    };

    let (code, out, _) = unsafe { call_script(ptr, "half", &[val_f64(9.0)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.tag, MS_VAL_F64);
    assert_eq!(out.f, 4.5);

    let (code, out, _) = unsafe { call_script(ptr, "flip", &[val_bool(true)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.tag, MS_VAL_BOOL);
    assert_eq!(out.i, 0);

    let (code, out, _) = unsafe { call_script(ptr, "add3", &[val_i64(1), val_i64(2), val_i64(3)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 6);

    // i64 arguments coerce into an f64 parameter, mirroring the Rust API.
    let (code, out, _) = unsafe { call_script(ptr, "half", &[val_i64(8)]) };
    assert_eq!(code, MS_OK, "coercion expected");
    assert_eq!(out.f, 4.0);

    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);
}

#[test]
fn ffi_script_strings_cross_the_boundary() {
    let ptr = unsafe { create_script("func greet(name: str) { \"hi, \" + name + \"!\" }") };

    let (code, out, buf) = unsafe { call_script(ptr, "greet", &[val_str("中文")]) };
    assert_eq!(code, MS_OK, "{}", last_error_and_report().0);
    assert_eq!(out.tag, MS_VAL_STR);
    assert_eq!(out.len, 11, "`hi, ` (4) + 中文 (6) + `!` (1) = 11 bytes");
    let nul = out.len;
    assert_eq!(buf[nul], 0, "NUL-terminated");
    assert_eq!(String::from_utf8_lossy(&buf[..nul]), "hi, 中文!");

    // A buffer that is too small: MS_ERR_BUFFER with the required size, no
    // write, retry with a bigger buffer.
    let func = CString::new("greet").unwrap();
    let mut out = MsValOut { tag: 0, i: 0, f: 0.0, len: 0 };
    let mut small = [0x7fu8; 4];
    let args = [val_str("中文")];
    let code = unsafe {
        ms_script_call(
            ptr,
            func.as_ptr(),
            args.as_ptr(),
            1,
            &mut out,
            small.as_mut_ptr().cast::<c_char>(),
            small.len(),
        )
    };
    assert_eq!(code, MS_ERR_BUFFER);
    assert_eq!(out.tag, MS_VAL_STR);
    assert_eq!(out.len, 11, "required size, so the caller can retry");
    assert_eq!(small, [0x7fu8; 4], "nothing was written");

    let (code, out, buf) = unsafe { call_script(ptr, "greet", &[val_str("")]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.len, 5);
    assert_eq!(String::from_utf8_lossy(&buf[..5]), "hi, !");

    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);
}

#[test]
fn ffi_script_create_cached_roundtrips() {
    let dir = std::env::temp_dir().join(format!("ms_ffi_cache_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("demo.sbc");
    let src = "func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }";
    let src_c = CString::new(src).unwrap();
    let name_c = CString::new("cached.bk").unwrap();
    let cache_c = CString::new(cache.to_string_lossy().as_bytes()).unwrap();

    // First create compiles and writes the cache; the second decodes it.
    let ptr1 = unsafe {
        let mut p: *mut brook::ffi::MsScript = std::ptr::null_mut();
        assert_eq!(
            ms_script_create_cached(src_c.as_ptr(), name_c.as_ptr(), cache_c.as_ptr(), &mut p),
            MS_OK,
            "{}",
            last_error_and_report().0
        );
        p
    };
    assert!(cache.exists(), "cache written best-effort");
    let ptr2 = unsafe {
        let mut p: *mut brook::ffi::MsScript = std::ptr::null_mut();
        assert_eq!(
            ms_script_create_cached(src_c.as_ptr(), name_c.as_ptr(), cache_c.as_ptr(), &mut p),
            MS_OK,
            "{}",
            last_error_and_report().0
        );
        p
    };

    let (code, out, _) = unsafe { call_script(ptr2, "fib", &[val_i64(10)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 55);

    // A changed source invalidates the cache transparently.
    let other = CString::new("func fib(n) { n * 1000 }").unwrap();
    let ptr3 = unsafe {
        let mut p: *mut brook::ffi::MsScript = std::ptr::null_mut();
        assert_eq!(
            ms_script_create_cached(other.as_ptr(), name_c.as_ptr(), cache_c.as_ptr(), &mut p),
            MS_OK,
            "{}",
            last_error_and_report().0
        );
        p
    };
    let (code, out, _) = unsafe { call_script(ptr3, "fib", &[val_i64(2)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 2000, "stale cache must be rebuilt, not served");

    assert_eq!(unsafe { ms_script_free(ptr1) }, MS_OK);
    assert_eq!(unsafe { ms_script_free(ptr2) }, MS_OK);
    assert_eq!(unsafe { ms_script_free(ptr3) }, MS_OK);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ffi_script_errors_carry_report_diagnostics() {
    // Runtime error on a retained script: message, location, and the
    // multi-line report rendered against the retained source.
    let ptr = unsafe { create_script("func go(x) {\n  100 / x\n}") };
    let (code, _, _) = unsafe { call_script(ptr, "go", &[val_i64(0)]) };
    assert_eq!(code, MS_ERR_RUNTIME);
    let (err, report) = last_error_and_report();
    assert!(err.contains("division by zero"), "got: {}", err);
    assert_eq!(ms_last_error_line(), 2);
    assert!(report.contains("--> test.bk:2:"), "got: {}", report);
    assert!(report.contains("call stack"), "got: {}", report);
    assert!(report.contains("100 / x"), "source line rendered, got: {}", report);
    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);

    // Compile failure at create: report rendered against the rejected source.
    let bad = CString::new("func go(x) {\n  let n: i64 = \"s\"\n  n\n}").unwrap();
    let name = CString::new("bad.bk").unwrap();
    let mut ptr: *mut brook::ffi::MsScript = std::ptr::null_mut();
    let code = unsafe { ms_script_create(bad.as_ptr(), name.as_ptr(), &mut ptr) };
    assert_eq!(code, MS_ERR_ENGINE);
    let (err, report) = last_error_and_report();
    assert!(err.contains("expected i64"), "got: {}", err);
    assert!(report.contains("--> bad.bk:2:"), "got: {}", report);
}

#[test]
fn ffi_script_contract_and_null_errors() {
    let ptr = unsafe { create_script("func go(x) { x }") };

    // Unknown function: runtime stage.
    let (code, _, _) = unsafe { call_script(ptr, "missing", &[val_i64(1)]) };
    assert_eq!(code, MS_ERR_RUNTIME);

    // Tuple results cannot cross the boundary.
    let tuple_script = unsafe { create_script("func pair() { (1, 2) }") };
    let (code, _, _) = unsafe { call_script(tuple_script, "pair", &[]) };
    assert_eq!(code, MS_ERR_ENGINE);
    let (err, _) = last_error_and_report();
    assert!(err.contains("tuple results"), "got: {}", err);
    assert_eq!(unsafe { ms_script_free(tuple_script) }, MS_OK);

    // Unknown value tag and negative string length: caller contract.
    let bogus = MsVal { tag: 99, i: 0, f: 0.0, s: std::ptr::null() };
    let (code, _, _) = unsafe { call_script(ptr, "go", &[bogus]) };
    assert_eq!(code, MS_ERR_ENGINE);
    let bad_len = MsVal { tag: MS_VAL_STR, i: -3, f: 0.0, s: std::ptr::null() };
    let (code, _, _) = unsafe { call_script(ptr, "go", &[bad_len]) };
    assert_eq!(code, MS_ERR_ENGINE);

    // Non-empty STR payload with a null pointer: reported, not dereferenced.
    let null_payload = MsVal { tag: MS_VAL_STR, i: 5, f: 0.0, s: std::ptr::null() };
    let (code, _, _) = unsafe { call_script(ptr, "go", &[null_payload]) };
    assert_eq!(code, MS_ERR_NULL, "{}", last_error_and_report().0);

    // Nulls are reported, never dereferenced.
    let func = CString::new("go").unwrap();
    let mut out = MsValOut { tag: 0, i: 0, f: 0.0, len: 0 };
    let args = [val_i64(1)];
    assert_eq!(
        unsafe {
            ms_script_call(
                std::ptr::null_mut(),
                func.as_ptr(),
                args.as_ptr(),
                1,
                &mut out,
                std::ptr::null_mut(),
                0,
            )
        },
        MS_ERR_NULL
    );
    assert_eq!(
        unsafe {
            ms_script_call(
                ptr,
                func.as_ptr(),
                args.as_ptr(),
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        },
        MS_ERR_NULL
    );

    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);
}

#[test]
fn ffi_script_thread_ownership_is_enforced() {
    let ptr = unsafe { create_script("func go(x) { x }") };
    let addr = ptr as usize;

    // Calling from another thread must fail cleanly instead of racing the
    // Rc-backed internals.
    let handle = std::thread::spawn(move || {
        let p = addr as *mut brook::ffi::MsScript;
        let func = CString::new("go").unwrap();
        let mut out = MsValOut { tag: 0, i: 0, f: 0.0, len: 0 };
        let args = [val_i64(1)];
        let code = unsafe {
            ms_script_call(
                p,
                func.as_ptr(),
                args.as_ptr(),
                1,
                &mut out,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(code, MS_ERR_THREAD, "cross-thread call must be rejected");

        // A cross-thread free must NOT take ownership: the owner can still
        // free the script afterwards.
        assert_eq!(unsafe { ms_script_free(p) }, MS_ERR_THREAD);
    });
    handle.join().unwrap();

    // The owner still works, and can still free.
    let (code, out, _) = unsafe { call_script(ptr, "go", &[val_i64(7)]) };
    assert_eq!(code, MS_OK);
    assert_eq!(out.i, 7);
    assert_eq!(unsafe { ms_script_free(ptr) }, MS_OK);
}
