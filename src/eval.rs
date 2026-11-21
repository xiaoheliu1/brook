//! AST interpreter. Executes a type-checked program.
//!
//! Integer overflow and division by zero terminate with a runtime error
//! (no wrap, no panic) — see PROJECT_PLAN.md §3.6.

use crate::ast::*;
use crate::engine::HostRegistry;
use crate::error::{Error, Result};
use crate::span::Span;
use crate::ty::Ty;
use crate::typecheck::SigTable;
use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Interpreter<'a> {
    funcs: &'a HashMap<String, FuncDecl>,
    host: &'a HostRegistry,
    sigs: &'a SigTable,
    scopes: Vec<HashMap<String, Value>>,
    steps: u64,
    max_steps: u64,
    depth: usize,
    max_depth: usize,
}

/// How a statement completes: normally with a value, or by transferring
/// control (`return`, and the loop-local `break` / `continue`).
enum Flow {
    Normal(Value),
    Return(Value),
    /// Leaves the innermost loop.
    Break,
    /// Skips to the innermost loop's next iteration.
    Continue,
}

impl<'a> Interpreter<'a> {
    pub fn new(
        funcs: &'a HashMap<String, FuncDecl>,
        host: &'a HostRegistry,
        sigs: &'a SigTable,
        max_steps: u64,
        max_depth: usize,
    ) -> Self {
        Self {
            funcs,
            host,
            sigs,
            scopes: Vec::new(),
            steps: 0,
            max_steps,
            depth: 0,
            max_depth,
        }
    }

    pub fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Value> {
        // Coerce args to the function's declared parameter types when needed.
        let sig = self.sigs.get(name).ok_or_else(|| {
            Error::runtime(format!("unknown function `{}`", name))
        })?;
        if sig.params.len() != args.len() {
            return Err(Error::runtime(format!(
                "`{}` expects {} args, got {}",
                name,
                sig.params.len(),
                args.len()
            )));
        }
        let coerced: Vec<Value> = args
            .into_iter()
            .zip(sig.params.iter())
            .map(|(v, t)| coerce(v, t))
            .collect::<Result<Vec<_>>>()?;

        let func = self.funcs.get(name).ok_or_else(|| {
            Error::runtime(format!("`{}` is not a script function", name))
        })?;
        self.invoke_script(func, coerced)
    }

    fn invoke_script(&mut self, func: &FuncDecl, args: Vec<Value>) -> Result<Value> {
        if self.depth >= self.max_depth {
            return Err(Error::runtime_at("call depth limit exceeded", func.span));
        }
        self.depth += 1;
        self.scopes.push(HashMap::new());
        for (p, v) in func.params.iter().zip(args.into_iter()) {
            self.scopes.last_mut().unwrap().insert(p.name.clone(), v);
        }
        let result = match self.exec_block(&func.body)? {
            Flow::Normal(v) | Flow::Return(v) => Ok(v),
            Flow::Break | Flow::Continue => Err(Error::runtime_at(
                "`break` / `continue` outside of a loop",
                func.span,
            )),
        };
        self.scopes.pop();
        self.depth -= 1;
        result
    }

    fn tick(&mut self, span: Span) -> Result<()> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(Error::runtime_at("step (gas) limit exceeded", span));
        }
        Ok(())
    }

    // ---- statements ----

    fn exec_block(&mut self, block: &Block) -> Result<Flow> {
        self.scopes.push(HashMap::new());
        let mut last = Value::Unit;
        let mut flow = Flow::Normal(Value::Unit);
        for stmt in &block.stmts {
            match self.exec_stmt(stmt)? {
                Flow::Normal(v) => last = v,
                other => {
                    flow = other;
                    break;
                }
            }
        }
        self.scopes.pop();
        match flow {
            Flow::Normal(_) => Ok(Flow::Normal(last)),
            other => Ok(other),
        }
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Flow> {
        self.tick(stmt_span(stmt))?;
        match stmt {
            Stmt::Let { name, init, .. } => {
                let v = self.eval_expr(init)?;
                self.scopes.last_mut().unwrap().insert(name.clone(), v);
                Ok(Flow::Normal(Value::Unit))
            }
            Stmt::Assign { name, value, .. } => {
                let v = self.eval_expr(value)?;
                // Variables are stored in the innermost scope that has them.
                let mut updated = false;
                for scope in self.scopes.iter_mut().rev() {
                    if scope.contains_key(name) {
                        scope.insert(name.clone(), v);
                        updated = true;
                        break;
                    }
                }
                if !updated {
                    return Err(Error::runtime(format!("undefined variable `{}`", name)));
                }
                Ok(Flow::Normal(Value::Unit))
            }
            Stmt::Expr(e) => Ok(Flow::Normal(self.eval_expr(e)?)),
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If { cond, then, otherwise, .. } => {
                if self.eval_expr(cond)?.as_bool()? {
                    self.exec_block(then)
                } else {
                    match otherwise.as_deref() {
                        None => Ok(Flow::Normal(Value::Unit)),
                        Some(ElseBranch::Block(b)) => self.exec_block(b),
                        Some(ElseBranch::ElseIf(s)) => self.exec_stmt(s),
                    }
                }
            }
            Stmt::While { cond, body, .. } => {
                loop {
                    self.tick(body.span)?;
                    if !self.eval_expr(cond)?.as_bool()? {
                        break;
                    }
                    match self.exec_block(body)? {
                        Flow::Normal(_) | Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Normal(Value::Unit))
            }
            Stmt::For { var, start, end, body, .. } => {
                let s = self.eval_expr(start)?.as_i64()?;
                let e = self.eval_expr(end)?.as_i64()?;
                let mut i = s;
                while i < e {
                    self.tick(body.span)?;
                    self.scopes.push(HashMap::new());
                    self.scopes.last_mut().unwrap().insert(var.clone(), Value::I64(i));
                    let flow = self.exec_block(body);
                    self.scopes.pop();
                    match flow? {
                        Flow::Normal(_) | Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                    i = i.checked_add(1).ok_or_else(|| Error::runtime("for loop counter overflow"))?;
                }
                Ok(Flow::Normal(Value::Unit))
            }
            Stmt::Break { .. } => Ok(Flow::Break),
            Stmt::Continue { .. } => Ok(Flow::Continue),
        }
    }

    // ---- expressions ----

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value> {
        self.tick(expr_span(expr))?;
        match expr {
            Expr::Int(n, _) => Ok(Value::I64(*n)),
            Expr::Float(f, _) => Ok(Value::F64(*f)),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Str(s, _) => Ok(Value::Str(Rc::from(s.as_str()))),
            Expr::Var(name, span) => self
                .lookup(name)
                .ok_or_else(|| Error::runtime_at(format!("undefined variable `{}`", name), *span)),
            Expr::Call { name, args, span } => self.eval_call(name, args, *span),
            Expr::Unary { op, expr, .. } => {
                let v = self.eval_expr(expr)?;
                match op {
                    UnOp::Neg => match v {
                        Value::I64(n) => n
                            .checked_neg()
                            .map(Value::I64)
                            .ok_or_else(|| Error::runtime("i64 negation overflow")),
                        Value::F64(f) => Ok(Value::F64(-f)),
                        _ => Err(Error::runtime("unary `-` requires a number")),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.as_bool()?)),
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let l = self.eval_expr(lhs)?;
                // Short-circuit logical operators.
                match op {
                    BinOp::And => {
                        if !l.as_bool()? {
                            return Ok(Value::Bool(false));
                        }
                        return Ok(Value::Bool(self.eval_expr(rhs)?.as_bool()?));
                    }
                    BinOp::Or => {
                        if l.as_bool()? {
                            return Ok(Value::Bool(true));
                        }
                        return Ok(Value::Bool(self.eval_expr(rhs)?.as_bool()?));
                    }
                    _ => {}
                }
                let r = self.eval_expr(rhs)?;
                self.eval_binary(*op, l, r, *span)
            }
            Expr::IfExpr { cond, then, otherwise, span } => {
                if self.eval_expr(cond)?.as_bool()? {
                    flow_to_value(self.exec_block(then)?, *span)
                } else {
                    flow_to_value(self.exec_block(otherwise)?, *span)
                }
            }
            Expr::Tuple(items, _) => {
                // `()` is the unit value (Q15), matching the VM and checker.
                if items.is_empty() {
                    return Ok(Value::Unit);
                }
                let mut vs = Vec::new();
                for it in items {
                    vs.push(self.eval_expr(it)?);
                }
                Ok(Value::Tuple(vs))
            }
            Expr::Index { base, index, span } => {
                let b = self.eval_expr(base)?;
                match b {
                    Value::Tuple(vs) => vs.get(*index).cloned().ok_or_else(|| {
                        Error::runtime_at(format!("tuple index {} out of range", index), *span)
                    }),
                    _ => Err(Error::runtime_at("cannot index non-tuple", *span)),
                }
            }
            Expr::BlockExpr(b, span) => flow_to_value(self.exec_block(b)?, *span),
        }
    }

    fn eval_call(&mut self, name: &str, args: &[Expr], span: Span) -> Result<Value> {
        let mut vals = Vec::new();
        for a in args {
            vals.push(self.eval_expr(a)?);
        }

        // Built-ins first.
        if let Some(v) = eval_builtin(name, &vals)? {
            return Ok(v);
        }

        // Host functions.
        if self.host.contains(name) {
            return self.host.invoke(name, &vals).map_err(|e| {
                // Keep the host's own error kind, mirroring the VM's
                // CallHost: an IO failure raised by a host function is still
                // an IO failure to the caller.
                Error::new(
                    e.kind,
                    format!("host function `{}`: {}", name, e.message),
                    Some(span),
                )
            });
        }

        // Script functions.
        if self.funcs.contains_key(name) {
            return self.call(name, vals);
        }

        Err(Error::runtime_at(format!("undefined function `{}`", name), span))
    }

    fn eval_binary(&self, op: BinOp, l: Value, r: Value, span: Span) -> Result<Value> {
        use BinOp::*;
        // Integer path
        if let (Value::I64(a), Value::I64(b)) = (&l, &r) {
            return match op {
                Add => a.checked_add(*b).map(Value::I64).ok_or_else(|| Error::runtime_at("i64 add overflow", span)),
                Sub => a.checked_sub(*b).map(Value::I64).ok_or_else(|| Error::runtime_at("i64 sub overflow", span)),
                Mul => a.checked_mul(*b).map(Value::I64).ok_or_else(|| Error::runtime_at("i64 mul overflow", span)),
                Div => {
                    if *b == 0 { return Err(Error::runtime_at("division by zero", span)); }
                    a.checked_div(*b).map(Value::I64).ok_or_else(|| Error::runtime_at("i64 div overflow", span))
                }
                Mod => {
                    if *b == 0 { return Err(Error::runtime_at("modulo by zero", span)); }
                    a.checked_rem(*b).map(Value::I64).ok_or_else(|| Error::runtime_at("i64 mod overflow", span))
                }
                Eq => Ok(Value::Bool(a == b)),
                NotEq => Ok(Value::Bool(a != b)),
                Lt => Ok(Value::Bool(a < b)),
                LtEq => Ok(Value::Bool(a <= b)),
                Gt => Ok(Value::Bool(a > b)),
                GtEq => Ok(Value::Bool(a >= b)),
                And | Or => Err(Error::runtime_at("logic operators need bool", span)),
            };
        }
        // Bool equality — mirrors the VM's EqBool / NeBool. Ordering on bool
        // is a compile error; this arm only fixes what used to fall into the
        // float path below with a misleading "expected f64" message.
        if let (Value::Bool(a), Value::Bool(b)) = (&l, &r) {
            return match op {
                Eq => Ok(Value::Bool(a == b)),
                NotEq => Ok(Value::Bool(a != b)),
                _ => Err(Error::runtime_at("ordering requires numbers or str, found bool", span)),
            };
        }
        // Handle identity — same semantics as the VM's EqHandle: object
        // identity via `Handle`'s PartialEq (`Rc` pointer + type name).
        if let (Value::Handle(a), Value::Handle(b)) = (&l, &r) {
            return match op {
                Eq => Ok(Value::Bool(a == b)),
                NotEq => Ok(Value::Bool(a != b)),
                _ => Err(Error::runtime_at(
                    format!("ordering requires numbers or str, found {}", a.type_name),
                    span,
                )),
            };
        }
        // String path — `+` concatenates (语言规格 §3.2) and the six
        // comparisons match the VM's typed string ops. Everything here used
        // to error through the float path, so the reference interpreter
        // disagreed with the VM on legal scripts.
        if let (Value::Str(a), Value::Str(b)) = (&l, &r) {
            return match op {
                Add => Ok(Value::Str(Rc::from(format!("{}{}", a, b)))),
                Eq => Ok(Value::Bool(a == b)),
                NotEq => Ok(Value::Bool(a != b)),
                Lt => Ok(Value::Bool(a < b)),
                LtEq => Ok(Value::Bool(a <= b)),
                Gt => Ok(Value::Bool(a > b)),
                GtEq => Ok(Value::Bool(a >= b)),
                _ => Err(Error::runtime_at("strings only support `+`", span)),
            };
        }
        // Float path (also accepts I64 promoted to F64 by the checker's coercion).
        let a = l.as_f64()?;
        let b = r.as_f64()?;
        match op {
            Add => Ok(Value::F64(a + b)),
            Sub => Ok(Value::F64(a - b)),
            Mul => Ok(Value::F64(a * b)),
            Div => {
                if b == 0.0 { return Err(Error::runtime_at("division by zero", span)); }
                Ok(Value::F64(a / b))
            }
            Mod => {
                if b == 0.0 { return Err(Error::runtime_at("modulo by zero", span)); }
                Ok(Value::F64(a % b))
            }
            Eq => Ok(Value::Bool(a == b)),
            NotEq => Ok(Value::Bool(a != b)),
            Lt => Ok(Value::Bool(a < b)),
            LtEq => Ok(Value::Bool(a <= b)),
            Gt => Ok(Value::Bool(a > b)),
            GtEq => Ok(Value::Bool(a >= b)),
            And | Or => Err(Error::runtime_at("logic operators need bool", span)),
        }
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        None
    }
}

fn coerce(v: Value, target: &Ty) -> Result<Value> {
    match (v, target) {
        (Value::I64(n), Ty::F64) => Ok(Value::F64(n as f64)),
        (v, _) => Ok(v),
    }
}

/// Evaluate a built-in. Returns `Ok(None)` if the name is not a built-in.
fn eval_builtin(name: &str, args: &[Value]) -> Result<Option<Value>> {
    let unary_f64 = |v: &Value| -> Result<f64> { v.as_f64() };
    let res = match name {
        // `abs`/`min`/`max` keep an i64 argument as i64 (Q7); the VM has a
        // matching specialization, so both engines agree on the result type.
        "abs" if args.len() == 1 => match &args[0] {
            Value::I64(n) => Value::I64(
                n.checked_abs()
                    .ok_or_else(|| Error::runtime("i64 abs overflow"))?,
            ),
            v => Value::F64(v.as_f64()?.abs()),
        },
        "sqrt" if args.len() == 1 => Value::F64(unary_f64(&args[0])?.sqrt()),
        "floor" if args.len() == 1 => Value::F64(unary_f64(&args[0])?.floor()),
        "ceil" if args.len() == 1 => Value::F64(unary_f64(&args[0])?.ceil()),
        "len" if args.len() == 1 => Value::I64(args[0].as_str()?.chars().count() as i64),
        "contains" if args.len() == 2 => {
            Value::Bool(args[0].as_str()?.contains(args[1].as_str()?))
        }
        "substr" if args.len() == 3 => {
            let s = args[0].as_str()?;
            let start = args[1].as_i64()?.max(0) as usize;
            let len = args[2].as_i64()?.max(0) as usize;
            let chars: Vec<char> = s.chars().collect();
            let end = (start + len).min(chars.len());
            let start = start.min(chars.len());
            Value::Str(Rc::from(chars[start..end].iter().collect::<String>()))
        }
        "to_i64" if args.len() == 1 => Value::I64(unary_f64(&args[0])? as i64),
        "to_f64" if args.len() == 1 => Value::F64(unary_f64(&args[0])?),
        "to_str" if args.len() == 1 => Value::Str(Rc::from(args[0].to_string())),
        "pow" if args.len() == 2 => Value::F64(args[0].as_f64()?.powf(args[1].as_f64()?)),
        "min" if args.len() == 2 => match (&args[0], &args[1]) {
            (Value::I64(a), Value::I64(b)) => Value::I64(*a.min(b)),
            (a, b) => Value::F64(a.as_f64()?.min(b.as_f64()?)),
        },
        "max" if args.len() == 2 => match (&args[0], &args[1]) {
            (Value::I64(a), Value::I64(b)) => Value::I64(*a.max(b)),
            (a, b) => Value::F64(a.as_f64()?.max(b.as_f64()?)),
        },
        _ => return Ok(None),
    };
    Ok(Some(res))
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
        | Expr::Index { span: s, .. }
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

/// Collapse a statement block's completion into the value it yields.
///
/// `eval_expr` returns a plain `Value`, so it cannot carry a control-flow
/// transfer out of an expression. The bytecode VM does support `break` /
/// `continue` in expression position (e.g. `let x = if c { break } else { 1 }`);
/// this reference interpreter reports that clearly instead of inventing a value.
fn flow_to_value(flow: Flow, span: Span) -> Result<Value> {
    match flow {
        Flow::Normal(v) | Flow::Return(v) => Ok(v),
        Flow::Break | Flow::Continue => Err(Error::runtime_at(
            "`break` / `continue` in expression position is not supported by the reference interpreter",
            span,
        )),
    }
}
