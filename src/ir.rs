//! Stack-based bytecode IR for the register VM.
//!
//! The defining property of this IR is **type specialization**: the type
//! checker's resolved signatures are threaded through codegen so every
//! arithmetic/comparison instruction records the operand type. The VM
//! dispatches straight to a typed operation with no runtime tag inspection.
//!
//! `Instr` is `Copy`: register lists are fixed-size inline arrays. This lets
//! the VM's dispatch loop read an instruction by value, so no borrow of the
//! frame stack is held while mutating registers or the heap.

use crate::span::Span;
use crate::ty::Ty;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A register index into a frame's unboxed slot array.
pub type Reg = u16;
/// Function id resolved at compile time (script / host namespaces).
pub type FuncId = u16;
/// Max arguments/operands an instruction can carry inline.
pub const MAX_OPERANDS: usize = 8;

/// Inline, `Copy` register list. Avoids per-instruction heap allocation in the
/// VM hot loop while keeping `Op` a plain value type.
#[derive(Debug, Clone, Copy)]
pub struct Regs {
    items: [Reg; MAX_OPERANDS],
    len: u8,
}

impl Regs {
    pub fn new(slice: &[Reg]) -> Self {
        assert!(slice.len() <= MAX_OPERANDS, "too many operands");
        let mut items = [0; MAX_OPERANDS];
        items[..slice.len()].copy_from_slice(slice);
        Self {
            items,
            len: slice.len() as u8,
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, i: usize) -> Reg {
        self.items[i]
    }

    pub fn iter(&self) -> impl Iterator<Item = Reg> + '_ {
        self.items[..self.len as usize].iter().copied()
    }
}

/// A fully resolved bytecode instruction. `Copy` is deliberate (see module doc).
///
/// The location is stored as two `u32`s rather than a full `Span` (4 × usize =
/// 32 bytes): the dispatch loop copies the whole instruction every step, so
/// keeping it small is a direct throughput win. Only line/column are needed for
/// runtime diagnostics (overflow, div-by-zero, bad index).
#[derive(Debug, Clone, Copy)]
pub struct Instr {
    pub op: Op,
    pub line: u32,
    pub col: u32,
}

impl Instr {
    pub fn new(op: Op, span: Span) -> Self {
        Self {
            op,
            line: span.line as u32,
            col: span.col as u32,
        }
    }

    /// Rebuild a `Span` for error reporting at runtime.
    pub fn span(&self) -> Span {
        Span::new(0, 0, self.line as usize, self.col as usize)
    }
}

/// The complete instruction set. Arithmetic and comparison come in typed
/// variants selected by codegen from `SigTable` — that is where the speedup
/// over the boxed AST interpreter comes from.
#[derive(Debug, Clone, Copy)]
pub enum Op {
    // ---- constants & moves ----
    ConstI64 {
        dst: Reg,
        val: i64,
    },
    ConstF64 {
        dst: Reg,
        val: f64,
    },
    ConstBool {
        dst: Reg,
        val: bool,
    },
    /// Load a pre-interned string constant (no allocation).
    ConstStr {
        dst: Reg,
        id: u32,
    },
    ConstUnit {
        dst: Reg,
    },
    /// Copy a slot (8 bytes, `Copy`). No Rc bump, no clone.
    Mov {
        dst: Reg,
        src: Reg,
    },

    // ---- integer arithmetic (checked) ----
    AddI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    SubI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    MulI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    DivI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    ModI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NegI64 {
        dst: Reg,
        src: Reg,
    },
    /// `dst = src + imm` (checked). Loop counters use this so an iteration costs
    /// one instruction instead of `const 1; add; mov`.
    AddI64Imm {
        dst: Reg,
        src: Reg,
        imm: i64,
    },

    // ---- float arithmetic ----
    AddF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    SubF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    MulF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    DivF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    ModF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NegF64 {
        dst: Reg,
        src: Reg,
    },

    // ---- comparisons (produce a bool slot) ----
    EqI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NeI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LtI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LeI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GtI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GeI64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    EqF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NeF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LtF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LeF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GtF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GeF64 {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    EqBool {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NeBool {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    EqStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NeStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LtStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    LeStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GtStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    GeStr {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    /// Reference equality for opaque handles: true when both operands carry
    /// the same host object (`Rc` identity), even though each argument or
    /// host-call result occupies its own heap slot.
    EqHandle {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    NeHandle {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    // ---- logic ----
    Not {
        dst: Reg,
        src: Reg,
    },
    AndBool {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    OrBool {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// String concatenation (allocates a new string on the VM heap).
    Concat {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    // ---- control flow (absolute instruction indices) ----
    Jump {
        target: usize,
    },
    /// Jump when the bool slot is false.
    JumpIfFalse {
        cond: Reg,
        target: usize,
    },

    /// Fused compare-and-branch: jump to `target` when `a OP b` holds.
    ///
    /// Codegen emits these for `if`/`while`/`for` conditions, where the bool
    /// result would only ever be consumed by the branch. Fusing drops one
    /// dispatch (and one register write) per condition evaluation — the
    /// dominant per-iteration overhead in loops.
    ///
    /// Note each variant means "jump when the predicate is TRUE", so the
    /// negation of a source-level comparison maps to the inverse opcode.
    JmpIfEqI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfNeI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfLtI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfLeI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfGtI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfGeI64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfEqF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfNeF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfLtF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfLeF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfGtF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },
    JmpIfGeF64 {
        a: Reg,
        b: Reg,
        target: usize,
    },

    /// Fused compare-immediate branch: jump to `target` when `a OP imm`
    /// holds. Codegen emits these when a condition is a comparison against an
    /// integer literal (e.g. `n < 2`), so the literal never needs a `ConstI64`
    /// instruction or a register of its own. Same negation convention as the
    /// register-register fused branches.
    JmpIfEqI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },
    JmpIfNeI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },
    JmpIfLtI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },
    JmpIfLeI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },
    JmpIfGtI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },
    JmpIfGeI64Imm {
        a: Reg,
        imm: i64,
        target: usize,
    },

    // ---- calls ----
    CallScript {
        func: FuncId,
        args: Regs,
        dst: Reg,
    },
    /// Host signature (params) is looked up by `func` id in the registry at
    /// runtime to reconstruct typed `Value`s — keeps this instruction Copy.
    CallHost {
        func: FuncId,
        args: Regs,
        dst: Reg,
    },
    /// `func` is a builtin id (see `builtin`); string conversion variants are
    /// type-specialized so no runtime type tag is needed.
    CallBuiltin {
        func: u8,
        args: Regs,
        dst: Reg,
    },
    /// Structural `contains` with the needle's **static** element type.
    ///
    /// `Slot` is an untagged `u64`: comparing two unequal scalars by reading
    /// them as heap indices is a silent wrong answer (and, with self-similar
    /// arrays, unbounded recursion). Codegen knows the element type, so it is
    /// recorded as an index into `Program::types` (D-047).
    ContainsTyped {
        hay: Reg,
        needle: Reg,
        dst: Reg,
        ty: u32,
    },

    // ---- tuples ----
    MakeTuple {
        dst: Reg,
        elems: Regs,
    },
    GetTuple {
        dst: Reg,
        base: Reg,
        index: usize,
    },

    // ---- arrays (ADR-018; reference semantics via shared heap entries) ----
    /// Allocate an empty array heap entry.
    NewArr {
        dst: Reg,
    },
    /// Append `src` to the array in `arr` (in-place heap mutation; quota is
    /// charged per element). Used for literal construction; scripts call the
    /// `push` builtin, which routes through the same heap helper.
    ArrPush {
        arr: Reg,
        src: Reg,
    },
    /// Element count into `dst` (loop bounds use it fused with `JmpIfGeI64`).
    ArrLen {
        dst: Reg,
        src: Reg,
    },
    /// `dst = arr[idx]`, bounds-checked.
    ArrGet {
        dst: Reg,
        base: Reg,
        idx: Reg,
    },

    // ---- return ----
    Ret {
        src: Reg,
    },
}

/// A compiled function body plus its calling metadata.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub code: Vec<Instr>,
    /// Number of registers in a frame for this function (params + locals + temps).
    pub nregs: u16,
    /// Resolved signature (all `Var`s defaulted by the type checker).
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Owns the constant pool shared by all functions in a compiled program.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub funcs: Vec<Function>,
    /// name -> function id, covering script functions only.
    pub ids: HashMap<String, FuncId>,
    pub strings: Vec<Rc<str>>,
    /// Deduplicated static types referenced by `ContainsTyped`. Keeping the
    /// type in a side table keeps `Instr` `Copy` (types own `Vec`/`String`).
    pub types: Vec<Ty>,
}

impl Program {
    pub fn get(&self, id: FuncId) -> Option<&Function> {
        self.funcs.get(id as usize)
    }

    pub fn id_of(&self, name: &str) -> Option<FuncId> {
        self.ids.get(name).copied()
    }
}

/// Stable ids for built-ins. Order must match `VM::invoke_builtin`.
pub mod builtin {
    pub const ABS: u8 = 0;
    pub const SQRT: u8 = 1;
    pub const FLOOR: u8 = 2;
    pub const CEIL: u8 = 3;
    pub const POW: u8 = 4;
    pub const MIN: u8 = 5;
    pub const MAX: u8 = 6;
    pub const LEN: u8 = 7;
    pub const CONTAINS: u8 = 8;
    pub const SUBSTR: u8 = 9;
    pub const TO_I64: u8 = 10;
    pub const TO_F64: u8 = 11;
    pub const TO_STR_F64: u8 = 12;
    pub const TO_STR_I64: u8 = 13;
    pub const TO_STR_BOOL: u8 = 14;
    pub const TO_STR_STR: u8 = 15;
    // `abs`/`min`/`max` have both an f64 and an i64 form. The 0/5/6 ids above
    // stay the f64 variants so existing `.sbc` files keep their meaning; the
    // integer variants are appended rather than renumbered.
    pub const ABS_I64: u8 = 16;
    pub const MIN_I64: u8 = 17;
    pub const MAX_I64: u8 = 18;
    /// `push(arr, v)` — the one side-effecting builtin (ADR-018).
    pub const PUSH: u8 = 19;

    /// Choose the type-specialized string conversion opcode.
    pub fn to_str_id(ty: &crate::ty::Ty) -> u8 {
        match ty {
            crate::ty::Ty::F64 => TO_STR_F64,
            crate::ty::Ty::Bool => TO_STR_BOOL,
            crate::ty::Ty::Str => TO_STR_STR,
            _ => TO_STR_I64,
        }
    }

    /// Type-specialized id for the builtins that accept `i64` as well as `f64`.
    /// Returns `None` for every other name, so codegen can use this as a
    /// refinement step over [`id`]. `abs`/`min`/`max` never mix the two types
    /// (the checker rejects that), which is why one argument decides the form.
    pub fn numeric_id(name: &str, ty: &crate::ty::Ty) -> Option<u8> {
        let int = matches!(ty, crate::ty::Ty::I64);
        Some(match (name, int) {
            ("abs", true) => ABS_I64,
            ("min", true) => MIN_I64,
            ("max", true) => MAX_I64,
            ("abs", false) => ABS,
            ("min", false) => MIN,
            ("max", false) => MAX,
            _ => return None,
        })
    }

    pub fn id(name: &str) -> Option<u8> {
        Some(match name {
            "abs" => ABS,
            "sqrt" => SQRT,
            "floor" => FLOOR,
            "ceil" => CEIL,
            "pow" => POW,
            "min" => MIN,
            "max" => MAX,
            "len" => LEN,
            "contains" => CONTAINS,
            "substr" => SUBSTR,
            "to_i64" => TO_I64,
            "to_f64" => TO_F64,
            "to_str" => TO_STR_I64, // refined by codegen via to_str_id
            "push" => PUSH,
            _ => return None,
        })
    }

    pub fn name(id: u8) -> &'static str {
        match id {
            ABS => "abs",
            SQRT => "sqrt",
            FLOOR => "floor",
            CEIL => "ceil",
            POW => "pow",
            MIN => "min",
            MAX => "max",
            LEN => "len",
            CONTAINS => "contains",
            SUBSTR => "substr",
            TO_I64 => "to_i64",
            TO_F64 => "to_f64",
            TO_STR_F64 | TO_STR_I64 | TO_STR_BOOL | TO_STR_STR => "to_str",
            ABS_I64 => "abs",
            MIN_I64 => "min",
            MAX_I64 => "max",
            PUSH => "push",
            _ => "?builtin",
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
