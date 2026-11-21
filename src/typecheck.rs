//! Static type inference with Hindley-Milner-style unification.
//!
//! All parameter and return types are inferred from literals, host function
//! signatures (the "seeds"), and operator propagation. Optional annotations
//! act as constraints. Unification conflicts are reported at the source span.

use crate::ast::*;
use crate::error::{Error, Result};
use crate::span::Span;
use crate::ty::Ty;
use std::collections::HashMap;

/// A function signature visible to the type checker.
#[derive(Debug, Clone)]
pub struct FuncSig {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Result of checking: resolved signatures for every script function.
pub type SigTable = HashMap<String, FuncSig>;

pub struct Checker {
    subst: Vec<Option<Ty>>,
    next_var: usize,
    /// Mutable table of all functions: host + script.
    sigs: SigTable,
    /// Host-supplied names, so pass 1 can distinguish "collides with a host
    /// function" from "the script defines the same name twice" (the latter is
    /// the common failure when a host concatenates a shared library with a
    /// business script — see docs/TUTORIAL.md §4.4).
    host_names: std::collections::HashSet<String>,
    scopes: Vec<HashMap<String, Ty>>,
    current_ret: Ty,
    /// Number of enclosing `while`/`for` bodies. `break` and `continue` are
    /// compile errors at depth 0 instead of silently looping or exiting.
    loop_depth: usize,
    /// 活跃的 for 循环变量：(名字, 绑定它的 scope 下标)。`for i in 0..n
    /// { i = 5 }` 会被放行到 codegen 直接写循环寄存器——自增照常进行，
    /// 每轮又被重置为同一个值 → 死循环（只靠 gas 兜底，D-046 5.9）。
    /// 记录 scope 下标是为了区分遮蔽：循环体内 `let i = ...` 造出新绑定，
    /// 对它赋值合法。
    active_for_vars: Vec<(String, usize)>,
}

impl Checker {
    pub fn new(host: &HashMap<String, FuncSig>) -> Self {
        Self {
            subst: Vec::new(),
            next_var: 0,
            sigs: host.clone(),
            host_names: host.keys().cloned().collect(),
            scopes: Vec::new(),
            current_ret: Ty::Unit,
            loop_depth: 0,
            active_for_vars: Vec::new(),
        }
    }

    pub fn check(mut self, program: &Program) -> Result<SigTable> {
        // Pass 1: build a signature skeleton for every script function.
        for f in &program.funcs {
            if self.host_names.contains(&f.name) {
                return Err(Error::type_error(
                    format!("function `{}` is already defined by the host", f.name),
                    f.span,
                ));
            }
            if self.sigs.contains_key(&f.name) {
                return Err(Error::type_error(
                    format!("function `{}` is already defined (duplicate definition)", f.name),
                    f.span,
                ));
            }
            // Same asymmetry-closing rule as the host check below (Q15):
            // `infer_call` resolves builtins first, so a script function named
            // like a builtin would be silently unreachable from inside the
            // script while still callable as an entry point. Reject it like
            // the host collision instead.
            if is_builtin(&f.name) {
                return Err(Error::type_error(
                    format!("function `{}` is a reserved built-in name", f.name),
                    f.span,
                ));
            }
            // 参数上限与 MAX_OPERANDS 同口径（D-046 5.13）：入口函数此前
            // 可以带 >8 个参数（Vm::call 直接走 Vec），但脚本内部调用它
            // 需要 CallScript 的内联寄存器列表（≤8）——定义能过、调用必挂，
            // 把不对称压到定义处一并报错。
            if f.params.len() > crate::ir::MAX_OPERANDS {
                return Err(Error::type_error(
                    format!(
                        "function `{}` has {} parameters; the maximum is {}",
                        f.name,
                        f.params.len(),
                        crate::ir::MAX_OPERANDS
                    ),
                    f.span,
                ));
            }
            let params = f
                .params
                .iter()
                .map(|p| p.ty.clone().unwrap_or_else(|| self.fresh()))
                .collect();
            let ret = f.ret.clone().unwrap_or_else(|| self.fresh());
            self.sigs.insert(f.name.clone(), FuncSig { params, ret });
        }

        // Pass 2: infer each body against its skeleton.
        for f in &program.funcs {
            self.check_func(f)?;
        }

        // Pass 3: resolve signatures. Variables with no constraint converge
        // to i64 (the language's default numeric type). This is what lets
        // host-called functions like `add(a,b){a+b}` work without annotations.
        let mut resolved = SigTable::new();
        for (name, sig) in &self.sigs {
            let params: Vec<Ty> = sig
                .params
                .iter()
                .map(|t| default_vars(self.resolve(t)))
                .collect();
            let ret = default_vars(self.resolve(&sig.ret));
            resolved.insert(name.clone(), FuncSig { params, ret });
        }
        Ok(resolved)
    }

    fn check_func(&mut self, f: &FuncDecl) -> Result<()> {
        let sig = self.sigs.get(&f.name).unwrap().clone();
        self.current_ret = sig.ret.clone();

        self.scopes.push(HashMap::new());
        for (p, ty) in f.params.iter().zip(sig.params.iter()) {
            self.bind(&p.name, ty.clone());
        }

        let body_ty = self.infer_block(&f.body)?;
        if let Some(t) = body_ty {
            let span = f.body.span;
            self.unify(&sig.ret, &t, span).map_err(|e| {
                Error::type_error(
                    format!("function `{}`: {}", f.name, e),
                    span,
                )
            })?;
        }
        self.scopes.pop();
        Ok(())
    }

    // ---- inference variable plumbing ----

    fn fresh(&mut self) -> Ty {
        let id = self.next_var;
        self.next_var += 1;
        self.subst.push(None);
        Ty::Var(id)
    }

    fn resolve(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(id) => match &self.subst[*id] {
                Some(t) => self.resolve(t),
                None => Ty::Var(*id),
            },
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Arr(t) => Ty::Arr(Box::new(self.resolve(t))),
            other => other.clone(),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty, span: Span) -> std::result::Result<(), String> {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (a, b) {
            (Ty::Var(i), Ty::Var(j)) if i == j => Ok(()),
            (Ty::Var(i), t) | (t, Ty::Var(i)) => {
                if occurs(i, &t) {
                    return Err(format!("infinite type: ?{} occurs in {}", i, t.render()));
                }
                self.subst[i] = Some(t);
                Ok(())
            }
            (Ty::I64, Ty::I64)
            | (Ty::F64, Ty::F64)
            | (Ty::Bool, Ty::Bool)
            | (Ty::Str, Ty::Str)
            | (Ty::Unit, Ty::Unit) => Ok(()),
            (Ty::Handle(x), Ty::Handle(y)) if x == y => Ok(()),
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return Err(format!("tuple arity mismatch: {} vs {}", xs.len(), ys.len()));
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    self.unify(x, y, span)?;
                }
                Ok(())
            }
            (Ty::Arr(x), Ty::Arr(y)) => self.unify(&x, &y, span),
            (x, y) => Err(format!("type mismatch: expected {}, found {}", x.render(), y.render())),
        }
    }

    // ---- scopes ----

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: &str, ty: Ty) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), ty);
        }
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    /// `name` 的绑定所在的 scope 下标（自内向外找）。
    fn find_scope(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rposition(|scope| scope.contains_key(name))
    }

    // ---- statements ----

    /// Returns `None` if the block diverges (ends in `return`).
    fn infer_block(&mut self, block: &Block) -> Result<Option<Ty>> {
        self.push_scope();
        let mut last: Option<Ty> = Some(Ty::Unit);
        for stmt in &block.stmts {
            match self.infer_stmt(stmt)? {
                Some(t) => last = Some(t),
                None => {
                    last = None;
                    break; // unreachable statements after divergence
                }
            }
        }
        self.pop_scope();
        Ok(last)
    }

    fn infer_stmt(&mut self, stmt: &Stmt) -> Result<Option<Ty>> {
        match stmt {
            Stmt::Let { name, ty, init, span } => {
                let it = self.infer_expr(init)?;
                if let Some(ann) = ty {
                    self.unify(ann, &it, *span)
                        .map_err(|e| Error::type_error(e, *span))?;
                    self.bind(name, ann.clone());
                } else {
                    self.bind(name, it);
                }
                Ok(Some(Ty::Unit))
            }
            Stmt::Assign { name, value, span } => {
                let vt = self.infer_expr(value)?;
                let old = self.lookup(name).ok_or_else(|| {
                    Error::type_error(format!("undefined variable `{}`", name), *span)
                })?;
                // 循环变量不可赋值（D-046 5.9）。内层 `let` 遮蔽后的同名
                // 变量绑定在别的 scope，find_scope 找到的是新绑定 → 放行。
                if let Some(idx) = self.find_scope(name) {
                    if self
                        .active_for_vars
                        .iter()
                        .any(|(v, s)| *s == idx && v == name)
                    {
                        return Err(Error::type_error(
                            format!("cannot assign to loop variable `{}`", name),
                            *span,
                        ));
                    }
                }
                self.unify(&old, &vt, *span)
                    .map_err(|e| Error::type_error(format!("assignment to `{}`: {}", name, e), *span))?;
                Ok(Some(Ty::Unit))
            }
            Stmt::Expr(e) => Ok(Some(self.infer_expr(e)?)),
            Stmt::Return { value, span } => {
                let t = match value {
                    Some(e) => self.infer_expr(e)?,
                    None => Ty::Unit,
                };
                self.unify(&self.current_ret.clone(), &t, *span)
                    .map_err(|e| Error::type_error(format!("return: {}", e), *span))?;
                Ok(None)
            }
            Stmt::If { cond, then, otherwise, span } => {
                let ct = self.infer_expr(cond)?;
                self.unify(&Ty::Bool, &ct, *span)
                    .map_err(|e| Error::type_error(format!("if condition: {}", e), *span))?;
                let tt = self.infer_block(then)?;
                match otherwise.as_deref() {
                    None => Ok(Some(Ty::Unit)),
                    Some(ElseBranch::Block(b)) => {
                        let et = self.infer_block(b)?;
                        self.merge_branches(tt, et, *span)
                    }
                    Some(ElseBranch::ElseIf(s)) => {
                        let et = self.infer_stmt(s)?;
                        self.merge_branches(tt, et, *span)
                    }
                }
            }
            Stmt::While { cond, body, span } => {
                let ct = self.infer_expr(cond)?;
                self.unify(&Ty::Bool, &ct, *span)
                    .map_err(|e| Error::type_error(format!("while condition: {}", e), *span))?;
                self.loop_depth += 1;
                let body_res = self.infer_block(body);
                self.loop_depth -= 1;
                body_res?;
                Ok(Some(Ty::Unit))
            }
            Stmt::For { var, iter, body, span } => {
                let var_ty = match iter {
                    ForIter::Range { start, end } => {
                        let st = self.infer_expr(start)?;
                        let et = self.infer_expr(end)?;
                        self.unify(&Ty::I64, &st, *span)
                            .map_err(|e| Error::type_error(format!("for range start: {}", e), *span))?;
                        self.unify(&Ty::I64, &et, *span)
                            .map_err(|e| Error::type_error(format!("for range end: {}", e), *span))?;
                        Ty::I64
                    }
                    ForIter::List(e) => {
                        // ADR-018: `for x in arr` — the loop variable takes the
                        // array's element type; an unresolved base is pinned to
                        // `Arr(elem)` so the first push can still settle it.
                        let it = self.infer_expr(e)?;
                        let elem = match self.resolve(&it) {
                            Ty::Arr(t) => *t,
                            Ty::Var(_) => {
                                let elem = self.fresh();
                                self.unify(&it, &Ty::Arr(Box::new(elem.clone())), *span)
                                    .map_err(|e| Error::type_error(format!("for-in: {}", e), *span))?;
                                elem
                            }
                            other => {
                                return Err(Error::type_error(
                                    format!(
                                        "for-in expects a range or an array, found {}",
                                        other.render()
                                    ),
                                    *span,
                                ))
                            }
                        };
                        elem
                    }
                };
                self.push_scope();
                self.bind(var, var_ty);
                self.active_for_vars.push((var.clone(), self.scopes.len() - 1));
                self.loop_depth += 1;
                let body_res = self.infer_block(body);
                self.loop_depth -= 1;
                self.active_for_vars.pop();
                self.pop_scope();
                body_res?;
                Ok(Some(Ty::Unit))
            }
            Stmt::Break { span } => self.check_loop_jump("break", *span),
            Stmt::Continue { span } => self.check_loop_jump("continue", *span),
        }
    }

    /// `break` / `continue` diverge: no value flows out of them, so `None`
    /// lets the branch merger absorb that path exactly like `return`.
    fn check_loop_jump(&self, kw: &str, span: Span) -> Result<Option<Ty>> {
        if self.loop_depth == 0 {
            return Err(Error::type_error(format!("`{}` outside of a loop", kw), span));
        }
        Ok(None)
    }

    /// Merge the types of an if's two branches. Divergence (`None`) is
    /// absorbed by the other branch. When both produce a value they are
    /// unified so freshly-inferred `Var`s (e.g. in tuple elements) converge
    /// instead of comparing unequal and collapsing to `Unit`.
    fn merge_branches(
        &mut self,
        a: Option<Ty>,
        b: Option<Ty>,
        span: Span,
    ) -> Result<Option<Ty>> {
        match (a, b) {
            (None, x) | (x, None) => Ok(x),
            (Some(x), Some(y)) => {
                self.unify(&x, &y, span)
                    .map_err(|e| Error::type_error(format!("if branches: {}", e), span))?;
                Ok(Some(self.resolve(&x)))
            }
        }
    }

    // ---- expressions ----

    fn infer_expr(&mut self, expr: &Expr) -> Result<Ty> {
        match expr {
            Expr::Int(..) => Ok(Ty::I64),
            Expr::Float(..) => Ok(Ty::F64),
            Expr::Bool(..) => Ok(Ty::Bool),
            Expr::Str(..) => Ok(Ty::Str),
            Expr::Var(name, span) => self.lookup(name).ok_or_else(|| {
                Error::type_error(format!("undefined variable `{}`", name), *span)
            }),
            Expr::Call { name, args, span } => self.infer_call(name, args, *span),
            Expr::Unary { op, expr, span } => {
                let t = self.infer_expr(expr)?;
                match op {
                    UnOp::Neg => {
                        if !matches!(self.resolve(&t), Ty::I64 | Ty::F64 | Ty::Var(_)) {
                            return Err(Error::type_error("unary `-` requires a number", *span));
                        }
                        Ok(t)
                    }
                    UnOp::Not => {
                        self.unify(&Ty::Bool, &t, *span)
                            .map_err(|e| Error::type_error(format!("unary `!`: {}", e), *span))?;
                        Ok(Ty::Bool)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let lt = self.infer_expr(lhs)?;
                let rt = self.infer_expr(rhs)?;
                match op {
                    BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod => {
                        self.unify(&lt, &rt, *span)
                            .map_err(|e| Error::type_error(format!("arithmetic: {}", e), *span))?;
                        let t = self.resolve(&lt);
                        // `+` doubles as string concatenation (语言规格 §3.2).
                        if *op == BinOp::Add && t == Ty::Str {
                            return Ok(Ty::Str);
                        }
                        if !matches!(t, Ty::I64 | Ty::F64 | Ty::Var(_)) {
                            return Err(Error::type_error(
                                format!("arithmetic requires numbers, found {}", t.render()),
                                *span,
                            ));
                        }
                        Ok(lt)
                    }
                    BinOp::Eq | BinOp::NotEq => {
                        self.unify(&lt, &rt, *span)
                            .map_err(|e| Error::type_error(format!("comparison: {}", e), *span))?;
                        let t = self.resolve(&lt);
                        if matches!(t, Ty::Tuple(_) | Ty::Arr(_) | Ty::Unit) {
                            return Err(Error::type_error(
                                format!("equality comparison is not supported for {}", t.render()),
                                *span,
                            ));
                        }
                        if matches!(t, Ty::Var(_)) {
                            // Equality has an opcode for every concrete type we
                            // allow here; pin an otherwise-unconstrained value
                            // to the language default so a later assignment
                            // cannot turn it into a heap-backed value.
                            self.unify(&lt, &Ty::I64, *span)
                                .map_err(|e| Error::type_error(e, *span))?;
                        }
                        Ok(Ty::Bool)
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        self.unify(&lt, &rt, *span)
                            .map_err(|e| Error::type_error(format!("comparison: {}", e), *span))?;
                        let t = self.resolve(&lt);
                        if !matches!(t, Ty::I64 | Ty::F64 | Ty::Str | Ty::Var(_)) {
                            return Err(Error::type_error(
                                format!("ordering requires numbers or str, found {}", t.render()),
                                *span,
                            ));
                        }
                        Ok(Ty::Bool)
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&Ty::Bool, &lt, *span).map_err(|e| {
                            Error::type_error(format!("`&&`/`||` left operand: {}", e), *span)
                        })?;
                        self.unify(&Ty::Bool, &rt, *span).map_err(|e| {
                            Error::type_error(format!("`&&`/`||` right operand: {}", e), *span)
                        })?;
                        Ok(Ty::Bool)
                    }
                }
            }
            Expr::IfExpr { cond, then, otherwise, span } => {
                let ct = self.infer_expr(cond)?;
                self.unify(&Ty::Bool, &ct, *span)
                    .map_err(|e| Error::type_error(format!("if condition: {}", e), *span))?;
                let tt = self.infer_block(then)?;
                let et = self.infer_block(otherwise)?;
                Ok(self.merge_branches(tt, et, *span)?.unwrap_or(Ty::Unit))
            }
            Expr::Tuple(items, _) => {
                // `()` is the unit value, not a 0-tuple (Q15): `Ty::Tuple([])`
                // rendered as `()` too but did not unify with `Ty::Unit`, and
                // equality on it took the tuple-rejection path.
                if items.is_empty() {
                    return Ok(Ty::Unit);
                }
                let mut ts = Vec::new();
                for it in items {
                    ts.push(self.infer_expr(it)?);
                }
                Ok(Ty::Tuple(ts))
            }
            Expr::Index { base, index, span } => {
                let base_ty = self.infer_expr(base)?;
                let bt = self.resolve(&base_ty);
                match bt {
                    Ty::Tuple(ts) => {
                        if let Some(t) = ts.get(*index) {
                            return Ok(t.clone());
                        }
                        // 长度不够时，若 base 仍是一个可重绑定的推断变量
                        // （例如参数先被 `.0` 反推成短元组），把它扩长成至少
                        // index+1 元 —— ADR-006 的"至少 N+1 元"语义需要对
                        // 已部分推断的元组续推，否则 `func f(t) { t.0 + t.1 }`
                        // 在第二个索引处就会误报越界。运行时边界本就接受
                        // 更长的元组（type_matches 长度≥），这里对齐。
                        if let Ty::Var(id) = base_ty {
                            let mut elems = ts.clone();
                            while elems.len() <= *index {
                                elems.push(self.fresh());
                            }
                            let wanted = elems[*index].clone();
                            self.subst[id] = Some(Ty::Tuple(elems));
                            return Ok(wanted);
                        }
                        Err(Error::type_error(
                            format!("tuple index {} out of range (len {})", index, ts.len()),
                            *span,
                        ))
                    }
                    Ty::Var(_) => {
                        // Reverse-infer: an unknown indexed base must be a
                        // tuple with at least index+1 elements.
                        let elems: Vec<Ty> = (0..=*index).map(|_| self.fresh()).collect();
                        let tuple_ty = Ty::Tuple(elems.clone());
                        self.unify(&base_ty, &tuple_ty, *span)
                            .map_err(|e| Error::type_error(e, *span))?;
                        Ok(elems[*index].clone())
                    }
                    other => Err(Error::type_error(
                        format!("cannot index {} with `.{}`", other.render(), index),
                        *span,
                    )),
                }
            }
            Expr::Array(items, span) => {
                // Empty `[]` defers to usage: the element variable settles on
                // the first `push` / index / annotation. Non-empty: all
                // elements unify with the first (homogeneous arrays, ADR-018).
                if items.is_empty() {
                    return Ok(Ty::Arr(Box::new(self.fresh())));
                }
                let first = self.infer_expr(&items[0])?;
                for it in &items[1..] {
                    let t = self.infer_expr(it)?;
                    self.unify(&first, &t, *span)
                        .map_err(|e| Error::type_error(format!("array literal: {}", e), *span))?;
                }
                Ok(Ty::Arr(Box::new(self.resolve(&first))))
            }
            Expr::DynIndex { base, index, span } => {
                let it = self.infer_expr(index)?;
                self.unify(&Ty::I64, &it, *span)
                    .map_err(|e| Error::type_error(format!("array index: {}", e), *span))?;
                let base_ty = self.infer_expr(base)?;
                match self.resolve(&base_ty) {
                    Ty::Arr(t) => Ok(self.resolve(&t)),
                    Ty::Var(_) => {
                        let elem = self.fresh();
                        self.unify(&base_ty, &Ty::Arr(Box::new(elem.clone())), *span)
                            .map_err(|e| Error::type_error(e, *span))?;
                        Ok(elem)
                    }
                    // Guide the common mistake to the right syntax.
                    Ty::Tuple(_) => Err(Error::type_error(
                        "tuples are indexed with `.0` / `.1`; `[i]` is for arrays",
                        *span,
                    )),
                    other => Err(Error::type_error(
                        format!("cannot index {} with `[i]`", other.render()),
                        *span,
                    )),
                }
            }
            Expr::BlockExpr(b, _) => Ok(self.infer_block(b)?.unwrap_or(Ty::Unit)),
        }
    }

    fn infer_call(&mut self, name: &str, args: &[Expr], span: Span) -> Result<Ty> {
        // `abs`/`min`/`max` are numeric-generic: they keep the argument type
        // instead of forcing f64. Decided here (not by overload resolution) so
        // codegen can pick the specialization from one resolved argument type.
        if let Some(arity) = numeric_builtin_arity(name) {
            if args.len() != arity {
                return Err(Error::type_error(
                    format!("`{}` expects {} argument(s), got {}", name, arity, args.len()),
                    span,
                ));
            }
            let mut tys = Vec::with_capacity(args.len());
            for a in args {
                tys.push(self.infer_expr(a)?);
            }
            // Pick the family from the arguments that are already known. An
            // integer literal therefore pulls the whole call to i64, exactly
            // like `x + 1` does; with no evidence either way we fall back to
            // f64, which is what this family meant before i64 support existed.
            let known: Vec<Ty> = tys.iter().map(|t| self.resolve(t)).collect();
            let int_evident = known.iter().any(|t| matches!(t, Ty::I64));
            let float_evident = known.iter().any(|t| matches!(t, Ty::F64));
            // A str/bool/tuple matches neither branch and fails the unify below
            // with a message naming the builtin and the offending type.
            let family = if int_evident && !float_evident { Ty::I64 } else { Ty::F64 };
            for t in &tys {
                self.unify(&family, t, span)
                    .map_err(|e| Error::type_error(format!("{}: {}", name, e), span))?;
            }
            return Ok(family);
        }

        // ADR-018: the three collection builtins are polymorphic over the
        // array's element type, which the fixed signatures below cannot
        // express — they are typed here instead.
        if name == "push" {
            if args.len() != 2 {
                return Err(Error::type_error(
                    format!("`push` expects 2 arguments, got {}", args.len()),
                    span,
                ));
            }
            let arr_ty = self.infer_expr(&args[0])?;
            let elem_ty = self.infer_expr(&args[1])?;
            let elem = match self.resolve(&arr_ty) {
                Ty::Arr(t) => *t,
                // An unresolved base gets pinned to `Arr(elem)` so
                // `let fails = []` + `push(fails, "x")` infers `[str]`.
                Ty::Var(_) => {
                    let elem = self.fresh();
                    self.unify(&arr_ty, &Ty::Arr(Box::new(elem.clone())), span)
                        .map_err(|e| Error::type_error(format!("push: {}", e), span))?;
                    elem
                }
                other => {
                    return Err(Error::type_error(
                        format!("push expects an array, found {}", other.render()),
                        span,
                    ))
                }
            };
            self.unify(&elem, &elem_ty, span)
                .map_err(|e| Error::type_error(format!("push: {}", e), span))?;
            return Ok(Ty::Unit);
        }
        if name == "len" {
            if args.len() != 1 {
                return Err(Error::type_error(
                    format!("`len` expects 1 argument, got {}", args.len()),
                    span,
                ));
            }
            let at = self.infer_expr(&args[0])?;
            return match self.resolve(&at) {
                Ty::Str | Ty::Tuple(_) | Ty::Arr(_) => Ok(Ty::I64),
                // An unresolved base keeps the pre-ADR-018 meaning (`str`):
                // `func f(s) { len(s) }` called with a string must keep
                // compiling. An array-typed base never lands here — it is
                // already `Arr(..)`, and array params used only through len
                // take an annotation (`xs: [str]`) as the escape hatch.
                Ty::Var(_) => {
                    self.unify(&at, &Ty::Str, span)
                        .map_err(|e| Error::type_error(format!("len: {}", e), span))?;
                    Ok(Ty::I64)
                }
                other => Err(Error::type_error(
                    format!("len expects a str, tuple or array, found {}", other.render()),
                    span,
                )),
            };
        }
        if name == "contains" {
            if args.len() != 2 {
                return Err(Error::type_error(
                    format!("`contains` expects 2 arguments, got {}", args.len()),
                    span,
                ));
            }
            let hay = self.infer_expr(&args[0])?;
            let needle = self.infer_expr(&args[1])?;
            // The needle type disambiguates an unresolved haystack: a str
            // needle keeps the classic substring reading (`contains(s, "x")`),
            // any other needle can only be an element of an array. Either way
            // the haystack itself is pinned — an unconstrained variable would
            // otherwise default to i64 at signature resolution and reject
            // every boundary call.
            let needle_resolved = self.resolve(&needle);
            let elem = match self.resolve(&hay) {
                Ty::Arr(t) => *t,
                Ty::Str => Ty::Str,
                Ty::Var(_) if matches!(needle_resolved, Ty::Str) => {
                    self.unify(&hay, &Ty::Str, span)
                        .map_err(|e| Error::type_error(format!("contains: {}", e), span))?;
                    Ty::Str
                }
                Ty::Var(_) => {
                    let arr = Ty::Arr(Box::new(needle_resolved.clone()));
                    self.unify(&hay, &arr, span)
                        .map_err(|e| Error::type_error(format!("contains: {}", e), span))?;
                    needle_resolved
                }
                other => {
                    return Err(Error::type_error(
                        format!("contains expects a str or array, found {}", other.render()),
                        span,
                    ))
                }
            };
            self.unify(&elem, &needle, span)
                .map_err(|e| Error::type_error(format!("contains: {}", e), span))?;
            return Ok(Ty::Bool);
        }

        // Built-in functions have special signatures.
        if let Some((params, ret)) = builtin_signature(name, args.len()) {
            // `to_str` is type-polymorphic over the scalar types only. We still
            // infer the argument and check it explicitly here: a Var is allowed
            // because unresolved parameters default to i64, but heap-backed
            // values would otherwise be stringified as raw heap indices.
            let is_to_str = name == "to_str";
            for (arg, expected) in args.iter().zip(params.iter()) {
                let at = self.infer_expr(arg)?;
                if is_to_str {
                    let resolved = self.resolve(&at);
                    if matches!(resolved, Ty::Tuple(_) | Ty::Arr(_) | Ty::Handle(_) | Ty::Unit) {
                        return Err(Error::type_error(
                            format!("to_str does not support {}", resolved.render()),
                            span,
                        ));
                    }
                    if matches!(resolved, Ty::Var(_)) {
                        // Unconstrained values default to i64, matching the
                        // resolved signature seen by codegen.
                        self.unify(&at, &Ty::I64, span)
                            .map_err(|e| Error::type_error(format!("to_str: {}", e), span))?;
                    }
                } else {
                    self.unify(expected, &at, span)
                        .map_err(|e| Error::type_error(format!("{}: {}", name, e), span))?;
                }
            }
            return Ok(ret);
        }

        let sig = self.sigs.get(name).cloned().ok_or_else(|| {
            Error::type_error(format!("undefined function `{}`", name), span)
        })?;
        if sig.params.len() != args.len() {
            return Err(Error::type_error(
                format!(
                    "`{}` expects {} argument(s), got {}",
                    name,
                    sig.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        for (arg, expected) in args.iter().zip(sig.params.iter()) {
            let at = self.infer_expr(arg)?;
            self.unify(expected, &at, span)
                .map_err(|e| Error::type_error(format!("{}: {}", name, e), span))?;
        }
        Ok(sig.ret)
    }
}

fn occurs(id: usize, ty: &Ty) -> bool {
    match ty {
        Ty::Var(j) => *j == id,
        Ty::Tuple(ts) => ts.iter().any(|t| occurs(id, t)),
        Ty::Arr(t) => occurs(id, t),
        _ => false,
    }
}

/// Replace any leftover inference variables with i64, the default type.
fn default_vars(ty: Ty) -> Ty {
    match ty {
        Ty::Var(_) => Ty::I64,
        Ty::Tuple(ts) => Ty::Tuple(ts.into_iter().map(default_vars).collect()),
        Ty::Arr(t) => Ty::Arr(Box::new(default_vars(*t))),
        other => other,
    }
}

/// True if the name belongs to the built-in white list.
pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "sqrt"
            | "floor"
            | "ceil"
            | "min"
            | "max"
            | "pow"
            | "len"
            | "contains"
            | "substr"
            | "to_i64"
            | "to_f64"
            | "to_str"
            | "push"
    )
}

/// Arity of the builtins that accept both `i64` and `f64`. `None` for every
/// other name. Kept next to [`builtin_signature`] so the two tables cannot
/// drift: a name in one must not appear in the other.
pub fn numeric_builtin_arity(name: &str) -> Option<usize> {
    match name {
        "abs" => Some(1),
        "min" | "max" => Some(2),
        _ => None,
    }
}

/// Return type of a builtin given its stable id (see `ir::builtin`).
pub fn builtin_ret(id: u8) -> Ty {
    match id {
        0 | 1 | 2 | 3 | 4 | 5 | 6 => Ty::F64,   // abs sqrt floor ceil pow min max
        7 => Ty::I64,                              // len
        8 => Ty::Bool,                             // contains
        9 | 12 | 13 | 14 | 15 => Ty::Str,         // substr + to_str variants
        10 => Ty::I64,                             // to_i64
        11 => Ty::F64,                             // to_f64
        16 | 17 | 18 => Ty::I64,                   // abs/min/max on i64
        19 => Ty::Unit,                            // push (ADR-018)
        _ => Ty::Unit,
    }
}

/// Built-in (white-listed, side-effect-free) function signatures.
///
/// `abs`/`min`/`max` are deliberately absent: they are handled by
/// [`numeric_builtin_arity`] because their result type follows the arguments.
fn builtin_signature(name: &str, argc: usize) -> Option<(Vec<Ty>, Ty)> {
    // `len` / `contains` / `push` are typed in `infer_call` (element-type
    // polymorphism, ADR-018) and must not appear here.
    let sig = match (name, argc) {
        ("substr", 3) => (vec![Ty::Str, Ty::I64, Ty::I64], Ty::Str),
        ("to_i64", 1) => (vec![Ty::F64], Ty::I64),
        ("to_f64", 1) => (vec![Ty::I64], Ty::F64),
        ("to_str", 1) => (vec![Ty::Unit], Ty::Str), // placeholder; unification skipped
        ("sqrt", 1) | ("floor", 1) | ("ceil", 1) => (vec![Ty::F64], Ty::F64),
        ("pow", 2) => (vec![Ty::F64, Ty::F64], Ty::F64),
        _ => return None,
    };
    Some(sig)
}
