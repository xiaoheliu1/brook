//! Source locations shared by all compiler stages.

/// A half-open source range with 1-based line/column at its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Self { start, end, line, col }
    }

    /// Combine a start span with an end span into one covering both.
    pub fn to(self, other: Span) -> Self {
        Span::new(self.start, other.end, self.line, self.col)
    }

    /// A synthetic span with no source location (used for generated code).
    pub fn synthetic() -> Self {
        Self::default()
    }
}
