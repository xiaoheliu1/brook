//! Structural validation of compiled bytecode.
//!
//! `Vm::run` indexes registers and jumps by absolute indices. Those indices
//! are produced by codegen or decoded from a `.sbc` file; validating them once
//! at program construction keeps the dispatch loop free of defensive checks
//! without turning a corrupt cache into undefined behaviour.
//!
//! This runs in `Engine::compile` and `cache::decode`, never in `Vm::new` —
//! a `Vm` is created per `Engine::call`, so an O(code) pass there would land on
//! the execution hot path.

use crate::error::{Error, Result};
use crate::ir::{builtin, FuncId, Op, Program, Reg, Regs, MAX_OPERANDS};

/// Reject a program with out-of-range registers, jump targets, function ids,
/// string ids or oversized operand lists.
pub fn validate(program: &Program, nhosts: usize) -> Result<()> {
    for f in &program.funcs {
        let nregs = f.nregs as usize;
        let code_len = f.code.len();
        if code_len == 0 {
            return Err(Error::io(format!("validate: `{}` has no instructions", f.name)));
        }

        let bad = |what: String| Error::io(format!("validate: `{}`: {}", f.name, what));
        let reg = |r: Reg, role: &str| -> Result<()> {
            if (r as usize) < nregs {
                Ok(())
            } else {
                Err(bad(format!("{role} register {} >= nregs {}", r, nregs)))
            }
        };
        let target = |t: usize| -> Result<()> {
            // Strictly less: a target equal to code.len() would step past the
            // final instruction, which the VM treats as a runtime error today.
            if t < code_len {
                Ok(())
            } else {
                Err(bad(format!("jump target {} >= code.len() {}", t, code_len)))
            }
        };
        let regs_ok = |v: &Regs, role: &str| -> Result<()> {
            if v.len() > MAX_OPERANDS {
                return Err(bad(format!("{role} has {} operands (max {})", v.len(), MAX_OPERANDS)));
            }
            for i in 0..v.len() {
                reg(v.get(i), role)?;
            }
            Ok(())
        };

        for (i, ins) in f.code.iter().enumerate() {
            match ins.op {
                Op::ConstI64 { dst, .. }
                | Op::ConstF64 { dst, .. }
                | Op::ConstBool { dst, .. }
                | Op::ConstUnit { dst }
                | Op::Ret { src: dst } => reg(dst, "dst")?,

                Op::ConstStr { dst, id } => {
                    reg(dst, "dst")?;
                    if id as usize >= program.strings.len() {
                        return Err(bad(format!("ins {i}: string id {} out of range", id)));
                    }
                }

                Op::Mov { dst, src }
                | Op::NegI64 { dst, src }
                | Op::NegF64 { dst, src }
                | Op::Not { dst, src } => {
                    reg(dst, "dst")?;
                    reg(src, "src")?;
                }

                Op::AddI64 { dst, a, b }
                | Op::SubI64 { dst, a, b }
                | Op::MulI64 { dst, a, b }
                | Op::DivI64 { dst, a, b }
                | Op::ModI64 { dst, a, b }
                | Op::AddF64 { dst, a, b }
                | Op::SubF64 { dst, a, b }
                | Op::MulF64 { dst, a, b }
                | Op::DivF64 { dst, a, b }
                | Op::ModF64 { dst, a, b }
                | Op::EqI64 { dst, a, b }
                | Op::NeI64 { dst, a, b }
                | Op::LtI64 { dst, a, b }
                | Op::LeI64 { dst, a, b }
                | Op::GtI64 { dst, a, b }
                | Op::GeI64 { dst, a, b }
                | Op::EqF64 { dst, a, b }
                | Op::NeF64 { dst, a, b }
                | Op::LtF64 { dst, a, b }
                | Op::LeF64 { dst, a, b }
                | Op::GtF64 { dst, a, b }
                | Op::GeF64 { dst, a, b }
                | Op::EqBool { dst, a, b }
                | Op::NeBool { dst, a, b }
                | Op::EqStr { dst, a, b }
                | Op::NeStr { dst, a, b }
                | Op::LtStr { dst, a, b }
                | Op::LeStr { dst, a, b }
                | Op::GtStr { dst, a, b }
                | Op::GeStr { dst, a, b }
                | Op::EqHandle { dst, a, b }
                | Op::NeHandle { dst, a, b }
                | Op::AndBool { dst, a, b }
                | Op::OrBool { dst, a, b }
                | Op::Concat { dst, a, b } => {
                    reg(dst, "dst")?;
                    reg(a, "a")?;
                    reg(b, "b")?;
                }

                Op::AddI64Imm { dst, src, .. } => {
                    reg(dst, "dst")?;
                    reg(src, "src")?;
                }

                Op::Jump { target: t } => target(t)?,
                Op::JumpIfFalse { cond, target: t } => {
                    reg(cond, "cond")?;
                    target(t)?;
                }

                Op::JmpIfEqI64 { a, b, target: t }
                | Op::JmpIfNeI64 { a, b, target: t }
                | Op::JmpIfLtI64 { a, b, target: t }
                | Op::JmpIfLeI64 { a, b, target: t }
                | Op::JmpIfGtI64 { a, b, target: t }
                | Op::JmpIfGeI64 { a, b, target: t }
                | Op::JmpIfEqF64 { a, b, target: t }
                | Op::JmpIfNeF64 { a, b, target: t }
                | Op::JmpIfLtF64 { a, b, target: t }
                | Op::JmpIfLeF64 { a, b, target: t }
                | Op::JmpIfGtF64 { a, b, target: t }
                |                 Op::JmpIfGeF64 { a, b, target: t } => {
                    reg(a, "a")?;
                    reg(b, "b")?;
                    target(t)?;
                }

                Op::JmpIfEqI64Imm { a, target: t, .. }
                | Op::JmpIfNeI64Imm { a, target: t, .. }
                | Op::JmpIfLtI64Imm { a, target: t, .. }
                | Op::JmpIfLeI64Imm { a, target: t, .. }
                | Op::JmpIfGtI64Imm { a, target: t, .. }
                | Op::JmpIfGeI64Imm { a, target: t, .. } => {
                    reg(a, "a")?;
                    target(t)?;
                }

                Op::CallScript { func, args, dst } => {
                    if func as usize >= program.funcs.len() {
                        return Err(bad(format!("ins {i}: script function id {} out of range", func)));
                    }
                    // Structural safety bound only: the caller copies argv
                    // into the callee's window `[nbase, nbase + nregs)`, so
                    // more args than the window panics on the copy (caught
                    // only by `catch_unwind`). Matching `params.len()` is a
                    // *semantic* invariant owned by the type checker — codec
                    // round-trip programs legitimately violate it — so it is
                    // deliberately not checked here.
                    let callee = &program.funcs[func as usize];
                    if args.len() > callee.nregs as usize {
                        return Err(bad(format!(
                            "ins {i}: `{}` receives {} args but its register window holds {}",
                            callee.name,
                            args.len(),
                            callee.nregs
                        )));
                    }
                    regs_ok(&args, "args")?;
                    reg(dst, "dst")?;
                }
                Op::CallHost { func, args, dst } => {
                    if func as usize >= nhosts {
                        return Err(bad(format!("ins {i}: host function id {} out of range ({nhosts} registered)", func)));
                    }
                    regs_ok(&args, "args")?;
                    reg(dst, "dst")?;
                }
                Op::CallBuiltin { func, args, dst } => {
                    if builtin::name(func) == "?builtin" {
                        return Err(bad(format!("ins {i}: unknown builtin id {}", func)));
                    }
                    regs_ok(&args, "args")?;
                    reg(dst, "dst")?;
                }

                Op::MakeTuple { dst, elems } => {
                    reg(dst, "dst")?;
                    regs_ok(&elems, "elems")?;
                }
                Op::GetTuple { dst, base, .. } => {
                    reg(dst, "dst")?;
                    reg(base, "base")?;
                }

                Op::NewArr { dst } => reg(dst, "dst")?,
                Op::ArrPush { arr, src } => {
                    reg(arr, "arr")?;
                    reg(src, "src")?;
                }
                Op::ArrLen { dst, src } => {
                    reg(dst, "dst")?;
                    reg(src, "src")?;
                }
                Op::ArrGet { dst, base, idx } => {
                    reg(dst, "dst")?;
                    reg(base, "base")?;
                    reg(idx, "idx")?;
                }
            }
        }
    }

    for (name, &id) in &program.ids {
        let id: FuncId = id;
        if id as usize >= program.funcs.len() {
            return Err(Error::io(format!(
                "validate: name `{name}` maps to function id {} out of range",
                id
            )));
        }
    }
    Ok(())
}
