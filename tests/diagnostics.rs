//! Diagnostics: source locations and rendered error reports.

use brook::Engine;

/// Columns are character positions, not byte offsets. A line containing CJK
/// text used to report the byte column, so the caret pointed past the token.
#[test]
fn column_counts_characters_not_bytes() {
    let engine = Engine::new();
    let err = engine
        .load_str("func f() -> i64 {\n    let s = \"中文\" + 1\n}")
        .unwrap_err();
    let span = err.span.expect("type error carries a span");
    assert_eq!(span.line, 2);
    assert_eq!(
        span.col, 18,
        "the `+` is the 18th character on line 2: {}",
        err
    );
    assert!(err.to_string().contains("at 2:18"), "{}", err);
}

/// Same script with ASCII-only text must report the *same* column: the fix
/// changed what a column means for multi-byte lines, not for plain ones.
#[test]
fn ascii_column_is_unchanged() {
    let engine = Engine::new();
    let err = engine
        .load_str("func f() -> i64 {\n    let s = \"ab\" + 1\n}")
        .unwrap_err();
    let span = err.span.expect("type error carries a span");
    assert_eq!((span.line, span.col), (2, 18), "{}", err);
}

/// Runtime errors carry the interpreted call stack, outermost frame first.
#[test]
fn runtime_error_records_the_call_stack() {
    let engine = Engine::new();
    let script = engine
        .load_str(
            "func c(n: i64) -> i64 { n / 0 }
func b(n: i64) -> i64 { c(n) }
func a(n: i64) -> i64 { b(n) + 1 }
func main() -> i64 { a(3) }",
        )
        .unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    let names: Vec<&str> = err.trace.iter().map(|f| f.func.as_str()).collect();
    assert_eq!(names, ["main", "a", "b", "c"], "{}", err);
    // Every caller reports the call site, not its own body start.
    assert_eq!(
        (err.trace[0].line, err.trace[0].col),
        (4, 22),
        "main: {}",
        err
    );
    assert_eq!((err.trace[1].line, err.trace[1].col), (3, 25), "a: {}", err);
    assert_eq!((err.trace[2].line, err.trace[2].col), (2, 25), "b: {}", err);
    // The innermost frame points at the faulting instruction itself.
    assert_eq!((err.trace[3].line, err.trace[3].col), (1, 27), "c: {}", err);
    assert_eq!(err.innermost().map(|f| f.func.as_str()), Some("c"));
}

/// A depth-limit failure happens *at* the call instruction, so the trace keeps
/// growing with the recursion instead of collapsing to one frame.
#[test]
fn depth_limit_trace_keeps_the_frames() {
    let engine = Engine::new().with_limits(1_000_000, 8);
    let script = engine
        .load_str("func down(n: i64) -> i64 { down(n + 1) }\nfunc main() -> i64 { down(0) }")
        .unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    assert!(err.message.contains("call depth limit exceeded"), "{}", err);
    assert_eq!(
        err.trace.len(),
        8,
        "main + 7 nested `down` (depth cap 8): {}",
        err
    );
    assert!(
        err.trace
            .iter()
            .all(|f| f.func == "down" || f.func == "main"),
        "{}",
        err
    );
}

/// Compile-time errors have no call stack, and that must not panic.
#[test]
fn compile_errors_have_no_trace() {
    let engine = Engine::new();
    let err = engine.load_str("func f() { 1 + \"a\" }").unwrap_err();
    assert!(err.trace.is_empty(), "{:?}", err.trace);
}

/// A host function's own error kind survives the trip through the VM.
#[test]
fn host_error_kind_is_preserved() {
    use brook::{register_fn, Error, ErrorKind};
    let mut engine = Engine::new();
    register_fn!(engine, "fail_io", () -> std::result::Result<(), Error>, || {
        Err(Error::io("disk gone"))
    })
    .unwrap();
    let script = engine.load_str("func f() { fail_io() }").unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Io, "{}", err);
    assert!(err.message.contains("disk gone"), "{}", err);
    assert!(err.span.is_some());
    assert_eq!(err.trace.last().map(|f| f.func.as_str()), Some("f"));
}

/// Display column of a byte offset: tabs are four columns wide, CJK glyphs two.
fn display_col(line: &str, byte: usize) -> usize {
    line[..byte]
        .chars()
        .map(|c| match c {
            '\t' => 4,
            '\u{2E80}'..='\u{FF60}' => 2,
            _ => 1,
        })
        .sum()
}

/// The snippet's caret must sit at the same display column as the offending
/// token — including when the line contains wide (CJK) glyphs or tabs.
fn caret_alignment(report: &str, line_no: usize) -> (usize, usize) {
    let lines: Vec<&str> = report.lines().collect();
    let prefix = format!("{} | ", line_no);
    let idx = lines
        .iter()
        .position(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no source line in: {:?}", report));
    let source = &lines[idx][prefix.len()..];
    let caret_prefix = format!("{} | ", " ".repeat(line_no.to_string().len()));
    let caret = &lines[idx + 1][caret_prefix.len()..];
    let col = display_col(
        source,
        source.find('+').expect("line contains the operator"),
    );
    let head = caret.find('^').expect("caret present");
    (col, head)
}

#[test]
fn compile_error_shows_the_offending_line() {
    let engine = Engine::new();
    let src = "func f() -> i64 {\n    let s = \"中文\" + 1\n}\n";
    let err = engine.load_str(src).unwrap_err();
    let report = err.report(src, "price.bk");

    assert!(report.starts_with("[type] error at 2:18:"), "{}", report);
    assert!(report.contains("  --> price.bk:2:18"), "{}", report);
    assert!(
        report.contains("2 |     let s = \"中文\" + 1"),
        "{}",
        report
    );
    // Two CJK glyphs are two columns each in a terminal; the caret must follow
    // the rendered width, not the character count.
    assert_eq!(caret_alignment(&report, 2), (19, 19), "{}", report);
}

#[test]
fn tabs_are_expanded_before_placing_the_caret() {
    let engine = Engine::new();
    let src = "func f() -> i64 {\n\tlet s = \"x\" + 1\n}\n";
    let err = engine.load_str(src).unwrap_err();
    let report = err.report(src, "tab.bk");
    assert_eq!(caret_alignment(&report, 2), (16, 16), "{}", report);
}

#[test]
fn runtime_error_renders_the_stack() {
    let engine = Engine::new();
    let src = "func c(n: i64) -> i64 {\n    n / 0\n}\nfunc b(n: i64) -> i64 {\n    c(n)\n}\nfunc main() -> i64 {\n    b(3)\n}";
    let script = engine.load_str(src).unwrap();
    let err = engine.call(&script, "main", vec![]).unwrap_err();
    let report = script.report(&err);

    assert!(report.contains("division by zero"), "{}", report);
    assert!(
        report.contains("= call stack (outermost first):"),
        "{}",
        report
    );
    assert!(report.contains("1. main (8:5)"), "{}", report);
    assert!(report.contains("2. b (5:5)"), "{}", report);
    assert!(report.contains("3. c (2:7)"), "{}", report);
}

/// A 256-frame recursion must not produce a 256-line message.
#[test]
fn deep_stack_is_elided() {
    let engine = Engine::new().with_limits(1_000_000, 40);
    let src =
        "func down(n: i64) -> i64 {\n    down(n + 1)\n}\nfunc main() -> i64 {\n    down(0)\n}";
    let script = engine.load_str(src).unwrap();
    let report = script.report(&engine.call(&script, "main", vec![]).unwrap_err());
    assert!(report.contains("\n      ...\n"), "{}", report);
    let frames = report.matches(" down (2:5)").count();
    assert!(frames <= 12, "{} frames printed:\n{}", frames, report);
    // The last frame is still the innermost one: elision trims the middle.
    assert!(report.trim_end().ends_with("12. down (2:5)"), "{}", report);
}

/// Scripts loaded through the bytecode cache keep reporting against source.
#[test]
fn cached_script_reports_against_source() {
    let engine = Engine::new();
    let dir = std::env::temp_dir().join("ms_diag_cache");
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("diag.sbc");
    let src = "func f() -> i64 {\n    1 / 0\n}\n";
    let script = engine.compile_cached(src, "diag.bk", &cache).unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert!(
        script.report(&err).contains("diag.bk:2:7"),
        "{}",
        script.report(&err)
    );
    let _ = std::fs::remove_file(&cache);
}
