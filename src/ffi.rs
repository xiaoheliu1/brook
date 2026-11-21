//! Minimal C ABI for embedding the engine on Android (JNI / native callers).
//!
//! Deliberately tiny: no host callbacks, no I/O, no opaque handles cross this
//! boundary. All Rust panics are caught before they could unwind through
//! `extern "C"` (which would be UB).
//!
//! Two layers, both following the same error protocol:
//!
//! * **One-shot** — [`ms_call_i64`]: compile `source`, call `func(arg) -> i64`,
//!   free. Convenient for smoke tests; it recompiles on every call.
//! * **Retained** — [`ms_script_create`] / [`ms_script_create_cached`] hand
//!   back an opaque [`MsScript`] handle that keeps the compiled bytecode (and
//!   its source, for diagnostics) across calls: [`ms_script_call`] passes
//!   tagged scalar/string arguments and receives a scalar/string result,
//!   [`ms_script_reload`] swaps the bytecode in place (hot reload), and
//!   [`ms_script_free`] releases it. [`ms_script_create_cached`] routes
//!   through `Engine::compile_cached`, so a fresh `.sbc` skips compilation
//!   entirely — the intended Android assets / network-payload path.
//!
//! Error protocol is two-step, and it never allocates across the boundary:
//!
//! 1. Every entry point returns an [`MS_OK`] / error code.
//! 2. On failure the same thread can read the message with [`ms_last_error`]
//!    (copied into a caller-owned buffer), the location with
//!    [`ms_last_error_line`] / [`ms_last_error_col`] (`0` means "no location",
//!    e.g. an unknown function name), and the multi-line diagnostic with
//!    [`ms_last_report`] (rendered against the script's own source, with
//!    caret and call stack).
//!
//! **Thread ownership**: an [`MsScript`] belongs to the thread that created
//! it. Calling, reloading or freeing from another thread returns
//! [`MS_ERR_THREAD`] instead of corrupting memory (the engine is `Rc`-backed
//! and deliberately not `Send`; one engine per thread is a design rule, and
//! the guard turns misuse into a clean error rather than UB).
//!
//! C sketch of the retained layer:
//!
//! ```c
//! typedef struct MsScript MsScript;
//! typedef struct { uint32_t tag;  int64_t i;  double f;  const char* s; } MsVal;
//! typedef struct { uint32_t tag;  int64_t i;  double f;  size_t   len; } MsValOut;
//!
//! int32_t ms_script_create(const char* source, const char* name, MsScript** out);
//! int32_t ms_script_create_cached(const char* source, const char* name,
//!                                 const char* cache_path, MsScript** out);
//! int32_t ms_script_call(MsScript* s, const char* func,
//!                        const MsVal* args, size_t nargs,
//!                        MsValOut* out, char* out_buf, size_t out_cap);
//! int32_t ms_script_reload(MsScript* s, const char* source, const char* name);
//! int32_t ms_script_free(MsScript* s);
//! size_t  ms_last_report(char* buf, size_t cap);
//! ```
//!
//! Only exported when the crate is built as a `cdylib` (see `scripts/
//! android_build.sh`). The `rlib` used by Rust hosts is unaffected. All new
//! symbols are additive exports — [`ms_abi_version`] stays `1`, and callers
//! that only test `!= MS_OK` keep working.

use crate::engine::{Engine, Script};
use crate::error::{Error, ErrorKind};
use crate::ir::MAX_OPERANDS;
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::thread::ThreadId;

pub const MS_OK: c_int = 0;
pub const MS_ERR_NULL: c_int = 1;
pub const MS_ERR_UTF8: c_int = 2;
/// The source could not be turned into bytecode (lex / parse / type / io), or
/// the compiled function's result cannot be carried by this ABI (not a scalar
/// or str).
pub const MS_ERR_ENGINE: c_int = 3;
pub const MS_ERR_PANIC: c_int = 4;
/// The call itself failed: unknown function, wrong arity, argument type
/// mismatch, gas exhausted, heap limit, arithmetic overflow, division by zero,
/// or a host callback error. The script may well be fine; this run was not.
pub const MS_ERR_RUNTIME: c_int = 5;
/// A string result did not fit the caller's buffer. Nothing was written; the
/// required size (excluding the terminator) is in `MsValOut.len`, so the
/// caller can grow the buffer and call again.
pub const MS_ERR_BUFFER: c_int = 6;
/// An [`MsScript`] was used from a thread other than the one that created it.
/// The engine is `Rc`-backed and deliberately not `Send`; this converts what
/// would be UB into a clean error.
pub const MS_ERR_THREAD: c_int = 7;

/// Argument/result tags for [`ms_script_call`].
pub const MS_VAL_I64: u32 = 1;
pub const MS_VAL_F64: u32 = 2;
pub const MS_VAL_BOOL: u32 = 3;
pub const MS_VAL_STR: u32 = 4;
pub const MS_VAL_UNIT: u32 = 5;

/// Last failure on this thread, mirroring the two-step C protocol above.
#[derive(Default)]
struct LastError {
    msg: String,
    /// `0` when the error carries no source location.
    line: u32,
    col: u32,
    /// Multi-line diagnostic rendered against the script's own source, when
    /// the source was available at the failure site. Empty otherwise.
    report: String,
}

thread_local! {
    static LAST_ERROR: RefCell<LastError> = RefCell::new(LastError::default());
}

/// A failure on its way out of the closure: code, message and span travel
/// together so one `record` call fills the message buffer and both getters.
struct Failure {
    code: c_int,
    msg: String,
    span: Option<Span>,
    report: String,
}

impl Failure {
    fn of_reported(e: Error, report: String) -> Self {
        let code = match e.kind {
            ErrorKind::Runtime => MS_ERR_RUNTIME,
            _ => MS_ERR_ENGINE,
        };
        Self { code, msg: e.to_string(), span: e.span, report }
    }

    /// The script ran but returned something this ABI cannot carry.
    fn contract(msg: impl Into<String>) -> Self {
        Self { code: MS_ERR_ENGINE, msg: msg.into(), span: None, report: String::new() }
    }

    fn null(msg: impl Into<String>) -> Self {
        Self { code: MS_ERR_NULL, msg: msg.into(), span: None, report: String::new() }
    }
}

fn record(f: Failure) -> c_int {
    LAST_ERROR.with(|e| {
        let mut e = e.borrow_mut();
        e.msg = f.msg;
        e.line = f.span.map_or(0, |s| s.line as u32);
        e.col = f.span.map_or(0, |s| s.col as u32);
        e.report = f.report;
    });
    f.code
}

unsafe fn as_str<'a>(p: *const c_char) -> Result<&'a str, Failure> {
    if p.is_null() {
        return Err(Failure::null("null pointer argument"));
    }
    CStr::from_ptr(p).to_str().map_err(|e| Failure {
        code: MS_ERR_UTF8,
        msg: format!("invalid UTF-8: {}", e),
        span: None,
        report: String::new(),
    })
}

/// Compile `source`, call `func(arg)` and write its i64 result to `out`.
/// Returns [`MS_OK`] on success, otherwise an error code; retrieve the message
/// with [`ms_last_error`], the location with [`ms_last_error_line`] /
/// [`ms_last_error_col`], and the multi-line diagnostic with
/// [`ms_last_report`]. `func` must take one i64 and return an i64.
///
/// Recompiles on every call; retain an [`MsScript`] instead when the same
/// script runs repeatedly.
///
/// # Safety
/// `source` and `func` must be valid NUL-terminated C strings, `out` must be a
/// writable aligned pointer to an `i64`.
#[no_mangle]
pub unsafe extern "C" fn ms_call_i64(
    source: *const c_char,
    func: *const c_char,
    arg: i64,
    out: *mut i64,
) -> c_int {
    if out.is_null() {
        return record(Failure::null("null out pointer"));
    }
    let body = || -> Result<i64, Failure> {
        let source = as_str(source)?;
        let func = as_str(func)?;
        let engine = Engine::new();
        // The source is at hand, so failures can carry the multi-line
        // diagnostic for `ms_last_report` without a second pass.
        let script = engine.load_str(source).map_err(|e| {
            let report = e.report(source, func);
            Failure::of_reported(e, report)
        })?;
        let value = engine.call(&script, func, vec![Value::I64(arg)]).map_err(|e| {
            let report = e.report(source, func);
            Failure::of_reported(e, report)
        })?;
        match value {
            Value::I64(n) => Ok(n),
            other => Err(Failure::contract(format!(
                "expected i64 result, got {}",
                other.ty()
            ))),
        }
    };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(n)) => {
            *out = n;
            MS_OK
        }
        Ok(Err(f)) => record(f),
        Err(_) => record(Failure {
            code: MS_ERR_PANIC,
            msg: "internal panic caught at the FFI boundary".into(),
            span: None,
            report: String::new(),
        }),
    }
}

/// 按字节上限回退到 UTF-8 字符边界的截断长度（D-046 5.13：字节截断会
/// 切断 CJK 字符，C 侧拿到非法 UTF-8 序列）。
fn utf8_cut(s: &str, max: usize) -> usize {
    let mut n = s.len().min(max);
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// Copy the last error message (NUL-terminated) into `buf`. Returns the number
/// of bytes written, excluding the terminator. A zero-length buffer is safe.
///
/// # Safety
/// `buf` must be null or point to at least `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ms_last_error(buf: *mut c_char, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    LAST_ERROR.with(|e| {
        let text = e.borrow();
        let bytes = text.msg.as_bytes();
        let n = utf8_cut(&text.msg, len - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
        *buf.add(n) = 0;
        n
    })
}

/// 1-based line of the last error on this thread, or `0` if it has no source
/// location (unknown function, arity/type contract mismatch, panic).
#[no_mangle]
pub extern "C" fn ms_last_error_line() -> u32 {
    LAST_ERROR.with(|e| e.borrow().line)
}

/// 1-based column of the last error on this thread; same `0` convention as
/// [`ms_last_error_line`]. Columns count characters, not bytes, so a caret
/// placed by a C-side UI lines up with CJK source text.
#[no_mangle]
pub extern "C" fn ms_last_error_col() -> u32 {
    LAST_ERROR.with(|e| e.borrow().col)
}

/// ABI/version marker so a JNI loader can detect a mismatched `.so`.
#[no_mangle]
pub extern "C" fn ms_abi_version() -> u32 {
    // Still 1: `MS_ERR_RUNTIME`, `ms_last_error_line` and `ms_last_error_col`
    // were added as *new* exports and a new code value. Existing callers keep
    // linking and keep seeing `!= MS_OK` for every failure, so the ABI is
    // compatible; a caller that wants the richer error model can probe these
    // symbols (dlsym) rather than bumping a version it does not read. The
    // retained-handle layer (`ms_script_*`, `MS_ERR_BUFFER`, `MS_ERR_THREAD`,
    // `ms_last_report`) followed the same additive rule.
    1
}

// ---------------------------------------------------------------------------
// Retained-handle layer: compile once, call many times, reload in place.
// ---------------------------------------------------------------------------

/// An argument (or the tag of a result) for [`ms_script_call`].
///
/// Flat struct instead of a union: trivially constructible from Java via a
/// `ByteBuffer`/JNA struct, and every field has one documented meaning per
/// tag. `s` is read as `(s, i)` — a pointer to `i` UTF-8 bytes, which need
/// **not** be NUL-terminated (JNI byte arrays map straight onto it).
///
/// # Safety (as an argument)
/// For `MS_VAL_STR`, `s` must point to at least `i` readable bytes for the
/// duration of the call; every other tag ignores `s`.
#[repr(C)]
pub struct MsVal {
    pub tag: u32,
    /// I64 payload; BOOL stores 0/1; STR stores the byte length; UNIT ignores.
    pub i: i64,
    /// F64 payload.
    pub f: f64,
    /// STR payload: pointer to `i` UTF-8 bytes (may be null when `i == 0`).
    pub s: *const c_char,
}

/// The result of [`ms_script_call`], written on success — and, for string
/// results, also on [`MS_ERR_BUFFER`] so the caller learns what to allocate.
#[repr(C)]
pub struct MsValOut {
    pub tag: u32,
    /// I64 payload; BOOL stores 0/1.
    pub i: i64,
    /// F64 payload.
    pub f: f64,
    /// STR results: the full byte length of the string, excluding the NUL
    /// terminator. `0` for every other tag.
    pub len: usize,
}

/// A compiled script retained across calls, plus the engine that compiled it.
///
/// Owned by the thread that created it (see the module docs): the thread id is
/// checked on every entry point so cross-thread misuse fails with
/// [`MS_ERR_THREAD`] instead of racing the `Rc`-backed internals.
pub struct MsScript {
    engine: Engine,
    script: Script,
    owner: ThreadId,
}

/// Borrow the handle for use on its owning thread.
unsafe fn script_ref<'a>(script: *mut MsScript) -> Result<&'a MsScript, Failure> {
    let s = script
        .as_ref()
        .ok_or_else(|| Failure::null("null script handle"))?;
    if s.owner != std::thread::current().id() {
        return Err(Failure {
            code: MS_ERR_THREAD,
            msg: "MsScript used from a thread other than its owner; \
                  create, call, reload and free on one thread"
                .into(),
            span: None,
            report: String::new(),
        });
    }
    Ok(s)
}

/// Mutably borrow the handle for use on its owning thread (reload).
unsafe fn script_mut<'a>(script: *mut MsScript) -> Result<&'a mut MsScript, Failure> {
    let ptr = script
        .as_mut()
        .ok_or_else(|| Failure::null("null script handle"))?;
    if ptr.owner != std::thread::current().id() {
        return Err(Failure {
            code: MS_ERR_THREAD,
            msg: "MsScript used from a thread other than its owner; \
                  create, call, reload and free on one thread"
                .into(),
            span: None,
            report: String::new(),
        });
    }
    Ok(ptr)
}

/// Convert one tagged C value into a script argument.
///
/// # Safety
/// For `MS_VAL_STR` with `i > 0`, `s` must point to `i` readable bytes.
unsafe fn msval_to_value(v: &MsVal) -> Result<Value, Failure> {
    match v.tag {
        MS_VAL_I64 => Ok(Value::I64(v.i)),
        MS_VAL_F64 => Ok(Value::F64(v.f)),
        MS_VAL_BOOL => Ok(Value::Bool(v.i != 0)),
        MS_VAL_UNIT => Ok(Value::Unit),
        MS_VAL_STR => {
            let len =
                usize::try_from(v.i).map_err(|_| Failure::contract("negative string length"))?;
            if len == 0 {
                return Ok(Value::from(""));
            }
            if v.s.is_null() {
                return Err(Failure::null("null string argument"));
            }
            let bytes = std::slice::from_raw_parts(v.s.cast::<u8>(), len);
            let s = std::str::from_utf8(bytes).map_err(|e| Failure {
                code: MS_ERR_UTF8,
                msg: format!("invalid UTF-8 string argument: {}", e),
                span: None,
                report: String::new(),
            })?;
            Ok(Value::from(s))
        }
        other => Err(Failure::contract(format!("unknown value tag {}", other))),
    }
}

/// Write the script result into the caller's output struct (and buffer, for
/// strings). A string that does not fit yields [`MS_ERR_BUFFER`] with
/// `out.len` set to the required size — nothing is written, so the caller can
/// grow the buffer and retry.
///
/// # Safety
/// `out` must be writable; when the result is a string and `out_cap` is large
/// enough, `out_buf` must be writable for `out.len + 1` bytes.
unsafe fn value_to_out(
    value: Value,
    out: &mut MsValOut,
    out_buf: *mut c_char,
    out_cap: usize,
) -> Result<(), Failure> {
    match value {
        Value::I64(n) => {
            out.tag = MS_VAL_I64;
            out.i = n;
            out.f = 0.0;
            out.len = 0;
            Ok(())
        }
        Value::F64(x) => {
            out.tag = MS_VAL_F64;
            out.i = 0;
            out.f = x;
            out.len = 0;
            Ok(())
        }
        Value::Bool(b) => {
            out.tag = MS_VAL_BOOL;
            out.i = if b { 1 } else { 0 };
            out.f = 0.0;
            out.len = 0;
            Ok(())
        }
        Value::Unit => {
            out.tag = MS_VAL_UNIT;
            out.i = 0;
            out.f = 0.0;
            out.len = 0;
            Ok(())
        }
        Value::Str(s) => {
            let bytes = s.as_bytes();
            let required = bytes.len();
            out.tag = MS_VAL_STR;
            out.i = 0;
            out.f = 0.0;
            out.len = required;
            if out_buf.is_null() || out_cap < required + 1 {
                return Err(Failure {
                    code: MS_ERR_BUFFER,
                    msg: format!(
                        "string result needs {} bytes (with terminator); buffer holds {}",
                        required + 1,
                        out_cap
                    ),
                    span: None,
                    report: String::new(),
                });
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf.cast::<u8>(), required);
            *out_buf.add(required) = 0;
            Ok(())
        }
        // Handles are host-owned by design; a script that received one through
        // the Rust API cannot run here, but a script *returning* one can. The
        // contract is scalars and strings only.
        Value::Handle(h) => Err(Failure::contract(format!(
            "handle results cannot cross the C ABI ({})",
            h.type_name
        ))),
        Value::Tuple(_) => Err(Failure::contract(
            "tuple results cannot cross the C ABI; return a scalar or str",
        )),
        // ADR-018: `MsVal` is a flat scalar struct; array results must cross
        // the boundary through the Rust API (`Vec<T>` HostResult) instead.
        Value::Arr(_) => Err(Failure::contract(
            "array results cannot cross the C ABI; return a scalar or str",
        )),
    }
}

fn panic_failure() -> Failure {
    Failure {
        code: MS_ERR_PANIC,
        msg: "internal panic caught at the FFI boundary".into(),
        span: None,
        report: String::new(),
    }
}

/// Compile `source` into a retained [`MsScript`]. The handle keeps the
/// bytecode *and* the source text (so failures render multi-line reports), and
/// belongs to the calling thread.
///
/// `name` labels diagnostics; a null name becomes `<script>`. On failure
/// `*out` is untouched and the compile error (with its report) is recorded.
///
/// # Safety
/// `source` must be a valid NUL-terminated C string; `name` may be null;
/// `out` must be a writable aligned pointer.
#[no_mangle]
pub unsafe extern "C" fn ms_script_create(
    source: *const c_char,
    name: *const c_char,
    out: *mut *mut MsScript,
) -> c_int {
    if out.is_null() {
        return record(Failure::null("null out pointer"));
    }
    let body = || -> Result<*mut MsScript, Failure> {
        let source = as_str(source)?;
        let name = if name.is_null() { "<script>" } else { as_str(name)? };
        let engine = Engine::new();
        let script = engine.compile(source, name).map_err(|e| {
            let report = e.report(source, name);
            Failure::of_reported(e, report)
        })?;
        Ok(Box::into_raw(Box::new(MsScript {
            engine,
            script,
            owner: std::thread::current().id(),
        })))
    };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(ptr)) => {
            *out = ptr;
            MS_OK
        }
        Ok(Err(f)) => record(f),
        Err(_) => record(panic_failure()),
    }
}

/// Like [`ms_script_create`], but routed through `Engine::compile_cached`: if
/// `cache_path` holds a fresh `.sbc` for this source (and host registry), it is
/// decoded instead of compiled — the intended path when the source comes from
/// Android assets or a network payload. A stale or unreadable cache is
/// transparently rebuilt (best-effort write), so a read-only directory still
/// loads fine.
///
/// # Safety
/// `source` and `cache_path` must be valid NUL-terminated C strings; `name`
/// may be null; `out` must be a writable aligned pointer.
#[no_mangle]
pub unsafe extern "C" fn ms_script_create_cached(
    source: *const c_char,
    name: *const c_char,
    cache_path: *const c_char,
    out: *mut *mut MsScript,
) -> c_int {
    if out.is_null() {
        return record(Failure::null("null out pointer"));
    }
    let body = || -> Result<*mut MsScript, Failure> {
        let source = as_str(source)?;
        let cache = as_str(cache_path)?;
        let name = if name.is_null() { "<script>" } else { as_str(name)? };
        let engine = Engine::new();
        let script = engine.compile_cached(source, name, cache).map_err(|e| {
            let report = e.report(source, name);
            Failure::of_reported(e, report)
        })?;
        Ok(Box::into_raw(Box::new(MsScript {
            engine,
            script,
            owner: std::thread::current().id(),
        })))
    };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(ptr)) => {
            *out = ptr;
            MS_OK
        }
        Ok(Err(f)) => record(f),
        Err(_) => record(panic_failure()),
    }
}

/// Call `func` on a retained script with tagged arguments.
///
/// Results are scalars (`I64`/`F64`/`BOOL`/`UNIT`) or `STR`. A string result is
/// written NUL-terminated into `out_buf`; when it does not fit, [`MS_ERR_BUFFER`]
/// is returned with `out->len` carrying the required size and nothing written —
/// grow the buffer and call again. Every failure (including this one) records
/// [`ms_last_error`] / [`ms_last_report`] as usual.
///
/// # Safety
/// `script` must be a handle returned by [`ms_script_create`] /
/// [`ms_script_create_cached`] that has not been freed, used on its owning
/// thread; `func` a valid NUL-terminated C string; `args` must point to
/// `nargs` readable `MsVal`s (string payloads per `MsVal`'s safety note);
/// `out` must be writable; `out_buf`/`out_cap` follow the string rule above.
#[no_mangle]
pub unsafe extern "C" fn ms_script_call(
    script: *mut MsScript,
    func: *const c_char,
    args: *const MsVal,
    nargs: usize,
    out: *mut MsValOut,
    out_buf: *mut c_char,
    out_cap: usize,
) -> c_int {
    if out.is_null() {
        return record(Failure::null("null out pointer"));
    }
    let body = || -> Result<(), Failure> {
        let s = script_ref(script)?;
        let func = as_str(func)?;
        if nargs > MAX_OPERANDS {
            return Err(Failure::contract(format!(
                "{} arguments exceed the ABI maximum of {}",
                nargs, MAX_OPERANDS
            )));
        }
        if nargs > 0 && args.is_null() {
            return Err(Failure::null("null args pointer"));
        }
        let mut values = Vec::with_capacity(nargs);
        for i in 0..nargs {
            values.push(msval_to_value(args.add(i).as_ref().ok_or_else(|| Failure::null("null args pointer"))?)?);
        }
        let result = s.engine.call(&s.script, func, values).map_err(|e| {
            let report = e.report(&s.script.source, &s.script.name);
            Failure::of_reported(e, report)
        })?;
        value_to_out(result, &mut *out, out_buf, out_cap)
    };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => MS_OK,
        Ok(Err(f)) => record(f),
        Err(_) => record(panic_failure()),
    }
}

/// Recompile `source` and replace the handle's bytecode in place — the hot
/// reload path, exposed over the C ABI. On failure the old bytecode stays
/// untouched (and usable), per the engine's reload guarantee.
///
/// # Safety
/// `script` must be a live handle on its owning thread; `source` a valid
/// NUL-terminated C string; `name` may be null (keeps the current name).
#[no_mangle]
pub unsafe extern "C" fn ms_script_reload(
    script: *mut MsScript,
    source: *const c_char,
    name: *const c_char,
) -> c_int {
    let body = || -> Result<(), Failure> {
        let s = script_mut(script)?;
        let source = as_str(source)?;
        let label = if name.is_null() {
            s.script.name.clone()
        } else {
            as_str(name)?.to_owned()
        };
        let fresh = s.engine.compile(source, label.clone()).map_err(|e| {
            let report = e.report(source, &label);
            Failure::of_reported(e, report)
        })?;
        s.script = fresh;
        Ok(())
    };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => MS_OK,
        Ok(Err(f)) => record(f),
        Err(_) => record(panic_failure()),
    }
}

/// Release a retained script. Ownership is taken only on the owning thread:
/// a null handle reports [`MS_ERR_NULL`], a cross-thread free reports
/// [`MS_ERR_THREAD`] **without** taking ownership (the allocation stays valid
/// so the owner can free it later) — never UB.
///
/// # Safety
/// `script` must be null or a live handle from [`ms_script_create`] /
/// [`ms_script_create_cached`], and must not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn ms_script_free(script: *mut MsScript) -> c_int {
    match script.as_ref() {
        None => record(Failure::null("null script handle")),
        Some(s) if s.owner != std::thread::current().id() => record(Failure {
            code: MS_ERR_THREAD,
            msg: "MsScript freed from a thread other than its owner; \
                  ownership not taken, free it on the owning thread"
                .into(),
            span: None,
            report: String::new(),
        }),
        Some(_) => {
            // Drop 链（Engine/Script/堆字符串）理论上有 panic 面——
            // extern "C" 里穿出 unwind 是 UB，边界兜住并上报（D-046 3.10：
            // 这是唯一没包 catch_unwind 的入口）。
            match catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(script)))) {
                Ok(()) => MS_OK,
                Err(_) => record(Failure {
                    code: MS_ERR_PANIC,
                    msg: "internal panic while releasing the script handle".into(),
                    span: None,
                    report: String::new(),
                }),
            }
        }
    }
}

/// Copy the last error's multi-line diagnostic (rendered against the script's
/// own source, with `--> file:line:col`, caret and call stack) into `buf`,
/// NUL-terminated. Same truncation convention as [`ms_last_error`]; empty when
/// the failure carried no source (e.g. a null argument).
///
/// # Safety
/// `buf` must be null or point to at least `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ms_last_report(buf: *mut c_char, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    LAST_ERROR.with(|e| {
        let text = e.borrow();
        let bytes = text.report.as_bytes();
        let n = utf8_cut(&text.report, len - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
            *buf.add(n) = 0;
        }
        n
    })
}
