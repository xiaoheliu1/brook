//! Token definitions.

use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    // Keywords
    Func,
    Let,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    True,
    False,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,       // ==
    NotEq,    // !=
    Lt,       // <
    LtEq,     // <=
    Gt,       // >
    GtEq,     // >=
    AndAnd,   // &&
    OrOr,     // ||
    Bang,     // !
    Assign,   // =

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,      // . and tuple access .0
    DotDot,   // ..
    Colon,    // : for optional type annotation
    Arrow,    // -> for return type

    Newline,  // statement terminator (no semicolons in this language)
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
