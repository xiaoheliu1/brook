//! ②: bytecode structure is validated once when a program is built, not per VM
//! invocation. Handcrafted `Program`s exercise the branches codegen never
//! emits (a corrupt `.sbc` can still contain them).

use brook::ir::{builtin, Function, Instr, Op, Program, Regs};
use brook::span::Span;
use brook::ty::Ty;
use brook::validate::validate;
use brook::vm::Vm;
use brook::Engine;
use std::collections::HashMap;
use std::rc::Rc;

fn program(op: Op, nregs: u16) -> Program {
    Program {
        funcs: vec![Function {
            name: "f".into(),
            code: vec![Instr::new(op, Span::synthetic())],
            nregs,
            params: vec![],
            ret: Ty::Unit,
        }],
        ids: HashMap::new(),
        strings: vec![Rc::from("x")],
        types: Vec::new(),
    }
}

fn rejected(op: Op, nregs: u16, nhosts: usize) -> bool {
    validate(&program(op, nregs), nhosts).is_err()
}

#[test]
fn registers_and_targets_are_checked() -> brook::Result<()> {
    assert!(rejected(Op::Mov { dst: 2, src: 0 }, 2, 0));
    assert!(rejected(Op::Mov { dst: 0, src: 2 }, 2, 0));
    assert!(validate(&program(Op::Mov { dst: 1, src: 0 }, 2), 0).is_ok());

    // Strictly `< code.len()` — a target equal to the length is rejected.
    assert!(rejected(Op::Jump { target: 1 }, 1, 0));
    assert!(validate(&program(Op::Jump { target: 0 }, 1), 0).is_ok());
    assert!(rejected(Op::JumpIfFalse { cond: 2, target: 0 }, 2, 0));
    assert!(rejected(
        Op::JmpIfGeI64 {
            a: 0,
            b: 1,
            target: 1
        },
        2,
        0
    ));
    Ok(())
}

#[test]
fn ids_and_pools_are_checked() -> brook::Result<()> {
    let no_args = Regs::new(&[]);
    assert!(rejected(
        Op::CallScript {
            func: 1,
            args: no_args,
            dst: 0
        },
        1,
        0
    ));
    assert!(rejected(
        Op::CallHost {
            func: 1,
            args: no_args,
            dst: 0
        },
        1,
        1
    ));
    assert!(validate(
        &program(
            Op::CallHost {
                func: 0,
                args: no_args,
                dst: 0
            },
            1
        ),
        1
    )
    .is_ok());
    assert!(rejected(
        Op::CallBuiltin {
            func: 200,
            args: Regs::new(&[0]),
            dst: 0
        },
        1,
        0
    ));
    assert!(validate(
        &program(
            Op::CallBuiltin {
                func: builtin::LEN,
                args: Regs::new(&[0]),
                dst: 0
            },
            1
        ),
        0
    )
    .is_ok());
    assert!(rejected(Op::ConstStr { dst: 0, id: 1 }, 1, 0));
    assert!(rejected(
        Op::MakeTuple {
            dst: 0,
            elems: Regs::new(&[2])
        },
        2,
        0
    ));
    assert!(rejected(
        Op::GetTuple {
            dst: 0,
            base: 2,
            index: 0
        },
        2,
        0
    ));

    let mut p = program(Op::Ret { src: 0 }, 1);
    p.ids.insert("ghost".into(), 9);
    assert!(validate(&p, 0).is_err());
    Ok(())
}

#[test]
fn builtin_arities_are_checked() -> brook::Result<()> {
    for (func, arity) in [
        (builtin::LEN, 1),
        (builtin::POW, 2),
        (builtin::SUBSTR, 3),
        (builtin::TO_STR_F64, 1),
        (builtin::TO_STR_I64, 1),
        (builtin::TO_STR_BOOL, 1),
        (builtin::TO_STR_STR, 1),
    ] {
        let wrong = if arity == 1 {
            Regs::new(&[])
        } else {
            Regs::new(&[0])
        };
        assert!(rejected(
            Op::CallBuiltin {
                func,
                args: wrong,
                dst: 0,
            },
            1,
            0
        ));
        let right = Regs::new(&vec![0; arity]);
        assert!(validate(
            &program(
                Op::CallBuiltin {
                    func,
                    args: right,
                    dst: 0,
                },
                1
            ),
            0
        )
        .is_ok());
    }
    Ok(())
}

#[test]
fn public_vm_rejects_invalid_program_before_execution() {
    let program = program(Op::ConstI64 { dst: 0, val: 1 }, 0);
    let host = brook::engine::HostRegistry::new();
    let err = match Vm::new(&program, &host, 1_000, 64, 4 << 20) {
        Ok(_) => panic!("invalid program must be rejected"),
        Err(err) => err,
    };
    assert!(
        err.message.contains("dst register 0 >= nregs 0"),
        "{}",
        err.message
    );
}

#[test]
fn validated_code_rejects_registry_count_mismatch() {
    let mut program = program(Op::ConstUnit { dst: 0 }, 1);
    program.ids.insert("f".into(), 0);
    let script = brook::Script {
        sigs: HashMap::from([(
            "f".to_string(),
            brook::typecheck::FuncSig {
                params: vec![],
                ret: Ty::Unit,
            },
        )]),
        code: brook::ValidatedCode::new(program, 1).unwrap(),
        name: "mismatch".into(),
        source_hash: 0,
        source: Rc::from(""),
        deps: Vec::new(),
        host_fp: Vec::new(),
    };
    let err = Engine::new().call(&script, "f", vec![]).unwrap_err();
    assert!(
        err.message.contains("expects 1 host slots, registry has 0"),
        "{}",
        err.message
    );
}

#[test]
fn compiler_output_always_passes() -> brook::Result<()> {
    let engine = Engine::new();
    let script = engine.load_str("func f(s) { let t = len(s)\n t + 1 }")?;
    // `compile` itself runs the validator; a successful load is the assertion.
    assert_eq!(
        engine.call(&script, "f", vec![brook::Value::Str("abc".into())])?,
        brook::Value::I64(4)
    );
    Ok(())
}

#[test]
fn call_script_args_stay_inside_the_callee_window() -> brook::Result<()> {
    // Structural bound only: argv is copied into the callee's window
    // `[nbase, nbase + nregs)`, so more args than the window panicked on the
    // copy (caught only by `catch_unwind`). Semantic arity (args ==
    // callee.params.len()) belongs to the type checker and is deliberately
    // NOT checked here — codec round-trip programs legitimately violate it.
    let callee = Function {
        name: "callee".into(),
        code: vec![
            Instr::new(Op::ConstI64 { dst: 0, val: 7 }, Span::synthetic()),
            Instr::new(Op::Ret { src: 0 }, Span::synthetic()),
        ],
        nregs: 1,
        params: vec![Ty::I64],
        ret: Ty::I64,
    };
    let caller_with = |args: &[u16]| Function {
        name: "caller".into(),
        code: vec![
            Instr::new(
                Op::CallScript {
                    func: 1,
                    args: Regs::new(args),
                    dst: 0,
                },
                Span::synthetic(),
            ),
            Instr::new(Op::Ret { src: 0 }, Span::synthetic()),
        ],
        nregs: 2,
        params: vec![],
        ret: Ty::Unit,
    };
    let p = |caller: Function| Program {
        funcs: vec![caller, callee.clone()],
        ids: HashMap::new(),
        strings: vec![Rc::from("x")],
        types: Vec::new(),
    };

    // 2 args into a 1-register window: rejected.
    let err = validate(&p(caller_with(&[0, 0])), 0).unwrap_err();
    assert!(
        err.message
            .contains("receives 2 args but its register window holds 1"),
        "{}",
        err.message
    );

    // Fewer args than the callee declares is a semantic concern, not a
    // structural one: validate accepts it (the callee would read defaults).
    validate(&p(caller_with(&[0])), 0)?;
    Ok(())
}
