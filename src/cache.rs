//! `.sbc` bytecode cache — zero-dependency binary format.
//!
//! A cache file holds the fully compiled [`Program`], the resolved signature
//! table, the source name/hash, and a fingerprint of the host registry. Loading
//! one skips lexing, parsing, type inference and codegen entirely.
//!
//! The format is intentionally NOT a security boundary: it is a build artifact
//! produced by this crate. Still, every read is bounds-checked so a truncated
//! or corrupted file returns `Err` instead of panicking, and the host
//! fingerprint prevents the classic failure mode of cached `CallHost` slot ids
//! pointing at a different registry after the host changed.

use crate::engine::Script;
use crate::error::{Error, Result};
use crate::ir::{builtin, Instr, Op, Program, Reg, Regs, Function, FuncId, MAX_OPERANDS};
use crate::ty::Ty;
use crate::typecheck::{FuncSig, SigTable};
use crate::validate;
use std::collections::HashMap;
use std::rc::Rc;

pub const MAGIC: &[u8; 4] = b"VTBC";
/// v2 adds the `JmpIf*I64Imm` opcodes (tags 66–71), which carry both a jump
/// target and a comparison literal; the wire record grew a second `imm2` word.
/// v3 changes no wire shape: the `()` literal now compiles to `Ty::Unit`
/// (Q15) instead of an empty tuple type, so a v2 cache built for a source
/// containing `()` would serve stale semantics after a compiler upgrade — the
/// version gate forces a cheap rebuild instead.
/// v3 改动不涉及 wire 布局：`()` 字面量编译为 `Ty::Unit`（Q15），版本门
/// 强制重建防止旧缓存以 `Tuple([])` 语义存活。
/// v4 新增依赖清单段（约定式 import，ADR-017）：记录主脚本 import 的
/// 文件路径与内容 hash，`load_cached`/`compile_cached` 加载时逐一校验，
/// 任一依赖变更 → 缓存失效重建。wire 布局仅在末尾追加，VERSION 3→4。
/// ADR-018：签名类型新增 `Ty::Arr`（write_ty tag 9），VERSION 4→5 强制重建。
/// D-046 5.2：末尾追加拼接视图源码段（带依赖的脚本缓存命中后 report
/// 才有源码行/caret），VERSION 5→6 强制重建。
pub const VERSION: u16 = 6;

/// (name, slot id, params, ret), sorted by name. `CallHost` instructions embed
/// the slot id, so both the set of functions and their order must be stable.
pub type HostFingerprint = Vec<(String, u16, Vec<Ty>, Ty)>;

/// FNV-1a over the source bytes. Used only for staleness detection, never for
/// authenticity; a real deployment should keep `.sbc` files in private storage.
pub fn hash_source(src: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in src.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------- primitive I/O ----------------------------

struct Writer(Vec<u8>);

impl Writer {
    fn new() -> Self { Self(Vec::new()) }
    fn u8(&mut self, x: u8) { self.0.push(x); }
    fn u16(&mut self, x: u16) { self.0.extend_from_slice(&x.to_le_bytes()); }
    fn u32(&mut self, x: u32) { self.0.extend_from_slice(&x.to_le_bytes()); }
    fn u64(&mut self, x: u64) { self.0.extend_from_slice(&x.to_le_bytes()); }
    fn slice(&mut self, x: &[u8]) { self.0.extend_from_slice(x); }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.slice(s.as_bytes());
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self { Self { buf, pos: 0 } }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| Error::io("cache: length overflow"))?;
        if end > self.buf.len() {
            return Err(Error::io("cache: truncated file"));
        }
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> { Ok(self.take(1)?[0]) }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn str(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| Error::io("cache: invalid UTF-8 string"))
    }
}

fn write_ty(w: &mut Writer, ty: &Ty) {
    match ty {
        Ty::I64 => w.u8(1),
        Ty::F64 => w.u8(2),
        Ty::Bool => w.u8(3),
        Ty::Str => w.u8(4),
        Ty::Handle(n) => { w.u8(5); w.str(n); }
        Ty::Tuple(ts) => {
            w.u8(6);
            w.u32(ts.len() as u32);
            for t in ts { write_ty(w, t); }
        }
        Ty::Arr(t) => {
            w.u8(9);
            write_ty(w, t);
        }
        Ty::Var(id) => { w.u8(7); w.u64(*id as u64); }
        Ty::Unit => w.u8(8),
    }
}

/// 类型嵌套的解码上限：与 parser 的 `MAX_NESTING` 同值。每个嵌套层只需
/// 5 字节（tag + 长度），1 MiB 的 .sbc 能编码 20 万层——没有这条限制，
/// `read_ty` 的递归就是又一个解码期栈溢出 abort（D-046 3.3）。
fn read_ty(r: &mut Reader<'_>) -> Result<Ty> {
    read_ty_at(r, 0)
}

fn read_ty_at(r: &mut Reader<'_>, depth: usize) -> Result<Ty> {
    if depth > crate::parser::MAX_NESTING {
        return Err(Error::io("cache: type nesting too deep"));
    }
    Ok(match r.u8()? {
        1 => Ty::I64,
        2 => Ty::F64,
        3 => Ty::Bool,
        4 => Ty::Str,
        5 => Ty::Handle(r.str()?),
        6 => {
            let n = r.u32()? as usize;
            if n > r.buf.len() { return Err(Error::io("cache: implausible tuple length")); }
            let mut ts = Vec::with_capacity(n);
            for _ in 0..n { ts.push(read_ty_at(r, depth + 1)?); }
            Ty::Tuple(ts)
        }
        7 => Ty::Var(usize::try_from(r.u64()?).map_err(|_| Error::io("cache: var id overflow"))?),
        8 => Ty::Unit,
        9 => Ty::Arr(Box::new(read_ty_at(r, depth + 1)?)),
        other => return Err(Error::io(format!("cache: unknown type tag {other}"))),
    })
}

fn write_sig(w: &mut Writer, sig: &FuncSig) {
    w.u32(sig.params.len() as u32);
    for p in &sig.params { write_ty(w, p); }
    write_ty(w, &sig.ret);
}

fn read_sig(r: &mut Reader<'_>) -> Result<FuncSig> {
    let n = r.u32()? as usize;
    if n > r.buf.len() { return Err(Error::io("cache: implausible signature length")); }
    let mut params = Vec::with_capacity(n);
    for _ in 0..n { params.push(read_ty(r)?); }
    Ok(FuncSig { params, ret: read_ty(r)? })
}

// ------------------------------ instruction encoding ------------------------------
//
// Every instruction uses the SAME fixed wire layout: tag, line, col, three
// registers, one 64-bit immediate, and an optional register list. Shapes differ
// only in how fields are mapped into that layout. One table below drives both
// the encoder and decoder, so opcodes can never drift apart.

#[derive(Clone)]
struct Wire {
    tag: u16,
    line: u32,
    col: u32,
    r: [Reg; 3],
    imm: u64,
    /// Second 64-bit payload, used by `JmpIf*I64Imm` for the comparison
    /// literal while `imm` holds the jump target. Zero for all older opcodes.
    imm2: u64,
    extra: Vec<Reg>,
}

impl Wire {
    fn new(tag: u16, r: [Reg; 3], imm: u64, imm2: u64, extra: Vec<Reg>) -> Self {
        Self { tag, line: 0, col: 0, r, imm, imm2, extra }
    }
}

fn wire_target(w: &Wire) -> Result<usize> {
    usize::try_from(w.imm).map_err(|_| Error::io("cache: jump target overflows usize"))
}

/// Single source of truth for opcode tags. Add new variants at the end and bump
/// [`VERSION`]; never renumber existing tags.
///
/// Each row is: tag, variant, destructuring pattern, the three wire registers,
/// the 64-bit immediate, the extra register list, and the decoding expression.
/// One macro generates BOTH match expressions, so tags can never drift.
/// (`macro_rules!` cannot expand into a match arm, hence the whole match is
/// generated here; the `w` identifier is passed in so hygiene keeps the
/// decoding expressions and the function parameter in the same context.)
macro_rules! op_table {
    ($w:ident; $( $tag:literal, $variant:ident, $fields:tt, $regs:expr, $imm:expr, $imm2:expr, $extra:expr, $build:expr );* $(;)?) => {
        fn encode_op(op: Op, line: u32, col: u32) -> Wire {
            let mut out = match op {
                $( Op::$variant $fields => Wire::new($tag, $regs, $imm, $imm2, $extra), )*
            };
            out.line = line;
            out.col = col;
            out
        }

        fn decode_op($w: Wire) -> Result<Op> {
            Ok(match $w.tag {
                $( $tag => $build, )*
                other => return Err(Error::io(format!("cache: unknown opcode {other}"))),
            })
        }
    };
}

op_table! {
    w;
    1,  ConstI64, { dst, val }, [dst, 0, 0], val as u64, 0, Vec::new(),
        Op::ConstI64 { dst: w.r[0], val: w.imm as i64 };
    2,  ConstF64, { dst, val }, [dst, 0, 0], val.to_bits(), 0, Vec::new(),
        Op::ConstF64 { dst: w.r[0], val: f64::from_bits(w.imm) };
    3,  ConstBool, { dst, val }, [dst, 0, 0], val as u64, 0, Vec::new(),
        Op::ConstBool { dst: w.r[0], val: w.imm != 0 };
    4,  ConstStr, { dst, id }, [dst, 0, 0], id as u64, 0, Vec::new(),
        Op::ConstStr { dst: w.r[0], id: u32::try_from(w.imm).map_err(|_| Error::io("cache: string id overflow"))? };
    5,  ConstUnit, { dst }, [dst, 0, 0], 0, 0, Vec::new(),
        Op::ConstUnit { dst: w.r[0] };
    6,  Mov, { dst, src }, [dst, src, 0], 0, 0, Vec::new(),
        Op::Mov { dst: w.r[0], src: w.r[1] };
    7,  AddI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::AddI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    8,  SubI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::SubI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    9,  MulI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::MulI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    10, DivI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::DivI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    11, ModI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::ModI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    12, NegI64, { dst, src }, [dst, src, 0], 0, 0, Vec::new(),
        Op::NegI64 { dst: w.r[0], src: w.r[1] };
    13, AddI64Imm, { dst, src, imm }, [dst, src, 0], imm as u64, 0, Vec::new(),
        Op::AddI64Imm { dst: w.r[0], src: w.r[1], imm: w.imm as i64 };
    14, AddF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::AddF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    15, SubF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::SubF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    16, MulF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::MulF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    17, DivF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::DivF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    18, ModF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::ModF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    19, NegF64, { dst, src }, [dst, src, 0], 0, 0, Vec::new(),
        Op::NegF64 { dst: w.r[0], src: w.r[1] };
    20, EqI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::EqI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    21, NeI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::NeI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    22, LtI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LtI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    23, LeI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LeI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    24, GtI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GtI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    25, GeI64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GeI64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    26, EqF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::EqF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    27, NeF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::NeF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    28, LtF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LtF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    29, LeF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LeF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    30, GtF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GtF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    31, GeF64, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GeF64 { dst: w.r[0], a: w.r[1], b: w.r[2] };
    32, EqBool, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::EqBool { dst: w.r[0], a: w.r[1], b: w.r[2] };
    33, NeBool, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::NeBool { dst: w.r[0], a: w.r[1], b: w.r[2] };
    34, EqStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::EqStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    35, NeStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::NeStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    36, LtStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LtStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    37, LeStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::LeStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    38, GtStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GtStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    39, GeStr, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::GeStr { dst: w.r[0], a: w.r[1], b: w.r[2] };
    40, EqHandle, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::EqHandle { dst: w.r[0], a: w.r[1], b: w.r[2] };
    41, NeHandle, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::NeHandle { dst: w.r[0], a: w.r[1], b: w.r[2] };
    42, Not, { dst, src }, [dst, src, 0], 0, 0, Vec::new(),
        Op::Not { dst: w.r[0], src: w.r[1] };
    43, AndBool, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::AndBool { dst: w.r[0], a: w.r[1], b: w.r[2] };
    44, OrBool, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::OrBool { dst: w.r[0], a: w.r[1], b: w.r[2] };
    45, Concat, { dst, a, b }, [dst, a, b], 0, 0, Vec::new(),
        Op::Concat { dst: w.r[0], a: w.r[1], b: w.r[2] };
    46, Jump, { target }, [0, 0, 0], target as u64, 0, Vec::new(),
        Op::Jump { target: wire_target(&w)? };
    47, JumpIfFalse, { cond, target }, [cond, 0, 0], target as u64, 0, Vec::new(),
        Op::JumpIfFalse { cond: w.r[0], target: wire_target(&w)? };
    48, JmpIfEqI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfEqI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    49, JmpIfNeI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfNeI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    50, JmpIfLtI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfLtI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    51, JmpIfLeI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfLeI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    52, JmpIfGtI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfGtI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    53, JmpIfGeI64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfGeI64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    54, JmpIfEqF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfEqF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    55, JmpIfNeF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfNeF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    56, JmpIfLtF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfLtF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    57, JmpIfLeF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfLeF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    58, JmpIfGtF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfGtF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    59, JmpIfGeF64, { a, b, target }, [a, b, 0], target as u64, 0, Vec::new(),
        Op::JmpIfGeF64 { a: w.r[0], b: w.r[1], target: wire_target(&w)? };
    60, CallScript, { func, args, dst }, [dst, func, 0], 0, 0, args.iter().collect(),
        Op::CallScript { func: w.r[1], args: Regs::new(&w.extra), dst: w.r[0] };
    61, CallHost, { func, args, dst }, [dst, func, 0], 0, 0, args.iter().collect(),
        Op::CallHost { func: w.r[1], args: Regs::new(&w.extra), dst: w.r[0] };
    62, CallBuiltin, { func, args, dst }, [dst, func as u16, 0], 0, 0, args.iter().collect(),
        Op::CallBuiltin {
            func: u8::try_from(w.r[1]).map_err(|_| Error::io("cache: builtin id overflow"))?,
            args: Regs::new(&w.extra),
            dst: w.r[0],
        };
    63, MakeTuple, { dst, elems }, [dst, 0, 0], 0, 0, elems.iter().collect(),
        Op::MakeTuple { dst: w.r[0], elems: Regs::new(&w.extra) };
    64, GetTuple, { dst, base, index }, [dst, base, 0], index as u64, 0, Vec::new(),
        Op::GetTuple { dst: w.r[0], base: w.r[1], index: wire_target(&w)? };
    65, Ret, { src }, [src, 0, 0], 0, 0, Vec::new(),
        Op::Ret { src: w.r[0] };
    // v2: compare-immediate branches. `imm` carries the target (like every
    // other jump), `imm2` the comparison literal.
    66, JmpIfEqI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfEqI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    67, JmpIfNeI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfNeI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    68, JmpIfLtI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfLtI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    69, JmpIfLeI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfLeI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    70, JmpIfGtI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfGtI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    71, JmpIfGeI64Imm, { a, imm, target }, [a, 0, 0], target as u64, imm as u64, Vec::new(),
        Op::JmpIfGeI64Imm { a: w.r[0], imm: w.imm2 as i64, target: wire_target(&w)? };
    // v5: arrays (ADR-018). Appended; tags 1–71 keep their meaning.
    72, NewArr, { dst }, [dst, 0, 0], 0, 0, Vec::new(),
        Op::NewArr { dst: w.r[0] };
    73, ArrPush, { arr, src }, [arr, src, 0], 0, 0, Vec::new(),
        Op::ArrPush { arr: w.r[0], src: w.r[1] };
    74, ArrLen, { dst, src }, [dst, src, 0], 0, 0, Vec::new(),
        Op::ArrLen { dst: w.r[0], src: w.r[1] };
    75, ArrGet, { dst, base, idx }, [dst, base, idx], 0, 0, Vec::new(),
        Op::ArrGet { dst: w.r[0], base: w.r[1], idx: w.r[2] };
}

fn write_instr(w: &mut Writer, ins: &Instr) {
    let x = encode_op(ins.op, ins.line, ins.col);
    w.u16(x.tag);
    w.u32(x.line);
    w.u32(x.col);
    for r in x.r { w.u16(r); }
    w.u64(x.imm);
    w.u64(x.imm2);
    w.u8(x.extra.len() as u8);
    for r in x.extra { w.u16(r); }
}

/// Cheap structural sanity check on decoded code. Keeps a damaged cache from
/// producing out-of-bounds indexing inside the VM.
fn validate_wire(w: &Wire, nregs: u16, code_len: usize, nfuncs: usize, nstrings: usize, nhost: usize) -> Result<()> {
    let bad = |what: &str| Error::io(format!("cache: {what} out of range"));
    // For call opcodes `r[1]` holds a function id, not a register.
    let skip = matches!(w.tag, 60..=62);
    // Strict `< nregs`, matching `validate::validate`: register `nregs`
    // itself is already outside the frame window. Keeping the two gates
    // equally strict avoids a "which one caught it" question when reading a
    // rejection.
    for (i, &r) in w.r.iter().enumerate() {
        if skip && i == 1 { continue; }
        if r as usize >= nregs as usize { return Err(bad("register")); }
    }
    for &r in &w.extra {
        if r as usize >= nregs as usize { return Err(bad("register")); }
    }
    match w.tag {
        4 => {
            let id = usize::try_from(w.imm).map_err(|_| bad("string id"))?;
            if id >= nstrings { return Err(bad("string id")); }
        }
        46 | 47 | 48..=59 | 66..=71 => {
            let t = wire_target(w)?;
            // Strictly less than, matching `validate::validate`: a target
            // equal to code.len() would step past the final instruction.
            if t >= code_len { return Err(bad("jump target")); }
        }
        60 => { if w.r[1] as usize >= nfuncs { return Err(bad("script function id")); } }
        61 => { if w.r[1] as usize >= nhost { return Err(bad("host function id")); } }
        62 => {
            // `builtin::name` is the single source of truth for the id space:
            // it returns "?builtin" for anything undefined, so adding a new
            // builtin cannot silently invalidate older decoders' bounds here.
            let id = u8::try_from(w.r[1]).map_err(|_| bad("builtin id"))?;
            if builtin::name(id) == "?builtin" { return Err(bad("builtin id")); }
        }
        _ => {}
    }
    Ok(())
}

fn read_instr(r: &mut Reader<'_>, nregs: u16, code_len: usize, nfuncs: usize, nstrings: usize, nhost: usize) -> Result<Instr> {
    let tag = r.u16()?;
    let line = r.u32()?;
    let col = r.u32()?;
    let regs = [r.u16()?, r.u16()?, r.u16()?];
    let imm = r.u64()?;
    let imm2 = r.u64()?;
    let n = r.u8()? as usize;
    if n > MAX_OPERANDS { return Err(Error::io("cache: too many inline operands")); }
    let mut extra = Vec::with_capacity(n);
    for _ in 0..n { extra.push(r.u16()?); }
    let w = Wire { tag, line, col, r: regs, imm, imm2, extra };
    validate_wire(&w, nregs, code_len, nfuncs, nstrings, nhost)?;
    Ok(Instr { op: decode_op(w)?, line, col })
}

// ------------------------------ whole program ------------------------------

pub fn encode(script: &Script, fp: &HostFingerprint) -> Vec<u8> {
    let mut w = Writer::new();
    w.slice(MAGIC);
    w.u16(VERSION);
    w.u64(script.source_hash);
    w.str(&script.name);

    w.u32(fp.len() as u32);
    for (name, id, params, ret) in fp {
        w.str(name);
        w.u16(*id);
        write_sig(&mut w, &FuncSig { params: params.clone(), ret: ret.clone() });
    }

    // 确定性编码（D-046 4.7）：HashMap 遍历顺序不定，两次 encode 出的字节
    // 会不同——破坏构建产物比对与缓存 diff。按名字排序后写。
    w.u32(script.sigs.len() as u32);
    let mut sig_pairs: Vec<_> = script.sigs.iter().collect();
    sig_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, sig) in sig_pairs {
        w.str(name);
        write_sig(&mut w, sig);
    }

    w.u32(script.code.strings.len() as u32);
    for s in &script.code.strings { w.str(s); }

    w.u32(script.code.funcs.len() as u32);
    for f in &script.code.funcs {
        w.str(&f.name);
        w.u16(f.nregs);
        write_sig(&mut w, &FuncSig { params: f.params.clone(), ret: f.ret.clone() });
        w.u32(f.code.len() as u32);
        for ins in &f.code { write_instr(&mut w, ins); }
    }

    // 同上：ids 按名字排序，保证逐字节可复现
    w.u32(script.code.ids.len() as u32);
    let mut id_pairs: Vec<_> = script.code.ids.iter().collect();
    id_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, id) in id_pairs {
        w.str(name);
        w.u16(*id);
    }

    // 依赖清单（约定式 import，ADR-017）：路径（或 resolver 名字）+ 内容
    // hash + 来源标记（1 = 文件，可按路径校验时效；0 = resolver，失效由
    // 宿主自行决定）。load_cached / compile_cached 加载时逐一校验文件依赖，
    // 任一依赖变更即失效重建。
    w.u32(script.deps.len() as u32);
    for (path, hash, is_file) in &script.deps {
        w.str(path);
        w.u64(*hash);
        w.u8(u8::from(*is_file));
    }

    // 拼接视图源码段（v6，D-046 5.2）：带依赖的脚本走缓存命中后，
    // `Script::report` 的源码行 / caret 必须对得上拼接视图的行号——v5 及
    // 之前缓存里没有源码，命中后 report 只有一行消息。
    w.str(&script.source);
    w.0
}

pub fn decode(bytes: &[u8], fp: &HostFingerprint) -> Result<Script> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != MAGIC { return Err(Error::io("cache: bad magic (not an .sbc file)")); }
    let version = r.u16()?;
    if version != VERSION {
        return Err(Error::io(format!("cache: version {version} != {VERSION}; recompile")));
    }
    let source_hash = r.u64()?;
    let name = r.str()?;

    // 容量合理性（D-046 3.6）：计数上界用【剩余字节】而不是文件总长——
    // 旧检查 `n > bytes.len()` 让 1 MiB 文件可声明百万条记录，在读取任何
    // 一条之前 with_capacity 就分配 ~100× 放大。每条记录至少 8 字节
    // （最短的是 id 表：len(4)+1 字符+u16），按此封顶预分配；超出部分
    // 由逐条读取的截断检查兜底。
    fn plausible_cap(n: usize, bytes: &[u8], pos: usize) -> usize {
        let remain = bytes.len().saturating_sub(pos);
        n.min(remain / 8)
    }

    let n_fp = r.u32()? as usize;
    if n_fp > bytes.len() { return Err(Error::io("cache: implausible registry size")); }
    let mut cached_fp = Vec::with_capacity(plausible_cap(n_fp, bytes, r.pos));
    for _ in 0..n_fp {
        let n = r.str()?;
        let id = r.u16()?;
        let sig = read_sig(&mut r)?;
        cached_fp.push((n, id, sig.params, sig.ret));
    }
    if &cached_fp != fp {
        return Err(Error::io("cache: host registry changed since the cache was built; recompile"));
    }
    let nhost = fp.len();

    let n_sigs = r.u32()? as usize;
    if n_sigs > bytes.len() { return Err(Error::io("cache: implausible signature table size")); }
    let mut sigs = SigTable::with_capacity(plausible_cap(n_sigs, bytes, r.pos));
    for _ in 0..n_sigs {
        let n = r.str()?;
        sigs.insert(n, read_sig(&mut r)?);
    }

    let n_strings = r.u32()? as usize;
    if n_strings > bytes.len() { return Err(Error::io("cache: implausible string pool size")); }
    let mut strings: Vec<Rc<str>> = Vec::with_capacity(plausible_cap(n_strings, bytes, r.pos));
    for _ in 0..n_strings { strings.push(Rc::from(r.str()?.as_str())); }

    let n_funcs = r.u32()? as usize;
    if n_funcs > bytes.len() { return Err(Error::io("cache: implausible function count")); }
    let mut funcs: Vec<Function> = Vec::with_capacity(plausible_cap(n_funcs, bytes, r.pos));
    for _ in 0..n_funcs {
        let fname = r.str()?;
        let nregs = r.u16()?;
        let sig = read_sig(&mut r)?;
        let n_code = r.u32()? as usize;
        if n_code > bytes.len() { return Err(Error::io("cache: implausible code length")); }
        let mut code = Vec::with_capacity(plausible_cap(n_code, bytes, r.pos));
        for _ in 0..n_code {
            code.push(read_instr(&mut r, nregs, n_code, n_funcs, n_strings, nhost)?);
        }
        funcs.push(Function { name: fname, code, nregs, params: sig.params, ret: sig.ret });
    }

    let n_ids = r.u32()? as usize;
    if n_ids > bytes.len() { return Err(Error::io("cache: implausible id table size")); }
    let mut ids: HashMap<String, FuncId> = HashMap::with_capacity(plausible_cap(n_ids, bytes, r.pos));
    for _ in 0..n_ids {
        let n = r.str()?;
        ids.insert(n, r.u16()?);
    }

    // 依赖清单（v4，约定式 import）：路径（或 resolver 名字）+ 内容 hash +
    // 来源标记。硬校验：依赖路径禁 '..' 组件与控制字符——恶意构造的 .sbc
    // 不能借依赖校验（deps_fresh 按路径读文件）做任意路径探测（D-046 3.7）。
    // 注意依赖路径存的是 resolve 后的真实路径（可以是绝对路径），所以这里
    // 不能禁绝对路径——包含性收敛在 deps_fresh 里按 import_dir 做。
    let n_deps = r.u32()? as usize;
    if n_deps > bytes.len() { return Err(Error::io("cache: implausible dependency count")); }
    let mut deps = Vec::with_capacity(plausible_cap(n_deps, bytes, r.pos));
    for _ in 0..n_deps {
        let path = r.str()?;
        let hash = r.u64()?;
        let is_file = r.u8()?;
        if is_file > 1 { return Err(Error::io("cache: invalid dependency flag")); }
        if path.is_empty() || path.chars().any(|c| c.is_control()) {
            return Err(Error::io("cache: dependency path contains control characters"));
        }
        if path.split(['/', '\\']).any(|c| c == "..") {
            return Err(Error::io("cache: dependency path contains '..'"));
        }
        deps.push((path, hash, is_file == 1));
    }

    // 拼接视图源码段（v6）：见 encode 侧注释。`r.str()` 自带长度头与
    // 越界检查，无需再带独立的长度字段。
    let source: Rc<str> = Rc::from(r.str()?.as_str());

    if r.pos != bytes.len() {
        return Err(Error::io("cache: trailing bytes"));
    }

    // ---- 语义校验（D-046 5.7）：结构合法 ≠ 语义一致 ----
    // 1) 签名里不允许出现推断变量：typecheck 承诺 Var 不会逃逸，解码出的
    //    Var 会让 `type_matches` 的 `(_, Ty::Var(_)) => true` 放行任意值，
    //    函数体却按 I64 位解释——静默错值。
    fn assert_no_var(ty: &Ty) -> Result<()> {
        match ty {
            Ty::Var(_) => Err(Error::io("cache: signature contains an inference variable")),
            Ty::Tuple(ts) => ts.iter().try_for_each(assert_no_var),
            Ty::Arr(t) => assert_no_var(t),
            _ => Ok(()),
        }
    }
    for sig in sigs.values() {
        sig.params.iter().try_for_each(assert_no_var)?;
        assert_no_var(&sig.ret)?;
    }
    // 2) sigs ↔ funcs 一致：`Engine::call` 用 sigs 做 coerce，VM 用 funcs
    //    做位解释——两者独立可篡改，必须逐项相等。
    for f in &funcs {
        if let Some(sig) = sigs.get(&f.name) {
            if sig.params != f.params || sig.ret != f.ret {
                return Err(Error::io(format!(
                    "cache: signature mismatch for `{}` between sig table and function body",
                    f.name
                )));
            }
        }
    }
    // 3) ids 必须是名字 → 函数的双射：入口名映射到任意函数（同 arity）
    //    会静默调错代码。
    if ids.len() != funcs.len() {
        return Err(Error::io("cache: id table does not cover every function"));
    }
    for (n, &id) in &ids {
        match funcs.get(id as usize) {
            Some(f) if f.name == *n => {}
            _ => return Err(Error::io("cache: id table maps a name to a foreign function")),
        }
    }

    let program = Program { funcs, ids, strings };
    validate::validate(&program, nhost)?;

    Ok(Script {
        code: program,
        sigs,
        name,
        source_hash,
        source,
        deps,
        host_fp: fp.clone(),
    })
}
