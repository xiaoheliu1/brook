//! Hand-written lexer. Emits Newline tokens; the parser treats them as
//! statement separators (this language has no semicolons).

use crate::error::{Error, Result};
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    /// 上一个发射的 token 是否为 `.`。Dot 之后的数字只按整数读（不吞
    /// `.小数`/指数）：否则 `t.1.0` 里的 `1.0` 会被整体读成浮点字面量，
    /// 嵌套元组的索引链（`t.0.1`）无法表达。普通位置的 `1.5`、`2e3`
    /// 不受影响。
    prev_dot: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self::new_with_offset(src, 1)
    }

    /// 合并编译用：`first_line` 是本段源码在拼接视图中的起始行号（1-based）。
    /// 约定式 import 会把多份源码按依赖顺序拼成一个编译单元，
    /// 每段的行号必须接续前一段，报错行号才能与拼接视图对齐。
    pub fn new_with_offset(src: &'a str, first_line: usize) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
            line: first_line,
            col: 1,
            prev_dot: false,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>> {
        let mut out = Vec::new();
        loop {
            let tok = self.next_token()?;
            self.prev_dot = tok.kind == TokenKind::Dot;
            let done = tok.kind == TokenKind::Eof;
            out.push(tok);
            if done {
                break;
            }
        }
        Ok(out)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if c & 0xC0 != 0x80 {
            // `col` is a *character* column, not a byte offset: a UTF-8
            // continuation byte is part of the glyph counted by its lead byte.
            // Counting bytes made every diagnostic on a line containing CJK
            // text report a column the reader could not find.
            self.col += 1;
        }
        Some(c)
    }

    fn span_from(&self, start: usize, line: usize, col: usize) -> Span {
        Span::new(start, self.pos, line, col)
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t') | Some(b'\r')) {
            self.bump();
        }
    }

    fn next_token(&mut self) -> Result<Token> {
        // Skip spaces/tabs and comments; keep newlines as tokens.
        loop {
            self.skip_inline_ws();
            if self.peek() == Some(b'/') && self.peek2() == Some(b'/') {
                // Line comment: stop *before* the newline so it still acts as
                // a statement separator.
                while let Some(c) = self.peek() {
                    if c == b'\n' {
                        break;
                    }
                    self.bump();
                }
                continue;
            }
            if self.peek() == Some(b'/') && self.peek2() == Some(b'*') {
                // Block comment. Not nestable (C style): the first `*/`
                // closes it, so `/* a /* b */` ends at the first `*/`.
                let start = self.pos;
                let line = self.line;
                let col = self.col;
                self.bump();
                self.bump();
                let mut spans_lines = false;
                loop {
                    match self.peek() {
                        None => {
                            return Err(Error::lex(
                                "unterminated block comment",
                                self.span_from(start, line, col),
                            ));
                        }
                        Some(b'*') if self.peek2() == Some(b'/') => {
                            self.bump();
                            self.bump();
                            break;
                        }
                        Some(b'\n') => {
                            spans_lines = true;
                            self.bump();
                        }
                        Some(_) => {
                            self.bump();
                        }
                    }
                }
                if spans_lines {
                    // Newlines inside the comment were consumed, but this
                    // language separates statements by newline. Emit one so
                    // `let a = 1 /* \n */ let b = 2` still parses as two
                    // statements rather than one malformed line.
                    return Ok(Token::new(
                        TokenKind::Newline,
                        self.span_from(start, line, col),
                    ));
                }
                continue;
            }
            break;
        }

        let start = self.pos;
        let line = self.line;
        let col = self.col;
        let c = match self.peek() {
            Some(c) => c,
            None => return Ok(Token::new(TokenKind::Eof, self.span_from(start, line, col))),
        };

        // Newline
        if c == b'\n' {
            self.bump();
            return Ok(Token::new(
                TokenKind::Newline,
                self.span_from(start, line, col),
            ));
        }

        // Identifier or keyword
        if is_ident_start(c) {
            while let Some(ch) = self.peek() {
                if is_ident_continue(ch) {
                    self.bump();
                } else {
                    break;
                }
            }
            let text = std::str::from_utf8(&self.src[start..self.pos])
                .unwrap()
                .to_string();
            let kind = match text.as_str() {
                "func" => TokenKind::Func,
                "import" => TokenKind::Import,
                "let" => TokenKind::Let,
                "if" => TokenKind::If,
                "else" => TokenKind::Else,
                "while" => TokenKind::While,
                "for" => TokenKind::For,
                "in" => TokenKind::In,
                "return" => TokenKind::Return,
                "break" => TokenKind::Break,
                "continue" => TokenKind::Continue,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Ident(text),
            };
            return Ok(Token::new(kind, self.span_from(start, line, col)));
        }

        // Number: integer, float with '.', or exponent.
        if c.is_ascii_digit() {
            while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                self.bump();
            }
            let mut is_float = false;
            // Dot 后的数字是索引链的一段（`t.1.0`），不吞 `.小数`。
            if !self.prev_dot
                && self.peek() == Some(b'.')
                && self.peek2().is_some_and(|d| d.is_ascii_digit())
            {
                is_float = true;
                self.bump(); // '.'
                while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    self.bump();
                }
            }
            if !self.prev_dot && matches!(self.peek(), Some(b'e') | Some(b'E')) {
                // Only consume exponent if followed by optional sign and a digit.
                let save = self.pos;
                let save_line = self.line;
                let save_col = self.col;
                self.bump(); // e/E
                if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                    self.bump();
                }
                if matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    is_float = true;
                    while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                        self.bump();
                    }
                } else {
                    self.pos = save;
                    self.line = save_line;
                    self.col = save_col;
                }
            }
            let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
            let kind = if is_float {
                TokenKind::Float(text.parse().map_err(|_| {
                    Error::lex("invalid float literal", self.span_from(start, line, col))
                })?)
            } else {
                TokenKind::Int(text.parse().map_err(|_| {
                    Error::lex(
                        "integer literal overflows i64",
                        self.span_from(start, line, col),
                    )
                })?)
            };
            return Ok(Token::new(kind, self.span_from(start, line, col)));
        }

        // String literal with common escapes.
        if c == b'"' {
            self.bump(); // opening quote
            let mut bytes = Vec::new();
            loop {
                match self.bump() {
                    None => {
                        return Err(Error::lex(
                            "unterminated string literal",
                            self.span_from(start, line, col),
                        ));
                    }
                    Some(b'"') => break,
                    Some(b'\\') => {
                        let esc = self.bump().ok_or_else(|| {
                            Error::lex("unterminated escape", self.span_from(start, line, col))
                        })?;
                        match esc {
                            b'n' => bytes.push(b'\n'),
                            b't' => bytes.push(b'\t'),
                            b'r' => bytes.push(b'\r'),
                            b'\\' => bytes.push(b'\\'),
                            b'"' => bytes.push(b'"'),
                            other => {
                                return Err(Error::lex(
                                    format!("unknown escape '\\{}'", other as char),
                                    self.span_from(start, line, col),
                                ));
                            }
                        }
                    }
                    Some(ch) => bytes.push(ch),
                }
            }
            let s = String::from_utf8(bytes).map_err(|_| {
                Error::lex(
                    "invalid UTF-8 in string literal",
                    self.span_from(start, line, col),
                )
            })?;
            return Ok(Token::new(
                TokenKind::Str(s),
                self.span_from(start, line, col),
            ));
        }

        // Operators and delimiters (longest match first).
        let two = [self.peek(), self.peek2()];
        let kind = match two {
            [Some(b'='), Some(b'=')] => {
                self.bump();
                TokenKind::Eq
            }
            [Some(b'!'), Some(b'=')] => {
                self.bump();
                TokenKind::NotEq
            }
            [Some(b'<'), Some(b'=')] => {
                self.bump();
                TokenKind::LtEq
            }
            [Some(b'>'), Some(b'=')] => {
                self.bump();
                TokenKind::GtEq
            }
            [Some(b'&'), Some(b'&')] => {
                self.bump();
                TokenKind::AndAnd
            }
            [Some(b'|'), Some(b'|')] => {
                self.bump();
                TokenKind::OrOr
            }
            [Some(b'.'), Some(b'.')] => {
                self.bump();
                TokenKind::DotDot
            }
            [Some(b'-'), Some(b'>')] => {
                self.bump();
                TokenKind::Arrow
            }
            _ => match c {
                b'+' => TokenKind::Plus,
                b'-' => TokenKind::Minus,
                b'*' => TokenKind::Star,
                b'/' => TokenKind::Slash,
                b'%' => TokenKind::Percent,
                b'<' => TokenKind::Lt,
                b'>' => TokenKind::Gt,
                b'!' => TokenKind::Bang,
                b'=' => TokenKind::Assign,
                b'(' => TokenKind::LParen,
                b')' => TokenKind::RParen,
                b'{' => TokenKind::LBrace,
                b'}' => TokenKind::RBrace,
                b',' => TokenKind::Comma,
                b'.' => TokenKind::Dot,
                b'[' => TokenKind::LBracket,
                b']' => TokenKind::RBracket,
                b':' => TokenKind::Colon,
                _ => {
                    // Decode the full UTF-8 scalar so the message shows the
                    // real glyph (e.g. '中') instead of a Latin-1 misreading
                    // of its lead byte, and advance past all of its bytes.
                    let len = utf8_seq_len(c);
                    let end = (start + len).min(self.src.len());
                    let shown = std::str::from_utf8(&self.src[start..end])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map(|ch| ch.to_string())
                        .unwrap_or_else(|| format!("\\x{:02x}", c));
                    for _ in 0..len {
                        self.bump();
                    }
                    return Err(Error::lex(
                        format!("unexpected character '{}'", shown),
                        self.span_from(start, line, col),
                    ));
                }
            },
        };
        self.bump();
        Ok(Token::new(kind, self.span_from(start, line, col)))
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Byte length of the UTF-8 sequence whose lead byte is `c`. Invalid or
/// truncated leads still yield a length (the caller bounds it against the
/// source end), so the lexer always makes progress.
fn utf8_seq_len(c: u8) -> usize {
    match c {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        Lexer::new(src)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn lexes_simple_expression() {
        assert_eq!(
            kinds("let x = 42"),
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".into()),
                TokenKind::Assign,
                TokenKind::Int(42),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_two_lines() {
        let k = kinds("let a = 1\nlet b = 2.5");
        assert!(k.contains(&TokenKind::Newline));
        assert!(k.contains(&TokenKind::Float(2.5)));
    }

    #[test]
    fn lexes_operators_and_range() {
        let k = kinds("0..n == x != y <= z >= 1 && a || !b -> : .");
        assert!(k.contains(&TokenKind::DotDot));
        assert!(k.contains(&TokenKind::Eq));
        assert!(k.contains(&TokenKind::NotEq));
        assert!(k.contains(&TokenKind::LtEq));
        assert!(k.contains(&TokenKind::GtEq));
        assert!(k.contains(&TokenKind::AndAnd));
        assert!(k.contains(&TokenKind::OrOr));
        assert!(k.contains(&TokenKind::Bang));
        assert!(k.contains(&TokenKind::Arrow));
        assert!(k.contains(&TokenKind::Colon));
        assert!(k.contains(&TokenKind::Dot));
    }

    #[test]
    fn lexes_string_with_escapes() {
        let k = kinds("\"hi\\nthere\"");
        assert_eq!(k, vec![TokenKind::Str("hi\nthere".into()), TokenKind::Eof]);
    }

    #[test]
    fn lexes_utf8_string_literal() {
        let k = kinds("\"中文\\nend\"");
        assert_eq!(k, vec![TokenKind::Str("中文\nend".into()), TokenKind::Eof]);
    }

    #[test]
    fn skips_comments() {
        let k = kinds("// comment\n42");
        assert_eq!(
            k,
            vec![TokenKind::Newline, TokenKind::Int(42), TokenKind::Eof]
        );
    }

    #[test]
    fn skips_trailing_line_comment() {
        // The newline must survive, otherwise `let a = 1 // x` and the next
        // line would be glued into one statement.
        let k = kinds("let a = 1 // note\nlet b = 2");
        assert_eq!(
            k,
            vec![
                TokenKind::Let,
                TokenKind::Ident("a".into()),
                TokenKind::Assign,
                TokenKind::Int(1),
                TokenKind::Newline,
                TokenKind::Let,
                TokenKind::Ident("b".into()),
                TokenKind::Assign,
                TokenKind::Int(2),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn double_slash_inside_string_is_not_a_comment() {
        let k = kinds("\"http://example.com\"");
        assert_eq!(
            k,
            vec![TokenKind::Str("http://example.com".into()), TokenKind::Eof]
        );
    }

    #[test]
    fn single_line_block_comment_acts_as_whitespace() {
        let k = kinds("1 /* note */ + 2");
        assert_eq!(
            k,
            vec![
                TokenKind::Int(1),
                TokenKind::Plus,
                TokenKind::Int(2),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn block_comment_spanning_lines_emits_one_newline() {
        // A comment that swallows a newline would otherwise merge the two
        // statements around it.
        let k = kinds("let a = 1 /* one\ntwo */ let b = 2");
        assert_eq!(
            k,
            vec![
                TokenKind::Let,
                TokenKind::Ident("a".into()),
                TokenKind::Assign,
                TokenKind::Int(1),
                TokenKind::Newline,
                TokenKind::Let,
                TokenKind::Ident("b".into()),
                TokenKind::Assign,
                TokenKind::Int(2),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn block_comment_is_not_nestable() {
        // C semantics: the first `*/` closes the comment, so `b` is code.
        let k = kinds("/* a /* b */ c");
        assert_eq!(
            k,
            vec![TokenKind::Ident("c".into()), TokenKind::Eof],
            "at least the text after the first `*/` must lex as code"
        );
    }

    #[test]
    fn unterminated_block_comment_is_a_lex_error() {
        let err = Lexer::new("1 /* never closed").tokenize().unwrap_err();
        assert!(
            err.message.contains("unterminated block comment"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn block_comment_preserves_unicode() {
        let k = kinds("/* 中文注释 { } \" ' */ 7");
        assert_eq!(k, vec![TokenKind::Int(7), TokenKind::Eof]);
    }

    #[test]
    fn tracks_line_and_col() {
        let toks = Lexer::new("a\n  b").tokenize().unwrap();
        let b = toks
            .iter()
            .find(|t| matches!(t.kind, TokenKind::Ident(ref s) if s == "b"))
            .unwrap();
        assert_eq!(b.span.line, 2);
        assert_eq!(b.span.col, 3);
    }
}
