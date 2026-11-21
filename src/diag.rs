//! Rendering an [`Error`] against the source it came from.
//!
//! This is a *reporting* helper, not part of the compiler: it never runs unless
//! an error already happened, and it allocates only strings. The engine keeps
//! the compiled script's source so a host can print a diagnostic without
//! tracking the text itself.
//!
//! ```text
//! [type] error at 2:18: arithmetic: expected i64, found str
//!   --> price.vt:2:18
//!    |
//!  2 |     let s = "中文" + 1
//!    |                    ^
//! ```
//!
//! Columns are *display* columns: tabs expand to four spaces and East Asian
//! wide characters count as two, so the caret lands under the token in a
//! terminal instead of drifting left by one column per CJK glyph.

use crate::error::Error;

/// Tab stop used when rendering a source line.
const TAB_WIDTH: usize = 4;
/// Do not let a pathological span paint the whole terminal.
const MAX_CARET: usize = 60;
/// Frames printed before the middle of a deep recursion is elided.
const MAX_TRACE: usize = 12;

/// Render `err` against `src`, labelling locations as `name:line:col`.
pub fn render(err: &Error, src: &str, name: &str) -> String {
    let mut out = String::new();
    out.push_str(&err.to_string());

    if let Some(span) = err.span {
        let label = if name.is_empty() {
            format!("{}:{}", span.line, span.col)
        } else {
            format!("{}:{}:{}", name, span.line, span.col)
        };
        out.push_str(&format!("\n  --> {}\n", label));

        if let Some(line) = line_text(src, span.line) {
            let gutter = span.line.to_string().len();
            out.push_str(&format!("{} |\n", " ".repeat(gutter)));
            out.push_str(&format!("{} | {}\n", span.line, expand_tabs(&line)));
            out.push_str(&format!(
                "{} | {}{}\n",
                " ".repeat(gutter),
                " ".repeat(caret_offset(&line, span.col)),
                "^".repeat(caret_width(src, span, &line))
            ));
        }
    }

    if !err.trace.is_empty() {
        out.push_str("\n  = call stack (outermost first):\n");
        let mut n = 0;
        for frame in trace_frames(&err.trace) {
            match frame {
                Some(f) => {
                    n += 1;
                    if f.line == 0 {
                        out.push_str(&format!("      {}. {} (no location)\n", n, f.func));
                    } else {
                        out.push_str(&format!("      {}. {} ({}:{})\n", n, f.func, f.line, f.col));
                    }
                }
                None => out.push_str("      ...\n"),
            }
        }
    }

    out
}

/// The full text of 1-based `line`, without its terminator.
fn line_text(src: &str, line: usize) -> Option<String> {
    if line == 0 {
        return None;
    }
    src.split('\n').nth(line - 1).map(|l| l.trim_end_matches('\r').to_string())
}

/// Replace tabs with spaces so the caret arithmetic matches what is printed.
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            out.push_str(&" ".repeat(TAB_WIDTH));
        } else {
            out.push(c);
        }
    }
    out
}

/// Display width of the characters before 1-based column `col`.
fn caret_offset(line: &str, col: usize) -> usize {
    line.chars().take(col.saturating_sub(1)).map(display_width).sum()
}

/// Display width of one source character as it is printed: tabs occupy a full
/// tab stop because [`expand_tabs`] already turned them into spaces.
fn display_width(c: char) -> usize {
    if c == '\t' {
        TAB_WIDTH
    } else {
        char_width(c)
    }
}

/// Caret length in display columns.
///
/// Compile-time spans carry byte offsets, so the width is exact when the whole
/// token sits on one line. Runtime spans only carry line/column (the VM stores
/// two `u32`s per instruction to keep dispatch cheap), and a zero-length span
/// still gets one caret so the position stays visible.
fn caret_width(src: &str, span: crate::span::Span, line: &str) -> usize {
    let offset = caret_offset(line, span.col);
    let total: usize = line.chars().map(display_width).sum();
    let room = total.saturating_sub(offset).max(1);

    let exact = span.start < span.end
        && span.end <= src.len()
        && src.is_char_boundary(span.start)
        && src.is_char_boundary(span.end)
        && !src[span.start..span.end].contains('\n');

    let width = if exact {
        src[span.start..span.end].chars().map(char_width).sum::<usize>()
    } else {
        1
    };
    width.clamp(1, room.min(MAX_CARET))
}

/// Print the head and tail of a deep stack, eliding the middle.
fn trace_frames(trace: &[crate::error::TraceFrame]) -> impl Iterator<Item = Option<&crate::error::TraceFrame>> {
    let head = MAX_TRACE * 3 / 4;
    let tail = MAX_TRACE - head;
    let truncated = trace.len() > MAX_TRACE;
    trace
        .iter()
        .enumerate()
        .filter_map(move |(i, f)| {
            if !truncated || i < head || i >= trace.len() - tail {
                Some(Some(f))
            } else if i == head {
                Some(None)
            } else {
                None
            }
        })
}

/// Display width of one character. Only the ranges that matter for this
/// project's scripts (CJK, fullwidth forms) are treated as double-width; the
/// rest is 1. Box drawing and combining marks are out of scope on purpose.
fn char_width(c: char) -> usize {
    let cp = c as u32;
    match cp {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}
