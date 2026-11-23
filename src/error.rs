//! Unified error type for every stage: lexing, parsing, type checking, runtime.

use crate::span::Span;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Type,
    Runtime,
    Io,
}

/// Message used by the host-cancellation error. Kept as a named constant so
/// hosts can recognise a stop without hard-coding a translated string: use
/// [`Error::is_cancelled`], which compares against this value.
pub const CANCELLED_MESSAGE: &str = "script stopped by host";

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub span: Option<Span>,
    /// Interpreted call stack, outermost frame first. Populated only for
    /// runtime errors thrown by the VM; empty for lex/parse/type/io errors.
    pub trace: Vec<TraceFrame>,
}

/// One level of an interpreted call stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFrame {
    /// Script function name.
    pub func: String,
    /// 1-based location inside `func`: the call site for the caller frames, the
    /// faulting instruction for the innermost one. `None` when the error carries
    /// no source location (e.g. a quota that tripped on a generated frame).
    pub line: usize,
    pub col: usize,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            kind,
            message: message.into(),
            span,
            trace: Vec::new(),
        }
    }

    pub fn lex(message: impl Into<String>, span: Span) -> Self {
        Self::new(ErrorKind::Lex, message, Some(span))
    }

    pub fn parse(message: impl Into<String>, span: Span) -> Self {
        Self::new(ErrorKind::Parse, message, Some(span))
    }

    pub fn type_error(message: impl Into<String>, span: Span) -> Self {
        Self::new(ErrorKind::Type, message, Some(span))
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message, None)
    }

    pub fn runtime_at(message: impl Into<String>, span: Span) -> Self {
        Self::new(ErrorKind::Runtime, message, Some(span))
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message, None)
    }

    /// A run aborted because the host's cancellation hook returned `true`.
    ///
    /// Deliberately stays a `Runtime` error: the C ABI maps that kind to
    /// `MS_ERR_RUNTIME` and `Display` keeps the `[runtime]` prefix, so no
    /// existing caller needs updating. Hosts distinguish a stop from other
    /// runtime failures with [`Error::is_cancelled`], never by string match.
    pub fn cancelled(span: Span) -> Self {
        Self::new(ErrorKind::Runtime, CANCELLED_MESSAGE, Some(span))
    }

    /// True when this error was produced by the cancellation hook.
    pub fn is_cancelled(&self) -> bool {
        self.kind == ErrorKind::Runtime && self.message == CANCELLED_MESSAGE
    }

    /// Attach a call stack (outermost frame first). Builder form so the VM can
    /// record the stack without mutating the error it was constructed from.
    pub fn with_trace(mut self, trace: Vec<TraceFrame>) -> Self {
        if self.trace.is_empty() {
            self.trace = trace;
        }
        self
    }

    /// The frame that actually failed, if a stack was captured.
    pub fn innermost(&self) -> Option<&TraceFrame> {
        self.trace.last()
    }

    /// Multi-line diagnostic: the message, the offending source line with a
    /// caret, and the call stack when one was captured.
    ///
    /// Kept separate from `Display`, which stays a single line so existing log
    /// lines and the C ABI error buffer keep their shape.
    pub fn report(&self, src: &str, name: &str) -> String {
        crate::diag::render(self, src, name)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.kind {
            ErrorKind::Lex => "lex",
            ErrorKind::Parse => "parse",
            ErrorKind::Type => "type",
            ErrorKind::Runtime => "runtime",
            ErrorKind::Io => "io",
        };
        write!(f, "[{}] error", stage)?;
        if let Some(s) = self.span {
            write!(f, " at {}:{}", s.line, s.col)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
