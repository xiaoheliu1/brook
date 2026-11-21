//! Register VM: executes typed bytecode with unboxed 8-byte slots.
//!
//! Design targets (see PROJECT_PLAN.md, phase 2):
//!   * No runtime type tags on the arithmetic path — `Op` variants are typed.
//!   * No string-keyed scope lookup — variables are compile-time registers.
//!   * No `Value` clones on the hot path — a `Slot` is a `Copy` u64; heap
//!     objects (str / handle / tuple) are referenced by index.
//!   * Explicit frame stack — recursion is bounded by `max_depth`, no host
//!     stack growth from interpreted calls.
//!
//! Layout: registers live in **one flat `Vec<Slot>`**. Frame `f` owns the slice
//! `[frames[f].base .. frames[f].base + nregs]`, so a register access is a
//! single indexed load — no per-frame `Vec` indirection, no `frames.last()`
//! lookup, and frames are allocated by bumping an offset instead of allocating.
//! The dispatch loop keeps `ip`, `base` and the current code slice in local
//! variables and only writes them back to the frame on calls/returns.

use crate::engine::HostRegistry;
use crate::error::{Error, Result, TraceFrame};
use crate::ir::{self, FuncId, Instr, Op, Program, Reg, Regs};
use crate::span::Span;
use crate::value::{Handle, Value};
use std::rc::Rc;

/// An 8-byte slot. Scalar values are stored inline by bit pattern; heap
/// objects store an index into `Vm.heap`. The instruction opcode tells the VM
/// how to interpret the bits, so no tag byte is required.
#[derive(Clone, Copy)]
struct Slot(u64);

impl Slot {
    #[inline(always)]
    fn i64(v: i64) -> Self {
        Slot(v as u64)
    }
    #[inline(always)]
    fn f64(v: f64) -> Self {
        Slot(v.to_bits())
    }
    #[inline(always)]
    fn as_i64(self) -> i64 {
        self.0 as i64
    }
    #[inline(always)]
    fn as_f64(self) -> f64 {
        f64::from_bits(self.0)
    }
    #[inline(always)]
    fn as_bool(self) -> bool {
        self.0 != 0
    }
    #[inline(always)]
    fn heap(idx: usize) -> Self {
        Slot(idx as u64)
    }
    #[inline(always)]
    fn as_heap(self) -> usize {
        self.0 as usize
    }
}

/// Heap-resident values. Strings from the constant pool are pre-interned;
/// runtime strings (substr/to_str) and tuples are appended lazily.
enum HeapObj {
    Str(Rc<str>),
    Handle(Handle),
    Tuple(Vec<Slot>),
}

/// Every heap entry occupies a slot in `Vm::heap` even when its payload is
/// empty. Charging only payload bytes let scripts bypass `max_heap` with
/// repeated empty strings or tuples.
const HEAP_OBJ_OVERHEAD: u64 = std::mem::size_of::<HeapObj>() as u64;

/// Flat register file backing every frame window.
///
/// Indexing is the hottest operation in the dispatch loop, so `Index`/`IndexMut`
/// skip the bounds check. Safety relies on two invariants established outside
/// this type:
///   * `validate` (run at program construction) guarantees every `Reg` in an
///     instruction is `< nregs` for the function that owns it;
///   * `Vm::call` / `CallScript` resize the file so `base + nregs <= len()` for
///     the active frame before its first instruction executes.
/// A `debug_assert` makes any violation of those invariants loud in debug
/// builds; in release it compiles away to a plain unchecked load/store.
struct RegFile(Vec<Slot>);

impl std::ops::Index<usize> for RegFile {
    type Output = Slot;
    #[inline(always)]
    fn index(&self, i: usize) -> &Slot {
        debug_assert!(i < self.0.len(), "register index out of range");
        unsafe { self.0.get_unchecked(i) }
    }
}

impl std::ops::IndexMut<usize> for RegFile {
    #[inline(always)]
    fn index_mut(&mut self, i: usize) -> &mut Slot {
        debug_assert!(i < self.0.len(), "register index out of range");
        unsafe { self.0.get_unchecked_mut(i) }
    }
}

impl std::ops::Deref for RegFile {
    type Target = Vec<Slot>;
    #[inline(always)]
    fn deref(&self) -> &Vec<Slot> {
        &self.0
    }
}

impl std::ops::DerefMut for RegFile {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Vec<Slot> {
        &mut self.0
    }
}

/// A call frame: which function, where its register window starts, and where to
/// resume the caller.
struct Frame {
    func: FuncId,
    /// Start index of this frame's register window in `Vm::regs`.
    base: usize,
    /// Resume offset in the caller's code (meaningful once we return *to* it).
    ip: usize,
    /// Caller register where this frame's return value is deposited.
    ret_to: Reg,
    /// Size of this frame's register window (used to bump the window on calls).
    nregs: usize,
}

/// The virtual machine. One instance per top-level `Engine::call`.
pub struct Vm<'a> {
    program: &'a Program,
    host: &'a HostRegistry,
    heap: Vec<HeapObj>,
    /// Flat register file shared by all frames (see module docs).
    /// Bounds checks are elided here; see [`RegFile`] for the invariants.
    regs: RegFile,
    frames: Vec<Frame>,
    max_steps: u64,
    max_depth: usize,
    /// Bytes allocated so far by script code (strings + tuple payloads).
    heap_bytes: u64,
    /// Hard cap; allocation beyond it aborts the script instead of the process.
    max_heap: u64,
}

impl<'a> Vm<'a> {
    pub fn new(
        program: &'a Program,
        host: &'a HostRegistry,
        max_steps: u64,
        max_depth: usize,
        max_heap: u64,
    ) -> Self {
        // Pre-intern all string constants so ConstStr never allocates.
        let heap = program.strings.iter().map(|s| HeapObj::Str(Rc::clone(s))).collect();
        Self {
            program,
            host,
            heap,
            regs: RegFile(Vec::with_capacity(64)),
            frames: Vec::with_capacity(16),
            max_steps,
            max_depth,
            heap_bytes: 0,
            max_heap,
        }
    }

    /// Account for a heap entry, aborting the script if the quota is exceeded.
    /// Checked *before* the allocation so a hostile script cannot OOM the host.
    /// Each entry pays a fixed overhead in addition to its payload, so a
    /// zero-length string or tuple still consumes quota.
    #[inline]
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.heap_bytes = self
            .heap_bytes
            .saturating_add(bytes as u64)
            .saturating_add(HEAP_OBJ_OVERHEAD);
        if self.heap_bytes > self.max_heap {
            return Err(Error::runtime(format!(
                "memory limit exceeded ({} of {} bytes)",
                self.heap_bytes, self.max_heap
            )));
        }
        Ok(())
    }

    /// Execute `func` with already-coerced argument values.
    ///
    /// The gas budget applies to this one call: a fresh `Vm` per `call` keeps
    /// `max_steps` a per-invocation limit.
    pub fn call(&mut self, func: FuncId, args: Vec<Value>) -> Result<Value> {
        let f = self
            .program
            .get(func)
            .ok_or_else(|| Error::runtime("unknown script function"))?;
        if f.params.len() != args.len() {
            return Err(Error::runtime(format!(
                "`{}` expects {} args, got {}",
                f.name,
                f.params.len(),
                args.len()
            )));
        }
        let nregs = f.nregs as usize;
        let ret_ty = f.ret.clone();

        self.regs.clear();
        self.regs.resize(nregs, Slot(0));
        for (i, (v, ty)) in args.into_iter().zip(f.params.iter()).enumerate() {
            let slot = self.value_to_slot(v, ty)?;
            self.regs[i] = slot;
        }
        self.frames.push(Frame { func, base: 0, ip: 0, ret_to: 0, nregs });

        let result = match self.run() {
            Ok(slot) => slot,
            // The frame stack is still intact here: `run` returns before any
            // unwinding. Walking it costs nothing on the successful path,
            // which is why the trace is not maintained eagerly.
            Err(e) => {
                let trace = self.capture_trace(&e);
                return Err(e.with_trace(trace));
            }
        };
        self.slot_to_value(result, &ret_ty)
    }

    /// Rebuild the interpreted call stack for a failed run, outermost first.
    ///
    /// The innermost frame reports the faulting instruction's own location
    /// (it is the error's span). Every caller frame still holds the resume
    /// offset it was given by `Op::CallScript`, and that offset points *past*
    /// the call, so `code[ip - 1]` is exactly the call site.
    fn capture_trace(&self, fault: &Error) -> Vec<TraceFrame> {
        let last = self.frames.len().saturating_sub(1);
        let mut trace = Vec::with_capacity(self.frames.len());
        for (idx, f) in self.frames.iter().enumerate() {
            let func = match self.program.get(f.func) {
                Some(fun) => fun.name.clone(),
                None => continue,
            };
            let loc = if idx == last {
                fault.span.map(|s| (s.line, s.col))
            } else {
                // `ip` is the already-incremented resume offset.
                let code = &self.program.funcs[f.func as usize].code;
                f.ip.checked_sub(1).and_then(|i| code.get(i)).map(|i| (i.line as usize, i.col as usize))
            };
            let (line, col) = loc.unwrap_or((0, 0));
            trace.push(TraceFrame { func, line, col });
        }
        trace
    }

    /// The dispatch loop. Frames are pushed/popped by CallScript/Ret.
    fn run(&mut self) -> Result<Slot> {
        // `program` is a shared reference with lifetime `'a`; copying it out of
        // `self` means the code slice below does not borrow the VM, so the loop
        // body can freely mutate registers, the heap and the frame stack.
        let program: &'a Program = self.program;
        let max_steps = self.max_steps;
        let max_depth = self.max_depth;

        let mut fi = self.frames.len() - 1;
        let mut base = self.frames[fi].base;
        let mut ip = self.frames[fi].ip;
        let mut code: &'a [Instr] = program.funcs[self.frames[fi].func as usize].code.as_slice();
        let mut steps: u64 = 0;

        loop {
            // One bounds-checked fetch per step; the instruction is read by
            // reference (`Op` is `Copy`), so no 56-byte struct copy.
            let instr = match code.get(ip) {
                Some(i) => i,
                None => return Err(Error::runtime("instruction pointer out of range")),
            };
            ip += 1;

            steps += 1;
            if steps > max_steps {
                return Err(err_at("step (gas) limit exceeded", instr.span()));
            }

            match instr.op {
                // ---- constants & moves ----
                Op::ConstI64 { dst, val } => self.regs[base + dst as usize] = Slot::i64(val),
                Op::ConstF64 { dst, val } => self.regs[base + dst as usize] = Slot::f64(val),
                Op::ConstBool { dst, val } => {
                    self.regs[base + dst as usize] = Slot(if val { 1 } else { 0 })
                }
                Op::ConstStr { dst, id } => self.regs[base + dst as usize] = Slot::heap(id as usize),
                Op::ConstUnit { dst } => self.regs[base + dst as usize] = Slot(0),
                Op::Mov { dst, src } => {
                    let v = self.regs[base + src as usize];
                    self.regs[base + dst as usize] = v;
                }

                // ---- integer arithmetic ----
                Op::AddI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x.checked_add(y).ok_or_else(|| err_at("i64 add overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::SubI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x.checked_sub(y).ok_or_else(|| err_at("i64 sub overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::MulI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x.checked_mul(y).ok_or_else(|| err_at("i64 mul overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::DivI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    if y == 0 {
                        return Err(err_at("division by zero", instr.span()));
                    }
                    let r = x.checked_div(y).ok_or_else(|| err_at("i64 div overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::ModI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    if y == 0 {
                        return Err(err_at("modulo by zero", instr.span()));
                    }
                    let r = x.checked_rem(y).ok_or_else(|| err_at("i64 mod overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::NegI64 { dst, src } => {
                    let x = self.regs[base + src as usize].as_i64();
                    let r = x.checked_neg().ok_or_else(|| err_at("i64 negation overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::AddI64Imm { dst, src, imm } => {
                    let x = self.regs[base + src as usize].as_i64();
                    let r = x.checked_add(imm).ok_or_else(|| err_at("i64 add overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }

                // ---- float arithmetic ----
                Op::AddF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() + self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::SubF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() - self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::MulF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() * self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::DivF64 { dst, a, b } => {
                    let y = self.regs[base + b as usize].as_f64();
                    if y == 0.0 {
                        return Err(err_at("division by zero", instr.span()));
                    }
                    let r = self.regs[base + a as usize].as_f64() / y;
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::ModF64 { dst, a, b } => {
                    let y = self.regs[base + b as usize].as_f64();
                    if y == 0.0 {
                        return Err(err_at("modulo by zero", instr.span()));
                    }
                    let r = self.regs[base + a as usize].as_f64() % y;
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::NegF64 { dst, src } => {
                    let r = -self.regs[base + src as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }

                // ---- integer comparisons ----
                Op::EqI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() == self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() != self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LtI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() < self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() <= self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GtI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() > self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64() >= self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- float comparisons ----
                Op::EqF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() == self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() != self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LtF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() < self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() <= self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GtF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() > self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64() >= self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- bool comparisons ----
                Op::EqBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool() == self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool() != self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- string comparisons ----
                Op::EqStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x == y)?,
                Op::NeStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x != y)?,
                Op::LtStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x < y)?,
                Op::LeStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x <= y)?,
                Op::GtStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x > y)?,
                Op::GeStr { dst, a, b } => self.cmp_str(base, dst, a, b, instr.span(), |x, y| x >= y)?,

                // ---- handle identity ----
                // Object identity, not heap-slot identity: every argument and
                // every host-call result gets its own heap entry, so the same
                // host object arriving through two slots must still compare
                // equal.
                Op::EqHandle { dst, a, b } => {
                    let r = self.handle_eq(base, a, b, instr.span())?;
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeHandle { dst, a, b } => {
                    let r = !self.handle_eq(base, a, b, instr.span())?;
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- logic ----
                Op::Not { dst, src } => {
                    let r = !self.regs[base + src as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::AndBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool() && self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::OrBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool() || self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::Concat { dst, a, b } => {
                    // Build the string before charging so the borrow of `heap`
                    // ends before the mutable allocation below.
                    let joined = {
                        let x = self.heap_str(self.regs[base + a as usize].as_heap(), instr.span())?;
                        let y = self.heap_str(self.regs[base + b as usize].as_heap(), instr.span())?;
                        let mut s = String::with_capacity(x.len() + y.len());
                        s.push_str(x);
                        s.push_str(y);
                        s
                    };
                    let slot = self.intern_runtime_str(joined)?;
                    self.regs[base + dst as usize] = slot;
                }

                // ---- control flow ----
                Op::Jump { target } => {
                    ip = target
                }
                Op::JumpIfFalse { cond, target } => {
                    if !self.regs[base + cond as usize].as_bool() {
                        ip = target;
                    }
                }

                // ---- fused compare-and-branch ----
                Op::JmpIfEqI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() == self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfNeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() != self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfLtI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() < self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfLeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() <= self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfGtI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() > self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfGeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() >= self.regs[base + b as usize].as_i64() {
                        ip = target;
                    }
                }
                Op::JmpIfEqF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() == self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                Op::JmpIfNeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() != self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                Op::JmpIfLtF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() < self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                Op::JmpIfLeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() <= self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                Op::JmpIfGtF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() > self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                Op::JmpIfGeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() >= self.regs[base + b as usize].as_f64() {
                        ip = target;
                    }
                }
                // ---- fused compare-immediate branch (one register read) ----
                Op::JmpIfEqI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() == imm { ip = target; }
                }
                Op::JmpIfNeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() != imm { ip = target; }
                }
                Op::JmpIfLtI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() < imm { ip = target; }
                }
                Op::JmpIfLeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() <= imm { ip = target; }
                }
                Op::JmpIfGtI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() > imm { ip = target; }
                }
                Op::JmpIfGeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() >= imm { ip = target; }
                }

                // ---- calls ----
                Op::CallScript { func, args, dst } => {
                    if self.frames.len() >= max_depth {
                        return Err(err_at("call depth limit exceeded", instr.span()));
                    }
                    let nf = program
                        .get(func)
                        .ok_or_else(|| err_at("unknown script function", instr.span()))?;
                    let n = args.len();
                    let nregs = nf.nregs as usize;
                    let nbase = base + self.frames[fi].nregs;

                    // Copy arguments out before growing `regs` (which may move).
                    let mut argv = [Slot(0); ir::MAX_OPERANDS];
                    for i in 0..n {
                        argv[i] = self.regs[base + args.get(i) as usize];
                    }
                    if self.regs.len() < nbase + nregs {
                        self.regs.resize(nbase + nregs, Slot(0));
                    }
                    for i in 0..n {
                        self.regs[nbase + i] = argv[i];
                    }

                    self.frames[fi].ip = ip;
                    let ncode = nf.code.as_slice();
                    self.frames.push(Frame { func, base: nbase, ip: 0, ret_to: dst, nregs });
                    fi += 1;
                    base = nbase;
                    ip = 0;
                    code = ncode;
                }
                Op::CallHost { func, args, dst } => {
                    let sig = self.host.sig_by_id(func);
                    let vals = self.gather_values(base, &args, &sig.params)?;
                    let result = self
                        .host
                        .invoke_by_id(func, &vals)
                        // Keep the host's own error kind instead of flattening
                        // every failure into `Runtime`: an IO failure raised by
                        // a host function is still an IO failure to the caller.
                        .map_err(|e| {
                            Error::new(
                                e.kind,
                                format!("host function: {}", e.message),
                                Some(instr.span()),
                            )
                        })?;
                    let ret_ty = sig.ret.clone();
                    let slot = self.value_to_slot(result, &ret_ty)?;
                    self.regs[base + dst as usize] = slot;
                }
                Op::CallBuiltin { func, args, dst } => {
                    let slot = self.invoke_builtin(base, func, &args, instr.span())?;
                    self.regs[base + dst as usize] = slot;
                }

                // ---- tuples ----
                Op::MakeTuple { dst, elems } => {
                    let n = elems.len();
                    let mut items = Vec::with_capacity(n);
                    for i in 0..n {
                        items.push(self.regs[base + elems.get(i) as usize]);
                    }
                    self.charge(n * std::mem::size_of::<Slot>())?;
                    let idx = self.heap.len();
                    self.heap.push(HeapObj::Tuple(items));
                    self.regs[base + dst as usize] = Slot::heap(idx);
                }
                Op::GetTuple { dst, base: tbase, index } => {
                    let idx = self.regs[base + tbase as usize].as_heap();
                    let item = match self.heap.get(idx) {
                        Some(HeapObj::Tuple(items)) => items.get(index).copied().ok_or_else(|| {
                            err_at(format!("tuple index {} out of range", index), instr.span())
                        })?,
                        _ => return Err(err_at("cannot index non-tuple", instr.span())),
                    };
                    self.regs[base + dst as usize] = item;
                }

                // ---- return ----
                Op::Ret { src } => {
                    let v = self.regs[base + src as usize];
                    if fi == 0 {
                        return Ok(v);
                    }
                    // Pop to the caller: restore its window, ip and code slice.
                    let ret_to = self.frames[fi].ret_to;
                    self.frames.pop();
                    fi -= 1;
                    let pf = &self.frames[fi];
                    base = pf.base;
                    ip = pf.ip;
                    code = program.funcs[pf.func as usize].code.as_slice();
                    self.regs[base + ret_to as usize] = v;
                }
            }
        }
    }

    fn cmp_str<F: Fn(&str, &str) -> bool>(
        &mut self,
        base: usize,
        dst: Reg,
        a: Reg,
        b: Reg,
        span: Span,
        f: F,
    ) -> Result<()> {
        // `to_string` sidesteps holding a borrow of `self.heap` across `set`.
        let x = self.heap_str(self.regs[base + a as usize].as_heap(), span)?.to_string();
        let y = self.heap_str(self.regs[base + b as usize].as_heap(), span)?.to_string();
        let r = f(&x, &y);
        self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
        Ok(())
    }

    fn gather_values(&self, base: usize, args: &Regs, tys: &[crate::ty::Ty]) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for (i, r) in args.iter().enumerate() {
            let ty = tys.get(i).unwrap_or(&crate::ty::Ty::Unit);
            out.push(self.slot_to_value(self.regs[base + r as usize], ty)?);
        }
        Ok(out)
    }

    // ---- heap accessors ----

    fn heap_str(&self, idx: usize, span: Span) -> Result<&str> {
        match self.heap.get(idx) {
            Some(HeapObj::Str(s)) => Ok(s.as_ref()),
            _ => Err(err_at("expected string in slot", span)),
        }
    }

    fn heap_handle(&self, idx: usize, span: Span) -> Result<&Handle> {
        match self.heap.get(idx) {
            Some(HeapObj::Handle(h)) => Ok(h),
            _ => Err(err_at("expected handle in slot", span)),
        }
    }

    /// Object identity for opaque handles: `Handle`'s `PartialEq` compares
    /// the payload's `Rc` pointer (plus the registered type name), so two
    /// heap entries holding clones of the same host object compare equal
    /// while genuinely different objects do not.
    fn handle_eq(&self, base: usize, a: Reg, b: Reg, span: Span) -> Result<bool> {
        let ha = self.heap_handle(self.regs[base + a as usize].as_heap(), span)?;
        let hb = self.heap_handle(self.regs[base + b as usize].as_heap(), span)?;
        Ok(ha == hb)
    }

    // ---- boundary conversions ----

    fn value_to_slot(&mut self, v: Value, ty: &crate::ty::Ty) -> Result<Slot> {
        use crate::ty::Ty;
        match (v, ty) {
            (Value::I64(n), _) => Ok(Slot::i64(n)),
            (Value::F64(f), _) => Ok(Slot::f64(f)),
            (Value::Bool(b), _) => Ok(Slot(if b { 1 } else { 0 })),
            (Value::Str(s), _) => {
                self.charge(s.len())?;
                let idx = self.heap.len();
                self.heap.push(HeapObj::Str(s));
                Ok(Slot::heap(idx))
            }
            (Value::Handle(h), _) => {
                self.charge(0)?;
                let idx = self.heap.len();
                self.heap.push(HeapObj::Handle(h));
                Ok(Slot::heap(idx))
            }
            (Value::Tuple(vals), _) => {
                self.charge(vals.len() * std::mem::size_of::<Slot>())?;
                let tys = match ty {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => Vec::new(),
                };
                let mut items = Vec::with_capacity(vals.len());
                for (i, val) in vals.into_iter().enumerate() {
                    let t = tys.get(i).cloned().unwrap_or(Ty::Unit);
                    items.push(self.value_to_slot(val, &t)?);
                }
                let idx = self.heap.len();
                self.heap.push(HeapObj::Tuple(items));
                Ok(Slot::heap(idx))
            }
            (Value::Unit, _) => Ok(Slot(0)),
        }
    }

    /// Convert a slot back to a Value using a known type.
    fn slot_to_value(&self, slot: Slot, ty: &crate::ty::Ty) -> Result<Value> {
        use crate::ty::Ty;
        match ty {
            Ty::I64 => Ok(Value::I64(slot.as_i64())),
            Ty::F64 => Ok(Value::F64(slot.as_f64())),
            Ty::Bool => Ok(Value::Bool(slot.as_bool())),
            Ty::Unit => Ok(Value::Unit),
            Ty::Str => Ok(Value::Str(Rc::from(
                self.heap_str(slot.as_heap(), Span::synthetic())?,
            ))),
            Ty::Handle(_) => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Handle(h)) => Ok(Value::Handle(h.clone())),
                _ => Err(Error::runtime("expected handle in slot")),
            },
            Ty::Tuple(ts) => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Tuple(items)) => {
                    let mut vals = Vec::with_capacity(items.len());
                    for (item, t) in items.iter().zip(ts.iter()) {
                        vals.push(self.slot_to_value(*item, t)?);
                    }
                    Ok(Value::Tuple(vals))
                }
                _ => Err(Error::runtime("expected tuple in slot")),
            },
            Ty::Var(_) => Ok(Value::I64(slot.as_i64())),
        }
    }

    // ---- builtins ----

    fn invoke_builtin(&mut self, base: usize, func: u8, args: &Regs, span: Span) -> Result<Slot> {
        use ir::builtin::*;
        let n = args.len();
        let at = |i: usize| base + args.get(i) as usize;
        match func {
            ABS | SQRT | FLOOR | CEIL => {
                if n != 1 {
                    return Err(err_at("builtin arity", span));
                }
                let x = self.regs[at(0)].as_f64();
                let r = match func {
                    ABS => x.abs(),
                    SQRT => x.sqrt(),
                    FLOOR => x.floor(),
                    CEIL => x.ceil(),
                    _ => unreachable!(),
                };
                Ok(Slot::f64(r))
            }
            POW | MIN | MAX => {
                if n != 2 {
                    return Err(err_at("builtin arity", span));
                }
                let x = self.regs[at(0)].as_f64();
                let y = self.regs[at(1)].as_f64();
                let r = match func {
                    POW => x.powf(y),
                    MIN => x.min(y),
                    MAX => x.max(y),
                    _ => unreachable!(),
                };
                Ok(Slot::f64(r))
            }
            ABS_I64 => {
                if n != 1 {
                    return Err(err_at("builtin arity", span));
                }
                // `i64::MIN.abs()` panics in Rust; the language reports it as
                // an overflow instead, like every other i64 operation here.
                let r = self.regs[at(0)]
                    .as_i64()
                    .checked_abs()
                    .ok_or_else(|| err_at("i64 abs overflow", span))?;
                Ok(Slot::i64(r))
            }
            MIN_I64 | MAX_I64 => {
                if n != 2 {
                    return Err(err_at("builtin arity", span));
                }
                let x = self.regs[at(0)].as_i64();
                let y = self.regs[at(1)].as_i64();
                Ok(Slot::i64(if func == MIN_I64 { x.min(y) } else { x.max(y) }))
            }
            LEN => {
                if n != 1 {
                    return Err(err_at("builtin arity", span));
                }
                let s = self.heap_str(self.regs[at(0)].as_heap(), span)?;
                Ok(Slot::i64(s.chars().count() as i64))
            }
            CONTAINS => {
                if n != 2 {
                    return Err(err_at("builtin arity", span));
                }
                let a = self.heap_str(self.regs[at(0)].as_heap(), span)?.to_string();
                let b = self.heap_str(self.regs[at(1)].as_heap(), span)?.to_string();
                Ok(Slot(if a.contains(&b) { 1 } else { 0 }))
            }
            SUBSTR => {
                if n != 3 {
                    return Err(err_at("builtin arity", span));
                }
                let s = self.heap_str(self.regs[at(0)].as_heap(), span)?.to_string();
                let start = self.regs[at(1)].as_i64().max(0) as usize;
                let len = self.regs[at(2)].as_i64().max(0) as usize;
                let chars: Vec<char> = s.chars().collect();
                let st = start.min(chars.len());
                let end = (start + len).min(chars.len());
                let out: String = chars[st..end].iter().collect();
                self.intern_runtime_str(out)
            }
            TO_I64 => {
                if n != 1 {
                    return Err(err_at("builtin arity", span));
                }
                Ok(Slot::i64(self.regs[at(0)].as_f64() as i64))
            }
            TO_F64 => {
                if n != 1 {
                    return Err(err_at("builtin arity", span));
                }
                Ok(Slot::f64(self.regs[at(0)].as_i64() as f64))
            }
            TO_STR_F64 => {
                let s = self.regs[at(0)].as_f64().to_string();
                self.intern_runtime_str(s)
            }
            TO_STR_I64 => {
                let s = self.regs[at(0)].as_i64().to_string();
                self.intern_runtime_str(s)
            }
            TO_STR_BOOL => {
                let s = self.regs[at(0)].as_bool().to_string();
                self.intern_runtime_str(s)
            }
            TO_STR_STR => Ok(self.regs[at(0)]),
            _ => Err(err_at("unknown builtin", span)),
        }
    }

    fn intern_runtime_str(&mut self, s: String) -> Result<Slot> {
        self.charge(s.len())?;
        let idx = self.heap.len();
        self.heap.push(HeapObj::Str(Rc::from(s)));
        Ok(Slot::heap(idx))
    }
}

fn err_at(msg: impl Into<String>, span: Span) -> Error {
    Error::runtime_at(msg, span)
}
