//! Hand-written recursive-descent parser.
//!
//! Grammar (see PROJECT_PLAN.md §3.8):
//!   program    := func_decl*
//!   func_decl  := "func" ident "(" params? ")" ("->" type)? block
//!   stmt       := let | expr-stmt | return | if | while | for
//!   expr       := precedence climbing over binary operators

use crate::ast::*;
use crate::error::{Error, Result};
use crate::span::Span;
use crate::token::{Token, TokenKind};
use crate::ty::Ty;

/// Maximum syntactic nesting depth: blocks, parentheses, unary operators and
/// binary-operator chains all count toward it.
///
/// Parsing is recursive descent and both the type checker and codegen recurse
/// over the same tree, so an unbounded depth turns a hostile (or merely
/// machine-generated) source into a stack overflow — which aborts the process
/// and cannot be caught by `catch_unwind`. Rejecting at the parser is the one
/// place that bounds every later stage, including the recursive `Drop` of the
/// AST itself.
///
/// 64 is far above hand-written code (nesting rarely exceeds 10) while staying
/// inside a 2 MiB stack even for unoptimized builds, where one level of
/// recursion costs roughly 17 KiB across `parse_expr_bp` / `parse_prefix` /
/// `parse_postfix` / `parse_primary`. Optimized builds need about an eighth of
/// that, so the same limit is safe on a 1 MiB Android thread stack.
pub const MAX_NESTING: usize = 64;

/// Built only when the limit is hit: keeping this out of `enter` keeps the
/// success path small enough to inline into the recursive descent helpers.
#[cold]
#[inline(never)]
fn depth_error(span: Span) -> Error {
    Error::parse(
        format!(
            "nesting too deep (limit {}): split this expression or block into smaller pieces",
            MAX_NESTING
        ),
        span,
    )
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Current nesting depth, counted along the path being parsed.
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, depth: 0 }
    }

    pub fn parse(mut self) -> Result<Program> {
        let mut imports = Vec::new();
        let mut funcs = Vec::new();
        self.skip_newlines();
        // 头部导入区：`import "名字"` 集中在所有函数定义之前（依赖声明，
        // 由引擎在编译期载入合并——见 HOST_GUIDE §12.1）。函数定义之后
        // 再出现 import 一律报错，保证依赖声明集中在文件头。
        while matches!(self.peek(), TokenKind::Import) {
            imports.push(self.parse_import()?);
            self.skip_newlines();
        }
        while !self.at_eof() {
            if matches!(self.peek(), TokenKind::Import) {
                return Err(Error::parse(
                    "import 必须写在所有函数定义之前",
                    self.peek_span(),
                ));
            }
            funcs.push(self.parse_func()?);
            self.skip_newlines();
        }
        Ok(Program { imports, funcs })
    }

    /// 解析 `import "名字"`。其后必须是换行或文件结束（本语言无分号，
    /// 声明行到此为止）。
    fn parse_import(&mut self) -> Result<ImportDecl> {
        let kw = self.expect(TokenKind::Import)?;
        let name = match self.peek().clone() {
            TokenKind::Str(s) if !s.is_empty() => {
                self.bump();
                s
            }
            TokenKind::Str(_) => {
                return Err(Error::parse("import 路径不能为空", self.peek_span()))
            }
            _ => {
                return Err(Error::parse(
                    "expected import 路径字符串，如 \"common\"",
                    self.peek_span(),
                ))
            }
        };
        match self.peek() {
            TokenKind::Newline | TokenKind::Eof => {}
            _ => {
                return Err(Error::parse(
                    "expected newline after import 声明（import 之后应换行）",
                    self.peek_span(),
                ))
            }
        }
        Ok(ImportDecl {
            name,
            span: kw.span.to(self.prev_span()),
        })
    }

    // ---- primitives ----

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    fn check(&self, k: &TokenKind) -> bool {
        self.peek() == k
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if !self.at_eof() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, k: TokenKind) -> Result<Token> {
        if self.check(&k) {
            Ok(self.bump())
        } else {
            Err(Error::parse(
                format!("expected {:?}, found {:?}", k, self.peek()),
                self.peek_span(),
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span)> {
        match self.peek().clone() {
            TokenKind::Ident(s) => {
                let span = self.peek_span();
                self.bump();
                Ok((s, span))
            }
            _ => Err(Error::parse(format!("expected identifier, found {:?}", self.peek()), self.peek_span())),
        }
    }

    /// Enter one level of nesting (see [`MAX_NESTING`]). Callers restore
    /// `self.depth` when they return; on the error path the whole parser is
    /// discarded, so a leaked increment is harmless.
    #[inline]
    fn enter(&mut self, span: Span) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(depth_error(span));
        }
        Ok(())
    }

    fn skip_newlines(&mut self) {
        while self.check(&TokenKind::Newline) {
            self.bump();
        }
    }

    fn eat_newlines(&mut self) {
        self.skip_newlines();
    }

    // ---- top level ----

    fn parse_func(&mut self) -> Result<FuncDecl> {
        let start = self.expect(TokenKind::Func)?.span;
        let (name, _) = self.expect_ident()?;
        self.expect(TokenKind::LParen)?;
        self.eat_newlines();

        let mut params = Vec::new();
        if !self.check(&TokenKind::RParen) {
            loop {
                self.eat_newlines();
                let (pname, pspan) = self.expect_ident()?;
                let ty = if self.check(&TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(Param { name: pname, ty, span: pspan });
                self.eat_newlines();
                if self.check(&TokenKind::Comma) {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.eat_newlines();
        self.expect(TokenKind::RParen)?;

        let ret = if self.check(&TokenKind::Arrow) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };

        self.eat_newlines();
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(FuncDecl { name, params, ret, body, span })
    }

    /// 类型文法的深度守卫（D-046 3.3）。`parse_type` 递归产生嵌套
    /// `Ty::Tuple` / `Ty::Arr`，而这些类型进入 AST 后被 `Drop`、
    /// `ty::render`、typecheck 的 `resolve`/`occurs`、cache 的
    /// `write_ty`/`read_ty` 全部递归处理——不设上限就把 `MAX_NESTING`
    /// 注释里"bounds every later stage"的论断砸出一个洞：约 2 万层
    /// `(i64)` 嵌套（40 KB 源码）即可在解析期栈溢出 abort，
    /// `catch_unwind` 抓不住。与表达式侧同一个上限、同一个报错。
    fn parse_type(&mut self) -> Result<Ty> {
        let span = self.peek_span();
        self.enter(span)?;
        let result = self.parse_type_inner();
        self.depth -= 1;
        result
    }

    fn parse_type_inner(&mut self) -> Result<Ty> {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                self.bump();
                Ok(match name.as_str() {
                    "i64" => Ty::I64,
                    "f64" => Ty::F64,
                    "bool" => Ty::Bool,
                    "str" => Ty::Str,
                    _ => Ty::Handle(name),
                })
            }
            TokenKind::LParen => {
                self.bump();
                self.eat_newlines();
                // `()` is the unit type (Q15), matching the `()` literal —
                // an empty tuple type and Unit render alike but do not unify.
                if self.check(&TokenKind::RParen) {
                    self.bump();
                    return Ok(Ty::Unit);
                }
                let mut tys = Vec::new();
                while !self.check(&TokenKind::RParen) {
                    tys.push(self.parse_type()?);
                    self.eat_newlines();
                    if self.check(&TokenKind::Comma) {
                        self.bump();
                        self.eat_newlines();
                    } else {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
                Ok(Ty::Tuple(tys))
            }
            TokenKind::LBracket => {
                // `[T]` array annotation (ADR-018): `let fails: [str] = []`,
                // params `rows: [(str, str, bool, i64)]`, return types.
                self.bump();
                self.eat_newlines();
                let elem = self.parse_type()?;
                self.eat_newlines();
                self.expect(TokenKind::RBracket)?;
                Ok(Ty::Arr(Box::new(elem)))
            }
            _ => Err(Error::parse(format!("expected type, found {:?}", self.peek()), self.peek_span())),
        }
    }

    // ---- blocks and statements ----

    fn parse_block(&mut self) -> Result<Block> {
        let open = self.expect(TokenKind::LBrace)?.span;
        self.enter(open)?;
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.check(&TokenKind::RBrace) {
            if self.at_eof() {
                return Err(Error::parse("unterminated block", open));
            }
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        let close = self.expect(TokenKind::RBrace)?.span;
        self.depth -= 1;
        Ok(Block { stmts, span: open.to(close) })
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        match self.peek().clone() {
            TokenKind::Let => {
                self.bump();
                let (name, _) = self.expect_ident()?;
                let ty = if self.check(&TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(TokenKind::Assign)?;
                let init = self.parse_expr()?;
                Ok(Stmt::Let { name, ty, init, span: span.to(self.prev_span()) })
            }
            TokenKind::Return => {
                self.bump();
                let value = if self.at_stmt_end() {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                Ok(Stmt::Return { value, span: span.to(self.prev_span()) })
            }
            TokenKind::Break => {
                self.bump();
                Ok(Stmt::Break { span: span.to(self.prev_span()) })
            }
            TokenKind::Continue => {
                self.bump();
                Ok(Stmt::Continue { span: span.to(self.prev_span()) })
            }
            TokenKind::If => self.parse_if_stmt(),
            TokenKind::While => {
                self.bump();
                let cond = self.parse_expr()?;
                self.eat_newlines();
                let body = self.parse_block()?;
                let end = body.span;
                Ok(Stmt::While { cond, body, span: span.to(end) })
            }
            TokenKind::For => {
                self.bump();
                let (var, _) = self.expect_ident()?;
                self.expect(TokenKind::In)?;
                // `for i in 0..n` (range) vs `for r in rows` (array,
                // ADR-018): the first expression is parsed normally — a
                // range's start stops at the DotDot token, which no binary
                // operator claims — and the presence of `..` picks the form.
                let first = self.parse_expr_bp(0)?;
                let iter = if self.check(&TokenKind::DotDot) {
                    self.bump();
                    let end = self.parse_expr()?;
                    ForIter::Range { start: first, end }
                } else {
                    ForIter::List(first)
                };
                self.eat_newlines();
                let body = self.parse_block()?;
                let bend = body.span;
                Ok(Stmt::For { var, iter, body, span: span.to(bend) })
            }
            TokenKind::Ident(_) if self.is_assignment_ahead() => {
                let (name, _) = self.expect_ident()?;
                self.expect(TokenKind::Assign)?;
                let value = self.parse_expr()?;
                Ok(Stmt::Assign { name, value, span: span.to(self.prev_span()) })
            }
            _ => {
                let e = self.parse_expr()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    /// Look ahead: `ident =` means assignment, otherwise a call/var expression.
    fn is_assignment_ahead(&self) -> bool {
        self.tokens.get(self.pos + 1).is_some_and(|t| t.kind == TokenKind::Assign)
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt> {
        let start = self.expect(TokenKind::If)?.span;
        self.enter(start)?;
        let cond = self.parse_expr()?;
        self.eat_newlines();
        let then = self.parse_block()?;
        // Allow `} else {` on the same or following line.
        self.skip_newlines();
        let otherwise = if self.check(&TokenKind::Else) {
            self.bump();
            self.eat_newlines();
            if self.check(&TokenKind::If) {
                // `else if` recurses without a block, so it needs its own
                // entry: a long else-if ladder is as deep as a nested `if`.
                Some(Box::new(ElseBranch::ElseIf(self.parse_if_stmt()?)))
            } else {
                Some(Box::new(ElseBranch::Block(self.parse_block()?)))
            }
        } else {
            None
        };
        let end = match otherwise.as_deref() {
            Some(ElseBranch::Block(b)) => b.span,
            Some(ElseBranch::ElseIf(s)) => stmt_span(s),
            None => then.span,
        };
        self.depth -= 1;
        Ok(Stmt::If { cond, then, otherwise, span: start.to(end) })
    }

    fn at_stmt_end(&self) -> bool {
        matches!(self.peek(), TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof)
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.peek_span()
        } else {
            self.tokens[self.pos - 1].span
        }
    }

    // ---- expressions: precedence climbing ----

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        let entry = self.depth;
        let mut lhs = self.parse_prefix()?;
        loop {
            let (op, l_bp, r_bp) = match self.binop_info() {
                Some(info) => info,
                None => break,
            };
            if l_bp < min_bp {
                break;
            }
            let op_span = self.peek_span();
            self.bump();
            // Allow line breaks after a binary operator.
            self.eat_newlines();
            // Every operator makes the left-nested result one level deeper,
            // even though this loop is iterative. Counting it here is what
            // stops `1+1+1+...` from building a tree the checker cannot walk.
            self.enter(op_span)?;
            let rhs = self.parse_expr_bp(r_bp)?;
            let span = expr_span(&lhs).to(expr_span(&rhs));
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span: op_span.to(span) };
        }
        // Siblings start from the caller's depth, not from this chain's depth.
        self.depth = entry;
        Ok(lhs)
    }

    fn binop_info(&self) -> Option<(BinOp, u8, u8)> {
        let (op, bp) = match self.peek() {
            TokenKind::OrOr => (BinOp::Or, 1),
            TokenKind::AndAnd => (BinOp::And, 2),
            TokenKind::Eq => (BinOp::Eq, 3),
            TokenKind::NotEq => (BinOp::NotEq, 3),
            TokenKind::Lt => (BinOp::Lt, 4),
            TokenKind::LtEq => (BinOp::LtEq, 4),
            TokenKind::Gt => (BinOp::Gt, 4),
            TokenKind::GtEq => (BinOp::GtEq, 4),
            TokenKind::Plus => (BinOp::Add, 5),
            TokenKind::Minus => (BinOp::Sub, 5),
            TokenKind::Star => (BinOp::Mul, 6),
            TokenKind::Slash => (BinOp::Div, 6),
            TokenKind::Percent => (BinOp::Mod, 6),
            _ => return None,
        };
        // All operators are left-associative: left bp = n, right bp = n+1.
        Some((op, bp, bp + 1))
    }

    fn parse_prefix(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.enter(span)?;
        let e = match self.peek().clone() {
            TokenKind::Minus => {
                self.bump();
                let e = self.parse_prefix()?;
                Ok(Expr::Unary { op: UnOp::Neg, expr: Box::new(e), span: span.to(self.prev_span()) })
            }
            TokenKind::Bang => {
                self.bump();
                let e = self.parse_prefix()?;
                Ok(Expr::Unary { op: UnOp::Not, expr: Box::new(e), span: span.to(self.prev_span()) })
            }
            _ => self.parse_postfix(),
        };
        self.depth -= 1;
        e
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.check(&TokenKind::Dot) {
                // Ambiguity with `..`: lexer produces DotDot for range, so a
                // lone Dot must be tuple index access.
                self.bump();
                let idx_span = self.peek_span();
                let idx = match self.peek().clone() {
                    TokenKind::Int(n) if n >= 0 => n as usize,
                    _ => return Err(Error::parse("expected tuple index after '.'", idx_span)),
                };
                self.bump();
                let span = expr_span(&e).to(idx_span);
                e = Expr::Index { base: Box::new(e), index: idx, span };
            } else if self.check(&TokenKind::LBracket) {
                // Array indexing `rows[i]` (ADR-018). Chained like `.n`
                // postfix: `rows[i][j]` indexes the (nested) result again.
                self.bump();
                self.eat_newlines();
                let idx = self.parse_expr()?;
                self.eat_newlines();
                let close = self.expect(TokenKind::RBracket)?.span;
                let span = expr_span(&e).to(close);
                e = Expr::DynIndex { base: Box::new(e), index: Box::new(idx), span };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        match self.peek().clone() {
            TokenKind::Int(n) => { self.bump(); Ok(Expr::Int(n, span)) }
            TokenKind::Float(f) => { self.bump(); Ok(Expr::Float(f, span)) }
            TokenKind::Str(s) => { self.bump(); Ok(Expr::Str(s, span)) }
            TokenKind::True => { self.bump(); Ok(Expr::Bool(true, span)) }
            TokenKind::False => { self.bump(); Ok(Expr::Bool(false, span)) }
            TokenKind::Ident(name) => {
                self.bump();
                if self.check(&TokenKind::LParen) {
                    self.bump();
                    self.eat_newlines();
                    let mut args = Vec::new();
                    while !self.check(&TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        self.eat_newlines();
                        if self.check(&TokenKind::Comma) {
                            self.bump();
                            self.eat_newlines();
                        } else {
                            break;
                        }
                    }
                    let close = self.expect(TokenKind::RParen)?.span;
                    Ok(Expr::Call { name, args, span: span.to(close) })
                } else {
                    Ok(Expr::Var(name, span))
                }
            }
            TokenKind::LParen => {
                self.bump();
                self.eat_newlines();
                if self.check(&TokenKind::RParen) {
                    let close = self.bump().span;
                    return Ok(Expr::Tuple(Vec::new(), span.to(close)));
                }
                let first = self.parse_expr()?;
                self.eat_newlines();
                if self.check(&TokenKind::Comma) {
                    // tuple
                    let mut items = vec![first];
                    while self.check(&TokenKind::Comma) {
                        self.bump();
                        self.eat_newlines();
                        if self.check(&TokenKind::RParen) {
                            break;
                        }
                        items.push(self.parse_expr()?);
                        self.eat_newlines();
                    }
                    let close = self.expect(TokenKind::RParen)?.span;
                    Ok(Expr::Tuple(items, span.to(close)))
                } else {
                    // grouping
                    let close = self.expect(TokenKind::RParen)?.span;
                    let _ = close;
                    // Re-span the inner expression to include parens roughly.
                    Ok(first)
                }
            }
            TokenKind::If => {
                // if-expression: requires else branches to be values.
                //
                // The `else if` form recurses through `parse_if_expr` with no
                // block in between, so an expression-position ladder builds up
                // parser — and later typecheck/codegen/Drop — recursion with no
                // other depth accounting. Give every if-expression its own
                // depth entry, exactly like `parse_if_stmt` does for the
                // statement form; on error paths the parser is discarded, so a
                // leaked increment is harmless.
                self.enter(span)?;
                self.bump();
                let cond = self.parse_expr()?;
                self.eat_newlines();
                let then = self.parse_block()?;
                self.skip_newlines();
                self.expect(TokenKind::Else)?;
                self.eat_newlines();
                let otherwise = if self.check(&TokenKind::If) {
                    // else-if as expression: wrap a single if-expression block.
                    let nested = self.parse_if_expr()?;
                    let bspan = expr_span(&nested);
                    Block { stmts: vec![Stmt::Expr(nested)], span: bspan }
                } else {
                    self.parse_block()?
                };
                let end = otherwise.span;
                self.depth -= 1;
                Ok(Expr::IfExpr {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    otherwise: Box::new(otherwise),
                    span: span.to(end),
                })
            }
            TokenKind::LBrace => {
                let b = self.parse_block()?;
                let s = b.span;
                Ok(Expr::BlockExpr(b, s))
            }
            TokenKind::LBracket => {
                // Array literal (ADR-018). Same comma/newline handling as the
                // tuple literal; empty `[]` infers its element type later.
                self.bump();
                self.eat_newlines();
                if self.check(&TokenKind::RBracket) {
                    let close = self.bump().span;
                    return Ok(Expr::Array(Vec::new(), span.to(close)));
                }
                let mut items = Vec::new();
                loop {
                    items.push(self.parse_expr()?);
                    self.eat_newlines();
                    if self.check(&TokenKind::Comma) {
                        self.bump();
                        self.eat_newlines();
                        if self.check(&TokenKind::RBracket) {
                            break; // trailing comma
                        }
                        continue;
                    }
                    break;
                }
                let close = self.expect(TokenKind::RBracket)?.span;
                Ok(Expr::Array(items, span.to(close)))
            }
            _ => Err(Error::parse(format!("unexpected token {:?} in expression", self.peek()), span)),
        }
    }

    fn parse_if_expr(&mut self) -> Result<Expr> {
        // Entry point with the leading `if` already unconsumed.
        self.parse_primary()
    }

}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int(_, s)
        | Expr::Float(_, s)
        | Expr::Bool(_, s)
        | Expr::Str(_, s)
        | Expr::Var(_, s)
        | Expr::Call { span: s, .. }
        | Expr::Unary { span: s, .. }
        | Expr::Binary { span: s, .. }
        | Expr::IfExpr { span: s, .. }
        | Expr::Tuple(_, s)
        | Expr::Array(_, s)
        | Expr::Index { span: s, .. }
        | Expr::DynIndex { span: s, .. }
        | Expr::BlockExpr(_, s) => *s,
    }
}

fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span } => *span,
        Stmt::Expr(e) => expr_span(e),
    }
}
