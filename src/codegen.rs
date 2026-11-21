//! AST → typed bytecode compiler.
//!
//! The type checker has already resolved every function signature and every
//! expression's type (via unification). This pass re-derives the concrete type
//! of each expression from that resolved signature table and emits the matching
//! typed opcode. Variables are bound to register numbers at compile time, so
//! the VM never hashes a name or inspects a runtime type tag on the hot path.

use crate::ast::{
    BinOp, Block, ElseBranch, Expr, ForIter, FuncDecl, Program, Stmt, UnOp,
};
use crate::error::{Error, Result};
use crate::ir::{builtin, FuncId, Function, Instr, Op, Reg, Regs};
use crate::span::Span;
use crate::ty::Ty;
use crate::typecheck::{builtin_ret, SigTable};
use std::collections::HashMap;
use std::rc::Rc;

/// Compiles a whole program. Host function names are mapped to their stable
/// slot ids supplied by the engine, so call sites are resolved once here.
pub struct Codegen<'a> {
    sigs: &'a SigTable,
    host_ids: &'a HashMap<String, u16>,
    strings: Vec<Rc<str>>,
    string_ids: HashMap<String, u32>,
    ids: HashMap<String, FuncId>,
}

impl<'a> Codegen<'a> {
    pub fn new(sigs: &'a SigTable, host_ids: &'a HashMap<String, u16>) -> Self {
        Self {
            sigs,
            host_ids,
            strings: Vec::new(),
            string_ids: HashMap::new(),
            ids: HashMap::new(),
        }
    }

    pub fn compile(mut self, program: &Program) -> Result<crate::ir::Program> {
        for (i, f) in program.funcs.iter().enumerate() {
            self.ids.insert(f.name.clone(), i as FuncId);
        }
        let mut funcs = Vec::with_capacity(program.funcs.len());
        for f in &program.funcs {
            funcs.push(self.compile_func(f)?);
        }
        Ok(crate::ir::Program {
            funcs,
            ids: self.ids,
            strings: self.strings,
        })
    }

    fn compile_func(&mut self, f: &FuncDecl) -> Result<Function> {
        let sig = self.sigs.get(&f.name).ok_or_else(|| {
            Error::type_error(format!("missing signature for `{}`", f.name), f.span)
        })?;
        let params = sig.params.clone();
        let ret = sig.ret.clone();

        let mut cg = FuncCodegen {
            sigs: self.sigs,
            host_ids: self.host_ids,
            ids: &self.ids,
            strings: &mut self.strings,
            string_ids: &mut self.string_ids,
            code: Vec::new(),
            next_reg: 0,
            reg_overflow: false,
            env: Vec::new(),
            loops: Vec::new(),
        };

        cg.env.push(HashMap::new());
        for (i, p) in f.params.iter().enumerate() {
            let ty = params.get(i).cloned().unwrap_or(Ty::I64);
            cg.env.last_mut().unwrap().insert(p.name.clone(), (i as Reg, ty));
        }
        cg.next_reg = f.params.len() as u16;

        let (value_reg, _) = cg.compile_block(&f.body)?;
        cg.emit(Op::Ret { src: value_reg }, f.body.span);

        if cg.reg_overflow {
            return Err(Error::type_error(
                format!("function `{}` is too complex (needs > 65535 registers)", f.name),
                f.span,
            ));
        }

        Ok(Function {
            name: f.name.clone(),
            code: cg.code,
            nregs: cg.next_reg,
            params,
            ret,
        })
    }
}

struct FuncCodegen<'a> {
    sigs: &'a SigTable,
    host_ids: &'a HashMap<String, u16>,
    ids: &'a HashMap<String, FuncId>,
    strings: &'a mut Vec<Rc<str>>,
    string_ids: &'a mut HashMap<String, u32>,
    code: Vec<Instr>,
    next_reg: u16,
    /// Set when a function needs more than `u16::MAX` registers; surfaced as a
    /// compile error instead of a panic.
    reg_overflow: bool,
    env: Vec<HashMap<String, (Reg, Ty)>>,
    /// One entry per enclosing loop, innermost last. `break` / `continue`
    /// record a placeholder jump here; the loop backfills it once its
    /// boundaries are known.
    loops: Vec<LoopCtx>,
}

/// Placeholder jumps emitted inside a loop body, resolved when the loop ends.
#[derive(Default)]
struct LoopCtx {
    /// Targets of `break`: the first instruction after the loop.
    breaks: Vec<usize>,
    /// Targets of `continue`: the loop's next-iteration entry point, which is
    /// the condition test for `while` and the increment for `for`.
    continues: Vec<usize>,
}

impl<'a> FuncCodegen<'a> {
    fn emit(&mut self, op: Op, span: Span) -> usize {
        self.code.push(Instr::new(op, span));
        self.code.len() - 1
    }

    fn fresh(&mut self) -> Reg {
        let r = self.next_reg;
        match self.next_reg.checked_add(1) {
            Some(next) => self.next_reg = next,
            // Not a panic: a pathological script must produce an error, never
            // take the host down (see PROJECT_PLAN.md, 安全边界).
            None => self.reg_overflow = true,
        }
        r
    }

    fn intern_str(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.string_ids.get(s) {
            return id;
        }
        let id = self.strings.len() as u32;
        self.strings.push(Rc::from(s));
        self.string_ids.insert(s.to_string(), id);
        id
    }

    fn lookup(&self, name: &str) -> Option<(Reg, Ty)> {
        for scope in self.env.iter().rev() {
            if let Some((reg, ty)) = scope.get(name) {
                return Some((*reg, ty.clone()));
            }
        }
        None
    }

    fn bind(&mut self, name: &str, reg: Reg, ty: Ty) {
        self.env.last_mut().unwrap().insert(name.to_string(), (reg, ty));
    }

    fn emit_const_unit(&mut self, span: Span) -> (Reg, Ty) {
        let r = self.fresh();
        self.emit(Op::ConstUnit { dst: r }, span);
        (r, Ty::Unit)
    }

    /// Emit a branch taken when `cond` is false; returns the branch's index so
    /// the caller can patch its target once the destination is known.
    ///
    /// The condition is compiled normally, then the *last* emitted instruction
    /// is rewritten into a fused compare-and-branch when it is a comparison
    /// producing exactly `cond`'s register. The comparison's result register is
    /// dead in every caller (if/while/for conditions are consumed only by the
    /// branch), so dropping the materialised bool is safe — and it saves one
    /// dispatch plus one register write per loop iteration.
    fn compile_cond_false(&mut self, cond: &Expr, span: Span) -> Result<usize> {
        // Match the AST shape *before* emitting anything: `cmp lhs, int-literal`
        // becomes one fused compare-immediate branch, so the literal never
        // materialises as a ConstI64 + register. This is the immediate-valued
        // counterpart of the existing register-register `fuse_branch`. The
        // type checker unified lhs with the literal, so lhs is I64 here; the
        // guard avoids compiling lhs twice for every other condition shape.
        if let Expr::Binary { op, lhs, rhs, .. } = cond {
            if is_i64_cmp(*op) {
                if let Expr::Int(n, _) = &**rhs {
                    let (lr, _) = self.compile_expr(lhs)?;
                    let fused = match op {
                        // Emit the negation: this branch is taken when cond
                        // is FALSE, matching `fuse_branch`'s convention.
                        BinOp::Eq => Op::JmpIfNeI64Imm { a: lr, imm: *n, target: usize::MAX },
                        BinOp::NotEq => Op::JmpIfEqI64Imm { a: lr, imm: *n, target: usize::MAX },
                        BinOp::Lt => Op::JmpIfGeI64Imm { a: lr, imm: *n, target: usize::MAX },
                        BinOp::LtEq => Op::JmpIfGtI64Imm { a: lr, imm: *n, target: usize::MAX },
                        BinOp::Gt => Op::JmpIfLeI64Imm { a: lr, imm: *n, target: usize::MAX },
                        BinOp::GtEq => Op::JmpIfLtI64Imm { a: lr, imm: *n, target: usize::MAX },
                        _ => unreachable!("is_i64_cmp guard"),
                    };
                    return Ok(self.emit(fused, span));
                }
            }
        }
        let (cr, _) = self.compile_expr(cond)?;
        if let Some(last) = self.code.last() {
            if let Some(fused) = fuse_branch(last.op, cr, usize::MAX) {
                let idx = self.code.len() - 1;
                self.code[idx].op = fused;
                return Ok(idx);
            }
        }
        Ok(self.emit(Op::JumpIfFalse { cond: cr, target: usize::MAX }, span))
    }

    /// Patch a branch emitted by `compile_cond_false` (or a plain `Jump`).
    fn patch_jump(&mut self, idx: usize, target: usize) {
        match &mut self.code[idx].op {
            Op::Jump { target: t } => *t = target,
            Op::JumpIfFalse { target: t, .. } => *t = target,
            Op::JmpIfEqI64 { target: t, .. }
            | Op::JmpIfNeI64 { target: t, .. }
            | Op::JmpIfLtI64 { target: t, .. }
            | Op::JmpIfLeI64 { target: t, .. }
            | Op::JmpIfGtI64 { target: t, .. }
            | Op::JmpIfGeI64 { target: t, .. }
            | Op::JmpIfEqF64 { target: t, .. }
            | Op::JmpIfNeF64 { target: t, .. }
            | Op::JmpIfLtF64 { target: t, .. }
            | Op::JmpIfLeF64 { target: t, .. }
            | Op::JmpIfGtF64 { target: t, .. }
            | Op::JmpIfGeF64 { target: t, .. }
            | Op::JmpIfEqI64Imm { target: t, .. }
            | Op::JmpIfNeI64Imm { target: t, .. }
            | Op::JmpIfLtI64Imm { target: t, .. }
            | Op::JmpIfLeI64Imm { target: t, .. }
            | Op::JmpIfGtI64Imm { target: t, .. }
            | Op::JmpIfGeI64Imm { target: t, .. } => *t = target,
            other => unreachable!("patch_jump on non-branch instruction: {:?}", other),
        }
    }

    // ---- statements ----

    /// Compile a block; returns the register holding its value and that type.
    /// `None` from a statement means it diverged (`return`).
    fn compile_block(&mut self, block: &Block) -> Result<(Reg, Ty)> {
        let (reg, ty, _) = self.compile_block_with_divergence(block)?;
        Ok((reg, ty))
    }

    /// Same as [`Self::compile_block`], but also reports whether the block
    /// ended in `return`. Callers that merge branch types need this: a
    /// diverging block contributes a placeholder `Unit` value, not a real one.
    fn compile_block_with_divergence(&mut self, block: &Block) -> Result<(Reg, Ty, bool)> {
        self.env.push(HashMap::new());
        let mut last: Option<(Reg, Ty)> = None;
        let mut diverged = false;
        for stmt in &block.stmts {
            match self.compile_stmt(stmt)? {
                Some(v) => last = Some(v),
                None => {
                    diverged = true;
                    break;
                }
            }
        }
        self.env.pop();
        if diverged {
            // A return was emitted; this value is unreachable but required
            // to keep the instruction stream well-formed.
            let (reg, ty) = self.emit_const_unit(block.span);
            return Ok((reg, ty, true));
        }
        let (reg, ty) = last.unwrap_or_else(|| self.emit_const_unit(block.span));
        Ok((reg, ty, false))
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<Option<(Reg, Ty)>> {
        match stmt {
            Stmt::Let { name, init, .. } => {
                let (r, ty) = self.compile_expr(init)?;
                self.bind(name, r, ty.clone());
                Ok(Some((r, ty)))
            }
            Stmt::Assign { name, value, span } => {
                let (slot, _) = self.lookup(name).ok_or_else(|| {
                    // Unreachable through Engine (the checker rejects unknown
                    // names first); `Type`, not `Runtime`, because this is a
                    // compile error and the C ABI maps Runtime to a runtime
                    // failure code.
                    Error::type_error(format!("undefined variable `{}`", name), *span)
                })?;
                // Compile the right-hand side straight into the variable's
                // slot when its lowering owns a result register. Nodes that
                // produce their result through an inner register fall back to
                // the ordinary path and get an explicit Mov below.
                let vr = self.compile_expr_into(value, slot)?;
                if vr != slot {
                    self.emit(Op::Mov { dst: slot, src: vr }, *span);
                }
                // The checker types an assignment as Unit; carrying the
                // variable's type made the codegen-derived block value
                // disagree with the resolved signature when an assignment is
                // the trailing statement of a function or block expression.
                Ok(Some((slot, Ty::Unit)))
            }
            Stmt::Expr(e) => Ok(Some(self.compile_expr(e)?)),
            Stmt::Return { value, span } => {
                let r = match value {
                    Some(e) => self.compile_expr(e)?.0,
                    None => self.emit_const_unit(*span).0,
                };
                self.emit(Op::Ret { src: r }, *span);
                Ok(None)
            }
            Stmt::If { cond, then, otherwise, span } => {
                Ok(Some(self.compile_if(cond, then, otherwise.as_deref(), *span)?))
            }
            Stmt::While { cond, body, span } => {
                self.compile_while(cond, body, *span)?;
                Ok(Some(self.emit_const_unit(*span)))
            }
            Stmt::For { var, iter, body, span } => {
                self.compile_for(var, iter, body, *span)?;
                Ok(Some(self.emit_const_unit(*span)))
            }
            Stmt::Break { span } => {
                let j = self.emit(Op::Jump { target: usize::MAX }, *span);
                match self.loops.last_mut() {
                    Some(ctx) => {
                        ctx.breaks.push(j);
                        Ok(None)
                    }
                    None => Err(Error::type_error("`break` outside of a loop", *span)),
                }
            }
            Stmt::Continue { span } => {
                let j = self.emit(Op::Jump { target: usize::MAX }, *span);
                match self.loops.last_mut() {
                    Some(ctx) => {
                        ctx.continues.push(j);
                        Ok(None)
                    }
                    None => Err(Error::type_error("`continue` outside of a loop", *span)),
                }
            }
        }
    }

    /// Compile an if (statement or expression) into a value-producing sequence.
    /// Returns `(result_reg, then_branch_type)`.
    fn compile_if(
        &mut self,
        cond: &Expr,
        then: &Block,
        otherwise: Option<&ElseBranch>,
        span: Span,
    ) -> Result<(Reg, Ty)> {
        let jf = self.compile_cond_false(cond, span)?;
        let result = self.fresh();

        let (tr, tty, then_diverged) = self.compile_block_with_divergence(then)?;
        self.emit(Op::Mov { dst: result, src: tr }, span);
        let je = self.emit(Op::Jump { target: usize::MAX }, span);

        let else_start = self.code.len();
        self.patch_jump(jf, else_start);

        let (er, ety) = match otherwise {
            None => self.emit_const_unit(span),
            Some(ElseBranch::Block(b)) => self.compile_block(b)?,
            Some(ElseBranch::ElseIf(s)) => match s {
                Stmt::If { cond, then, otherwise, span } => {
                    self.compile_if(cond, then, otherwise.as_deref(), *span)?
                }
                _ => self.emit_const_unit(span),
            },
        };
        self.emit(Op::Mov { dst: result, src: er }, span);

        let end = self.code.len();
        self.code[je].op = Op::Jump { target: end };
        // Mirror the checker: an if *statement* with no else is Unit no
        // matter what the then-branch produced (Rust semantics). Letting the
        // then-branch type escape here silently dropped the value at the
        // call boundary and bound a wrong type in `let x = { if c { 1 } }`
        // style block expressions.
        let result_ty = if then_diverged {
            ety
        } else if otherwise.is_none() {
            Ty::Unit
        } else {
            tty
        };
        Ok((result, result_ty))
    }

    fn compile_while(&mut self, cond: &Expr, body: &Block, span: Span) -> Result<()> {
        let loop_start = self.code.len();
        let jf = self.compile_cond_false(cond, span)?;
        self.loops.push(LoopCtx::default());
        self.compile_block(body)?;
        let ctx = self.loops.pop().unwrap_or_default();
        // `continue` re-tests the condition, so it re-enters at the loop head.
        for j in ctx.continues {
            self.patch_jump(j, loop_start);
        }
        self.emit(Op::Jump { target: loop_start }, span);
        let end = self.code.len();
        self.patch_jump(jf, end);
        for j in ctx.breaks {
            self.patch_jump(j, end);
        }
        Ok(())
    }

    fn compile_for(
        &mut self,
        var: &str,
        iter: &ForIter,
        body: &Block,
        span: Span,
    ) -> Result<()> {
        match iter {
            ForIter::Range { start, end } => {
                let (sr, _) = self.compile_expr(start)?;
                let (er, _) = self.compile_expr(end)?;
                let vr = self.fresh();
                self.emit(Op::Mov { dst: vr, src: sr }, span);

                let loop_start = self.code.len();
                // Jump when `!(vr < er)`, which is the loop's exit condition.
                let jf = self.emit(Op::JmpIfGeI64 { a: vr, b: er, target: usize::MAX }, span);

                self.env.push(HashMap::new());
                self.bind(var, vr, Ty::I64);
                self.loops.push(LoopCtx::default());
                self.compile_block(body)?;
                let ctx = self.loops.pop().unwrap_or_default();
                self.env.pop();

                // `continue` must still run the increment, otherwise the loop spins
                // forever on the same index. So it lands *before* the increment, not
                // on the loop head.
                let step = self.code.len();
                for j in ctx.continues {
                    self.patch_jump(j, step);
                }

                // Single-instruction increment instead of `const 1; add; mov`.
                self.emit(Op::AddI64Imm { dst: vr, src: vr, imm: 1 }, span);
                self.emit(Op::Jump { target: loop_start }, span);

                let end = self.code.len();
                self.patch_jump(jf, end);
                for j in ctx.breaks {
                    self.patch_jump(j, end);
                }
                Ok(())
            }
            ForIter::List(arr_expr) => {
                // ADR-018: desugar to an index walk over the array. The loop
                // reads `len` and the element fresh every iteration, so a
                // `push` inside the body extends the walk — same observable
                // behaviour as Python's `for x in list`.
                let (arr, arr_ty) = self.compile_expr(arr_expr)?;
                let elem_ty = match &arr_ty {
                    Ty::Arr(t) => (**t).clone(),
                    // The checker guarantees an array here; the placeholder
                    // only keeps the env typing well-formed.
                    _ => Ty::Unit,
                };

                let idx = self.fresh();
                self.emit(Op::ConstI64 { dst: idx, val: 0 }, span);
                let len = self.fresh();
                let vr = self.fresh();

                let loop_start = self.code.len();
                self.emit(Op::ArrLen { dst: len, src: arr }, span);
                let jf = self.emit(Op::JmpIfGeI64 { a: idx, b: len, target: usize::MAX }, span);
                self.emit(Op::ArrGet { dst: vr, base: arr, idx }, span);

                self.env.push(HashMap::new());
                self.bind(var, vr, elem_ty);
                self.loops.push(LoopCtx::default());
                self.compile_block(body)?;
                let ctx = self.loops.pop().unwrap_or_default();
                self.env.pop();

                // `continue` re-enters before the increment, mirroring the
                // range form.
                let step = self.code.len();
                for j in ctx.continues {
                    self.patch_jump(j, step);
                }
                self.emit(Op::AddI64Imm { dst: idx, src: idx, imm: 1 }, span);
                self.emit(Op::Jump { target: loop_start }, span);

                let end = self.code.len();
                self.patch_jump(jf, end);
                for j in ctx.breaks {
                    self.patch_jump(j, end);
                }
                Ok(())
            }
        }
    }

    // ---- expressions ----

    /// Compile `expr` with `hint` as the destination register of its
    /// result-producing instruction. Returns the register that actually holds
    /// the result; the caller must emit a `Mov` when it differs from `hint`.
    ///
    /// Operands are always compiled through the ordinary path, so the hint
    /// applies to the outermost node only. Every handler reads its operands
    /// before writing `dst` (`CallHost` gathers `Value`s first), so hinting a
    /// slot that is also an operand is safe.
    fn compile_expr_into(&mut self, expr: &Expr, hint: Reg) -> Result<Reg> {
        match expr {
            Expr::Int(n, s) => {
                self.emit(Op::ConstI64 { dst: hint, val: *n }, *s);
                Ok(hint)
            }
            Expr::Float(f, s) => {
                self.emit(Op::ConstF64 { dst: hint, val: *f }, *s);
                Ok(hint)
            }
            Expr::Bool(b, s) => {
                self.emit(Op::ConstBool { dst: hint, val: *b }, *s);
                Ok(hint)
            }
            Expr::Str(sval, s) => {
                let id = self.intern_str(sval);
                self.emit(Op::ConstStr { dst: hint, id }, *s);
                Ok(hint)
            }
            Expr::Unary { op, expr, span } => {
                let (r, ty) = self.compile_expr(expr)?;
                match op {
                    UnOp::Neg => match ty {
                        Ty::F64 => { self.emit(Op::NegF64 { dst: hint, src: r }, *span); }
                        _ => { self.emit(Op::NegI64 { dst: hint, src: r }, *span); }
                    },
                    UnOp::Not => { self.emit(Op::Not { dst: hint, src: r }, *span); }
                }
                Ok(hint)
            }
            Expr::Binary { op, lhs, rhs, span } => {
                self.compile_binary(*op, lhs, rhs, *span, Some(hint)).map(|(r, _)| r)
            }
            Expr::Call { name, args, span } => {
                self.compile_call(name, args, *span, Some(hint)).map(|(r, _)| r)
            }
            // These nodes produce their result in an inner register; honoured
            // by the caller's explicit Mov instead of a destination rewrite.
            _ => Ok(self.compile_expr(expr)?.0),
        }
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(Reg, Ty)> {
        match expr {
            Expr::Int(n, s) => {
                let r = self.fresh();
                self.emit(Op::ConstI64 { dst: r, val: *n }, *s);
                Ok((r, Ty::I64))
            }
            Expr::Float(f, s) => {
                let r = self.fresh();
                self.emit(Op::ConstF64 { dst: r, val: *f }, *s);
                Ok((r, Ty::F64))
            }
            Expr::Bool(b, s) => {
                let r = self.fresh();
                self.emit(Op::ConstBool { dst: r, val: *b }, *s);
                Ok((r, Ty::Bool))
            }
            Expr::Str(sval, s) => {
                let id = self.intern_str(sval);
                let r = self.fresh();
                self.emit(Op::ConstStr { dst: r, id }, *s);
                Ok((r, Ty::Str))
            }
            Expr::Var(name, span) => self.lookup(name).ok_or_else(|| {
                Error::type_error(format!("undefined variable `{}`", name), *span)
            }),
            Expr::Unary { op, expr, span } => {
                let (r, ty) = self.compile_expr(expr)?;
                let dst = self.fresh();
                match op {
                    UnOp::Neg => match ty {
                        Ty::F64 => { self.emit(Op::NegF64 { dst, src: r }, *span); }
                        _ => { self.emit(Op::NegI64 { dst, src: r }, *span); }
                    },
                    UnOp::Not => { self.emit(Op::Not { dst, src: r }, *span); }
                }
                Ok((dst, ty))
            }
            Expr::Binary { op, lhs, rhs, span } => self.compile_binary(*op, lhs, rhs, *span, None),
            Expr::Call { name, args, span } => self.compile_call(name, args, *span, None),
            Expr::IfExpr { cond, then, otherwise, span } => {
                // Reuse the statement-form if by synthesizing an ElseBranch::Block.
                let branch = ElseBranch::Block((**otherwise).clone());
                Ok(self.compile_if(cond, then, Some(&branch), *span)?)
            }
            Expr::Tuple(items, span) => {
                // `()` is the unit value (Q15): emit ConstUnit instead of an
                // empty tuple allocation, matching the checker's Ty::Unit.
                if items.is_empty() {
                    return Ok(self.emit_const_unit(*span));
                }
                if items.len() > crate::ir::MAX_OPERANDS {
                    return Err(Error::type_error(
                        format!(
                            "tuple has {} elements; the maximum is {}",
                            items.len(),
                            crate::ir::MAX_OPERANDS
                        ),
                        *span,
                    ));
                }
                let mut regs = Vec::with_capacity(items.len());
                let mut tys = Vec::with_capacity(items.len());
                for it in items {
                    let (r, t) = self.compile_expr(it)?;
                    regs.push(r);
                    tys.push(t);
                }
                let dst = self.fresh();
                self.emit(Op::MakeTuple { dst, elems: Regs::new(&regs) }, *span);
                Ok((dst, Ty::Tuple(tys)))
            }
            Expr::Index { base, index, span } => {
                let (br, bt) = self.compile_expr(base)?;
                let elem_ty = match bt {
                    Ty::Tuple(ref ts) => ts.get(*index).cloned().unwrap_or(Ty::Unit),
                    _ => Ty::Unit,
                };
                let dst = self.fresh();
                self.emit(Op::GetTuple { dst, base: br, index: *index }, *span);
                Ok((dst, elem_ty))
            }
            Expr::Array(items, span) => {
                // NewArr + per-element push: no inline operand-count limit,
                // and the empty `[]` case is the same two instructions.
                let dst = self.fresh();
                self.emit(Op::NewArr { dst }, *span);
                let mut elem_ty = Ty::Unit;
                for (i, it) in items.iter().enumerate() {
                    let (r, t) = self.compile_expr(it)?;
                    self.emit(Op::ArrPush { arr: dst, src: r }, *span);
                    if i == 0 {
                        elem_ty = t;
                    }
                }
                Ok((dst, Ty::Arr(Box::new(elem_ty))))
            }
            Expr::DynIndex { base, index, span } => {
                let (br, bt) = self.compile_expr(base)?;
                let (ir, _) = self.compile_expr(index)?;
                let elem_ty = match bt {
                    Ty::Arr(t) => (*t).clone(),
                    _ => Ty::Unit,
                };
                let dst = self.fresh();
                self.emit(Op::ArrGet { dst, base: br, idx: ir }, *span);
                Ok((dst, elem_ty))
            }
            Expr::BlockExpr(block, _) => self.compile_block(block),
        }
    }

    fn compile_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        dst_hint: Option<Reg>,
    ) -> Result<(Reg, Ty)> {
        let (lr, lt) = self.compile_expr(lhs)?;

        if op == BinOp::And || op == BinOp::Or {
            let dst = dst_hint.unwrap_or_else(|| self.fresh());
            self.emit(Op::Mov { dst, src: lr }, span);
            if op == BinOp::And {
                // if lhs false → skip rhs, dst stays false
                let skip = self.emit(Op::JumpIfFalse { cond: dst, target: usize::MAX }, span);
                let (rr, _) = self.compile_expr(rhs)?;
                self.emit(Op::Mov { dst, src: rr }, span);
                let end = self.code.len();
                self.code[skip].op = Op::JumpIfFalse { cond: dst, target: end };
            } else {
                // if lhs false → evaluate rhs; if true → skip rhs (dst stays true)
                let eval_rhs = self.emit(Op::JumpIfFalse { cond: dst, target: usize::MAX }, span);
                let skip = self.emit(Op::Jump { target: usize::MAX }, span);
                let rhs_start = self.code.len();
                self.code[eval_rhs].op = Op::JumpIfFalse { cond: dst, target: rhs_start };
                let (rr, _) = self.compile_expr(rhs)?;
                self.emit(Op::Mov { dst, src: rr }, span);
                let end = self.code.len();
                self.code[skip].op = Op::Jump { target: end };
            }
            return Ok((dst, Ty::Bool));
        }

        // Strings only support `+` (concatenation). Emitting this before the
        // numeric paths is what keeps `s1 + s2` from being lowered into an
        // integer add over heap indices — a silent-wrong-answer bug that the
        // one-pass inference order could otherwise hide.
        if matches!(lt, Ty::Str) {
            match op {
                BinOp::Add => {
                    let (rr, _) = self.compile_expr(rhs)?;
                    let dst = self.fresh();
                    self.emit(Op::Concat { dst, a: lr, b: rr }, span);
                    return Ok((dst, Ty::Str));
                }
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    return Err(Error::type_error(
                        format!("strings only support `+`, found `{}`", op_name(op)),
                        span,
                    ));
                }
                // Comparisons fall through to the typed string compare ops.
                _ => {}
            }
        }

        // `x + 1` / `x - 1` are by far the most common integer operations
        // (counters, index walks). Folding the literal into the opcode removes
        // a constant load and a register operand from the hot path.
        if matches!(op, BinOp::Add | BinOp::Sub) && !matches!(lt, Ty::F64) {
            if let Expr::Int(n, _) = rhs {
                let imm = if op == BinOp::Add { Some(*n) } else { n.checked_neg() };
                if let Some(imm) = imm {
                    let dst = dst_hint.unwrap_or_else(|| self.fresh());
                    self.emit(Op::AddI64Imm { dst, src: lr, imm }, span);
                    return Ok((dst, Ty::I64));
                }
            }
        }

        let (rr, _) = self.compile_expr(rhs)?;
        let dst = dst_hint.unwrap_or_else(|| self.fresh());
        let is_float = matches!(lt, Ty::F64);
        let opc = match op {
            BinOp::Add if is_float => Op::AddF64 { dst, a: lr, b: rr },
            BinOp::Add => Op::AddI64 { dst, a: lr, b: rr },
            BinOp::Sub if is_float => Op::SubF64 { dst, a: lr, b: rr },
            BinOp::Sub => Op::SubI64 { dst, a: lr, b: rr },
            BinOp::Mul if is_float => Op::MulF64 { dst, a: lr, b: rr },
            BinOp::Mul => Op::MulI64 { dst, a: lr, b: rr },
            BinOp::Div if is_float => Op::DivF64 { dst, a: lr, b: rr },
            BinOp::Div => Op::DivI64 { dst, a: lr, b: rr },
            BinOp::Mod if is_float => Op::ModF64 { dst, a: lr, b: rr },
            BinOp::Mod => Op::ModI64 { dst, a: lr, b: rr },
            BinOp::Eq => match lt {
                Ty::F64 => Op::EqF64 { dst, a: lr, b: rr },
                Ty::Bool => Op::EqBool { dst, a: lr, b: rr },
                Ty::Str => Op::EqStr { dst, a: lr, b: rr },
                Ty::Handle(_) => Op::EqHandle { dst, a: lr, b: rr },
                _ => Op::EqI64 { dst, a: lr, b: rr },
            },
            BinOp::NotEq => match lt {
                Ty::F64 => Op::NeF64 { dst, a: lr, b: rr },
                Ty::Bool => Op::NeBool { dst, a: lr, b: rr },
                Ty::Str => Op::NeStr { dst, a: lr, b: rr },
                Ty::Handle(_) => Op::NeHandle { dst, a: lr, b: rr },
                _ => Op::NeI64 { dst, a: lr, b: rr },
            },
            BinOp::Lt => match lt {
                Ty::F64 => Op::LtF64 { dst, a: lr, b: rr },
                Ty::Str => Op::LtStr { dst, a: lr, b: rr },
                _ => Op::LtI64 { dst, a: lr, b: rr },
            },
            BinOp::LtEq => match lt {
                Ty::F64 => Op::LeF64 { dst, a: lr, b: rr },
                Ty::Str => Op::LeStr { dst, a: lr, b: rr },
                _ => Op::LeI64 { dst, a: lr, b: rr },
            },
            BinOp::Gt => match lt {
                Ty::F64 => Op::GtF64 { dst, a: lr, b: rr },
                Ty::Str => Op::GtStr { dst, a: lr, b: rr },
                _ => Op::GtI64 { dst, a: lr, b: rr },
            },
            BinOp::GtEq => match lt {
                Ty::F64 => Op::GeF64 { dst, a: lr, b: rr },
                Ty::Str => Op::GeStr { dst, a: lr, b: rr },
                _ => Op::GeI64 { dst, a: lr, b: rr },
            },
            BinOp::And | BinOp::Or => unreachable!(),
        };
        self.emit(opc, span);

        let result_ty = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => lt,
            _ => Ty::Bool,
        };
        Ok((dst, result_ty))
    }

    fn compile_call(
        &mut self,
        name: &str,
        args: &[Expr],
        span: Span,
        dst_hint: Option<Reg>,
    ) -> Result<(Reg, Ty)> {
        // Operand lists are inline fixed arrays in the instruction encoding;
        // an over-long list is a compile error, not a panic.
        if args.len() > crate::ir::MAX_OPERANDS {
            return Err(Error::type_error(
                format!(
                    "`{}` has {} arguments; the maximum is {}",
                    name,
                    args.len(),
                    crate::ir::MAX_OPERANDS
                ),
                span,
            ));
        }
        let mut arg_regs = Vec::with_capacity(args.len());
        let mut arg_tys = Vec::with_capacity(args.len());
        for a in args {
            let (r, t) = self.compile_expr(a)?;
            arg_regs.push(r);
            arg_tys.push(t);
        }
        let regs = Regs::new(&arg_regs);
        let dst = dst_hint.unwrap_or_else(|| self.fresh());

        if let Some(bid) = builtin::id(name) {
            // Two builtins are type-specialized at the call site: `to_str`
            // stringifies whatever it is given, `abs`/`min`/`max` keep an i64
            // argument as i64 instead of widening to f64.
            let arg_ty = arg_tys.first().unwrap_or(&Ty::Unit);
            let func = if name == "to_str" {
                builtin::to_str_id(arg_ty)
            } else if let Some(id) = builtin::numeric_id(name, arg_ty) {
                id
            } else {
                bid
            };
            self.emit(Op::CallBuiltin { func, args: regs, dst }, span);
            return Ok((dst, builtin_ret(func)));
        }
        if let Some(&hid) = self.host_ids.get(name) {
            let sig = self.sigs.get(name).ok_or_else(|| {
                Error::type_error(format!("missing host signature for `{}`", name), span)
            })?;
            self.emit(Op::CallHost { func: hid, args: regs, dst }, span);
            return Ok((dst, sig.ret.clone()));
        }
        if let Some(&sid) = self.ids.get(name) {
            let sig = self.sigs.get(name).ok_or_else(|| {
                Error::type_error(format!("missing signature for `{}`", name), span)
            })?;
            self.emit(Op::CallScript { func: sid, args: regs, dst }, span);
            return Ok((dst, sig.ret.clone()));
        }
        Err(Error::type_error(format!("undefined function `{}`", name), span))
    }
}

/// Source spelling of a binary operator, for diagnostics.
fn op_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

/// The six binary operators that compare an I64 against a literal and can be
/// lowered to `JmpIf*I64Imm` in branch position.
fn is_i64_cmp(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
    )
}

/// Rewrite a comparison whose result is only consumed by a branch into the
/// fused compare-and-branch opcode.
///
/// The fused opcode encodes the *negated* predicate: the branch is a
/// "jump when false", so `a < b` becomes `JmpIfGe` (jump when `a >= b`).
/// Fusing is skipped for string/handle comparisons — those are rare in
/// conditions and keeping them unfused keeps the dispatch loop small.
fn fuse_branch(op: Op, cond: Reg, target: usize) -> Option<Op> {
    Some(match op {
        Op::LtI64 { dst, a, b } if dst == cond => Op::JmpIfGeI64 { a, b, target },
        Op::LeI64 { dst, a, b } if dst == cond => Op::JmpIfGtI64 { a, b, target },
        Op::GtI64 { dst, a, b } if dst == cond => Op::JmpIfLeI64 { a, b, target },
        Op::GeI64 { dst, a, b } if dst == cond => Op::JmpIfLtI64 { a, b, target },
        Op::EqI64 { dst, a, b } if dst == cond => Op::JmpIfNeI64 { a, b, target },
        Op::NeI64 { dst, a, b } if dst == cond => Op::JmpIfEqI64 { a, b, target },
        Op::LtF64 { dst, a, b } if dst == cond => Op::JmpIfGeF64 { a, b, target },
        Op::LeF64 { dst, a, b } if dst == cond => Op::JmpIfGtF64 { a, b, target },
        Op::GtF64 { dst, a, b } if dst == cond => Op::JmpIfLeF64 { a, b, target },
        Op::GeF64 { dst, a, b } if dst == cond => Op::JmpIfLtF64 { a, b, target },
        Op::EqF64 { dst, a, b } if dst == cond => Op::JmpIfNeF64 { a, b, target },
        Op::NeF64 { dst, a, b } if dst == cond => Op::JmpIfEqF64 { a, b, target },
        // Bool slots are canonical 0/1, so integer comparison is exact.
        Op::EqBool { dst, a, b } if dst == cond => Op::JmpIfNeI64 { a, b, target },
        Op::NeBool { dst, a, b } if dst == cond => Op::JmpIfEqI64 { a, b, target },
        _ => return None,
    })
}
