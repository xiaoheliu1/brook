//! Abstract syntax tree.

use crate::span::Span;

/// 顶层 `import "名字"` 声明（约定式导入，见 HOST_GUIDE §12.1）。
/// 仅编译期指令：解析阶段被引擎消化（载入库文本合并进本单元），
/// 不进入可执行流。
#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub imports: Vec<ImportDecl>,
    pub funcs: Vec<FuncDecl>,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub name: String,
    pub params: Vec<Param>,
    /// Optional return type annotation. `None` means infer.
    pub ret: Option<crate::ty::Ty>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// Parameters are inferred; annotation is the escape hatch.
    pub ty: Option<crate::ty::Ty>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<crate::ty::Ty>,
        init: Expr,
        span: Span,
    },
    /// Reassignment to an existing variable: `x = expr`
    Assign { name: String, value: Expr, span: Span },
    Expr(Expr),
    Return { value: Option<Expr>, span: Span },
    If {
        cond: Expr,
        then: Block,
        otherwise: Option<Box<ElseBranch>>,
        span: Span,
    },
    While { cond: Expr, body: Block, span: Span },
    For {
        var: String,
        iter: ForIter,
        body: Block,
        span: Span,
    },
    /// Leave the innermost enclosing loop.
    Break { span: Span },
    /// Jump to the innermost enclosing loop's next iteration.
    Continue { span: Span },
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    Block(Block),
    ElseIf(Stmt),
}

/// What a `for` loop walks (ADR-018): a half-open integer range, or an
/// array. The parser decides from the presence of `..` after the first
/// expression; the type checker pins the variant to a type.
#[derive(Debug, Clone)]
pub enum ForIter {
    Range { start: Expr, end: Expr },
    List(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64, Span),
    Float(f64, Span),
    Bool(bool, Span),
    Str(String, Span),
    Var(String, Span),
    Call { name: String, args: Vec<Expr>, span: Span },
    Unary { op: UnOp, expr: Box<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    /// `if cond {..} else {..}` used as a value.
    IfExpr {
        cond: Box<Expr>,
        then: Box<Block>,
        otherwise: Box<Block>,
        span: Span,
    },
    /// A block used as an expression; its value is the last statement's value.
    BlockExpr(Block, Span),
    Tuple(Vec<Expr>, Span),
    /// Array literal `[a, b, c]` (ADR-018). Empty `[]` infers its element
    /// type from usage (first `push`, annotation, or context).
    Array(Vec<Expr>, Span),
    /// Tuple element access: `expr . 0`
    Index { base: Box<Expr>, index: usize, span: Span },
    /// Array indexing: `expr[i]` with a runtime index. Tuples are indexed
    /// with constant `.n` (see `Index`); this is the array counterpart.
    DynIndex { base: Box<Expr>, index: Box<Expr>, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}
