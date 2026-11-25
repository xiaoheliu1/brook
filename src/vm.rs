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
use crate::ty::Ty;
use crate::validate::ValidatedCode;
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
/// runtime strings (substr/to_str), tuples and arrays are appended lazily.
enum HeapObj {
    Str(Rc<str>),
    Handle(Handle),
    Tuple(Vec<Slot>),
    /// Arrays (ADR-018). Reference semantics fall out of the slot model:
    /// copying a slot copies the heap *index*, so `let b = a` aliases the
    /// same `Arr` entry and `ArrPush` mutates it in place — exactly the
    /// Python-style behaviour the type checker promises. Cycles cannot be
    /// constructed (`push(a, a)` is rejected by the occurs check), so entries
    /// staying alive for the whole call is bounded by the gas/quota budget.
    Arr(Vec<Slot>),
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
///   - `validate` (run at program construction) guarantees every `Reg` in an
///     instruction is `< nregs` for the function that owns it;
///   - `Vm::call` / `CallScript` resize the file so `base + nregs <= len()` for
///     the active frame before its first instruction executes.
///     A `debug_assert` makes any violation of those invariants loud in debug
///     builds; in release it compiles away to a plain unchecked load/store.
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
    /// O(n) 内建（len/contains/substr/concat/字符串比较）产生的额外 gas
    /// （D-046 3.4）：一条指令计 1 步会让 `max_steps` 与真实工作量脱钩——
    /// `len(4MiB 串)` 循环 1000 万轮只烧 4000 万步，宿主线程实际扫了
    /// 10^13 个字符。计费单位在指令内累积、由 run 循环顶部折叠进 steps。
    pending: u64,
    /// Optional host "should I stop?" callback. `None` keeps the old hot loop
    /// byte-for-byte: no callback, no branch, no extra work per instruction.
    cancel: Option<std::rc::Rc<dyn Fn() -> bool>>,
}

/// How many instructions between two cancellation probes. Matches the legacy
/// rhai host's `PROGRESS_CHECK_EVERY` so the user-visible stop latency is
/// unchanged after the engine swap. A native callback every step would make
/// normal scripts unusable (JNI round-trip per instruction).
const CANCEL_CHECK_EVERY: u64 = 65_536;

impl<'a> Vm<'a> {
    /// Validate the caller-supplied program before constructing a VM.
    pub fn new(
        program: &'a Program,
        host: &'a HostRegistry,
        max_steps: u64,
        max_depth: usize,
        max_heap: u64,
    ) -> Result<Self> {
        Self::with_cancel(program, host, max_steps, max_depth, max_heap, None)
    }

    /// Same as [`Vm::new`] with a host cancellation hook attached.
    /// Returns an error when the program is not structurally valid.
    pub fn with_cancel(
        program: &'a Program,
        host: &'a HostRegistry,
        max_steps: u64,
        max_depth: usize,
        max_heap: u64,
        cancel: Option<std::rc::Rc<dyn Fn() -> bool>>,
    ) -> Result<Self> {
        crate::validate::validate(program, host.len())?;
        Ok(Self::build(
            program, host, max_steps, max_depth, max_heap, cancel,
        ))
    }

    /// Engine-only fast path: immutability preserves validation, leaving an O(1) host-count check.
    pub(crate) fn from_validated(
        code: &'a ValidatedCode,
        host: &'a HostRegistry,
        max_steps: u64,
        max_depth: usize,
        max_heap: u64,
        cancel: Option<std::rc::Rc<dyn Fn() -> bool>>,
    ) -> Result<Self> {
        if code.host_count() != host.len() {
            return Err(Error::runtime(format!(
                "validated program expects {} host slots, registry has {}",
                code.host_count(),
                host.len()
            )));
        }
        Ok(Self::build(
            code.program(),
            host,
            max_steps,
            max_depth,
            max_heap,
            cancel,
        ))
    }

    fn build(
        program: &'a Program,
        host: &'a HostRegistry,
        max_steps: u64,
        max_depth: usize,
        max_heap: u64,
        cancel: Option<std::rc::Rc<dyn Fn() -> bool>>,
    ) -> Self {
        // Pre-intern all string constants so ConstStr never allocates.
        let heap = program
            .strings
            .iter()
            .map(|s| HeapObj::Str(Rc::clone(s)))
            .collect();
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
            pending: 0,
            cancel,
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

    /// O(n) 指令的工作量计费（D-046 3.4）。单位按操作数长度折算：真实
    /// 扫描是字节级，这里 1 gas ≈ 64 字节——足够让"1 步指令 × 海量轮次"
    /// 的放大攻击被 `max_steps` 提前拦住，又不影响正常脚本的预算体感。
    #[inline]
    fn charge_work(&mut self, units: u64) {
        self.pending = self.pending.saturating_add(units / 64 + 1);
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
        // 纵深防御（validate 已拦，这里是 safe-API 侧的第二道）：实参写入
        // 上界是 params.len()，窗口只有 nregs 格——两者在缓存路径上独立，
        // 编译产物恒满足，但这条约束必须在这里也成立才没有 unchecked 越界。
        if f.params.len() > f.nregs as usize {
            return Err(Error::runtime(
                "function signature exceeds its register window (corrupt program)",
            ));
        }
        let nregs = f.nregs as usize;
        let ret_ty = f.ret.clone();

        // 可重入防护（D-046 3.2）：错误路径刻意保留 frames 供 capture_trace，
        // 成功路径的底帧也不 pop——复用同一个 `Vm` 会带着陈旧帧再次进入
        // `run()`，Ret 时恢复出旧的 base/ip/code，指向已经缩小的寄存器窗口
        // 之外的地址（unchecked 写）。`Engine::call` 每次 new 一个 Vm 所以
        // 不会踩到，但 `Vm` 是 pub 的 safe API：重置到 `Vm::new` 后的初始
        // 状态（常量池长度 + 零字节计费），让复用语义干净。
        self.frames.clear();
        self.heap.truncate(self.program.strings.len());
        self.heap_bytes = 0;

        // 内存配额覆盖三块此前的盲区（D-046 3.5）：
        // (b) 常量池入账——池在 Vm::new 每次 call 全量物化，此前配额为 0；
        // (a) 初始寄存器窗口按 8 字节/槽计费——恶意 .sbc 的 65535 窗口
        //     ×256 深度 ≈ 128 MiB 的上界从此受 max_heap 约束。
        let pool_bytes: usize = self.program.strings.iter().map(|s| s.len()).sum();
        self.charge(pool_bytes)?;
        self.charge(nregs * std::mem::size_of::<Slot>())?;

        self.regs.clear();
        self.regs.resize(nregs, Slot(0));
        for (i, (v, ty)) in args.into_iter().zip(f.params.iter()).enumerate() {
            let slot = self.value_to_slot(v, ty)?;
            self.regs[i] = slot;
        }
        self.frames.push(Frame {
            func,
            base: 0,
            ip: 0,
            ret_to: 0,
            nregs,
        });

        // Monomorphise the dispatch loop into two copies: with and without
        // cancellation. The no-hook copy has the probe compiled out entirely
        // (measured: leaving even one always-false u64 compare in the loop
        // cost ~16% on fib(18)). `run` below is written once as a generic.
        let result = match if self.cancel.is_some() {
            self.run::<true>()
        } else {
            self.run::<false>()
        } {
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
                f.ip.checked_sub(1)
                    .and_then(|i| code.get(i))
                    .map(|i| (i.line as usize, i.col as usize))
            };
            let (line, col) = loc.unwrap_or((0, 0));
            trace.push(TraceFrame { func, line, col });
        }
        trace
    }

    /// Cancellation probe, reached once every [`CANCEL_CHECK_EVERY`] steps.
    ///
    /// `#[cold]` + `#[inline(never)]` is deliberate: the closure call and the
    /// error construction must stay out of the dispatch loop's straight-line
    /// code. The previous parser-depth guard regressed compile time 15% just
    /// by putting a `format!` in a hot helper; same lesson applied here.
    #[cold]
    #[inline(never)]
    fn poll_cancel(
        cancel: &Option<Rc<dyn Fn() -> bool>>,
        next: &mut u64,
        steps: u64,
        span: Span,
    ) -> Result<()> {
        *next = steps.saturating_add(CANCEL_CHECK_EVERY);
        if let Some(cb) = cancel {
            if cb() {
                return Err(Error::cancelled(span));
            }
        }
        Ok(())
    }

    /// The dispatch loop. Frames are pushed/popped by CallScript/Ret.
    ///
    /// `CANCEL` is a compile-time switch: `false` folds the probe away so the
    /// no-hook build has byte-identical hot-loop code to the pre-hook engine.
    fn run<const CANCEL: bool>(&mut self) -> Result<Slot> {
        // `program` is a shared reference with lifetime `'a`; copying it out of
        // `self` means the code slice below does not borrow the VM, so the loop
        // body can freely mutate registers, the heap and the frame stack.
        let program: &'a Program = self.program;
        let max_steps = self.max_steps;
        let max_depth = self.max_depth;
        // Clone the Rc once per call so the loop body never borrows `self`
        // immutably while mutating registers. When CANCEL is false the
        // compiler drops both this and the probe below as dead code.
        let cancel = if CANCEL { self.cancel.clone() } else { None };
        let mut next_cancel_check = CANCEL_CHECK_EVERY;

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
            if CANCEL && steps >= next_cancel_check {
                Self::poll_cancel(&cancel, &mut next_cancel_check, steps, instr.span())?;
            }

            match instr.op {
                // ---- constants & moves ----
                Op::ConstI64 { dst, val } => self.regs[base + dst as usize] = Slot::i64(val),
                Op::ConstF64 { dst, val } => self.regs[base + dst as usize] = Slot::f64(val),
                Op::ConstBool { dst, val } => {
                    self.regs[base + dst as usize] = Slot(if val { 1 } else { 0 })
                }
                Op::ConstStr { dst, id } => {
                    self.regs[base + dst as usize] = Slot::heap(id as usize)
                }
                Op::ConstUnit { dst } => self.regs[base + dst as usize] = Slot(0),
                Op::Mov { dst, src } => {
                    let v = self.regs[base + src as usize];
                    self.regs[base + dst as usize] = v;
                }

                // ---- integer arithmetic ----
                Op::AddI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x
                        .checked_add(y)
                        .ok_or_else(|| err_at("i64 add overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::SubI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x
                        .checked_sub(y)
                        .ok_or_else(|| err_at("i64 sub overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::MulI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    let r = x
                        .checked_mul(y)
                        .ok_or_else(|| err_at("i64 mul overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::DivI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    if y == 0 {
                        return Err(err_at("division by zero", instr.span()));
                    }
                    let r = x
                        .checked_div(y)
                        .ok_or_else(|| err_at("i64 div overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::ModI64 { dst, a, b } => {
                    let x = self.regs[base + a as usize].as_i64();
                    let y = self.regs[base + b as usize].as_i64();
                    if y == 0 {
                        return Err(err_at("modulo by zero", instr.span()));
                    }
                    let r = x
                        .checked_rem(y)
                        .ok_or_else(|| err_at("i64 mod overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::NegI64 { dst, src } => {
                    let x = self.regs[base + src as usize].as_i64();
                    let r = x
                        .checked_neg()
                        .ok_or_else(|| err_at("i64 negation overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }
                Op::AddI64Imm { dst, src, imm } => {
                    let x = self.regs[base + src as usize].as_i64();
                    let r = x
                        .checked_add(imm)
                        .ok_or_else(|| err_at("i64 add overflow", instr.span()))?;
                    self.regs[base + dst as usize] = Slot::i64(r);
                }

                // ---- float arithmetic ----
                Op::AddF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        + self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::SubF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        - self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot::f64(r);
                }
                Op::MulF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        * self.regs[base + b as usize].as_f64();
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
                    let r = self.regs[base + a as usize].as_i64()
                        == self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64()
                        != self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LtI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64()
                        < self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64()
                        <= self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GtI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64()
                        > self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GeI64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_i64()
                        >= self.regs[base + b as usize].as_i64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- float comparisons ----
                Op::EqF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        == self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        != self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LtF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        < self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::LeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        <= self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GtF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        > self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::GeF64 { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_f64()
                        >= self.regs[base + b as usize].as_f64();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- bool comparisons ----
                Op::EqBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool()
                        == self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::NeBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool()
                        != self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }

                // ---- string comparisons ----
                // 比较臂各自结算 O(n) 计费（3.4）——见 cmp_str 的注释
                Op::EqStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x == y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::NeStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x != y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::LtStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x < y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::LeStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x <= y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::GtStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x > y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::GeStr { dst, a, b } => {
                    let w = self.cmp_str(base, dst, a, b, instr.span(), |x, y| x >= y)?;
                    steps = steps.saturating_add(w);
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }

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
                    let r = self.regs[base + a as usize].as_bool()
                        && self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::OrBool { dst, a, b } => {
                    let r = self.regs[base + a as usize].as_bool()
                        || self.regs[base + b as usize].as_bool();
                    self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
                }
                Op::Concat { dst, a, b } => {
                    // 先按结果长度计费再分配（D-046 3.5c）：旧实现注释明说
                    // "先建串再计费"——临时分配峰值不受配额约束。 gas 同步
                    // 按长度计（3.4）。
                    let (xlen, ylen) = {
                        let x =
                            self.heap_str(self.regs[base + a as usize].as_heap(), instr.span())?;
                        let y =
                            self.heap_str(self.regs[base + b as usize].as_heap(), instr.span())?;
                        (x.len(), y.len())
                    };
                    self.charge_work((xlen + ylen) as u64);
                    self.charge(xlen + ylen)?;
                    let joined = {
                        let x =
                            self.heap_str(self.regs[base + a as usize].as_heap(), instr.span())?;
                        let y =
                            self.heap_str(self.regs[base + b as usize].as_heap(), instr.span())?;
                        let mut s = String::with_capacity(x.len() + y.len());
                        s.push_str(x);
                        s.push_str(y);
                        s
                    };
                    // 直接入堆（concat 已计费，跳过 intern 的二次计费）
                    let idx = self.heap.len();
                    self.heap.push(HeapObj::Str(Rc::from(joined)));
                    self.regs[base + dst as usize] = Slot::heap(idx);
                    // O(n) 计费结算（3.4）：所有充电点都在 CallBuiltin /
                    // Concat 臂内，臂尾结算覆盖全部充电、无逃逸；循环顶部
                    // 保持零开销（紧循环如 sum_to 不付任何代价）。
                    steps = steps.saturating_add(std::mem::take(&mut self.pending));
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }

                // ---- control flow ----
                Op::Jump { target } => ip = target,
                Op::JumpIfFalse { cond, target } => {
                    if !self.regs[base + cond as usize].as_bool() {
                        ip = target;
                    }
                }

                // ---- fused compare-and-branch ----
                Op::JmpIfEqI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64()
                        == self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfNeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64()
                        != self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfLtI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() < self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfLeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64()
                        <= self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfGtI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64() > self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfGeI64 { a, b, target } => {
                    if self.regs[base + a as usize].as_i64()
                        >= self.regs[base + b as usize].as_i64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfEqF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64()
                        == self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfNeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64()
                        != self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfLtF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() < self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfLeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64()
                        <= self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfGtF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64() > self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                Op::JmpIfGeF64 { a, b, target } => {
                    if self.regs[base + a as usize].as_f64()
                        >= self.regs[base + b as usize].as_f64()
                    {
                        ip = target;
                    }
                }
                // ---- fused compare-immediate branch (one register read) ----
                Op::JmpIfEqI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() == imm {
                        ip = target;
                    }
                }
                Op::JmpIfNeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() != imm {
                        ip = target;
                    }
                }
                Op::JmpIfLtI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() < imm {
                        ip = target;
                    }
                }
                Op::JmpIfLeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() <= imm {
                        ip = target;
                    }
                }
                Op::JmpIfGtI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() > imm {
                        ip = target;
                    }
                }
                Op::JmpIfGeI64Imm { a, imm, target } => {
                    if self.regs[base + a as usize].as_i64() >= imm {
                        ip = target;
                    }
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
                        // 寄存器文件按【实际增长】计费（D-046 3.5a）：扁平
                        // 寄存器文件被所有帧共享，真实内存 = 最深层栈 × 窗口
                        // 宽度——按调用次数计费会把 fib 这类合法递归误杀。
                        // nregs 因"寄存器只增不复用"极易做大（65535 窗口 ×
                        // 256 深度 ≈ 128 MiB 上界），现在受 max_heap 约束。
                        let delta = (nbase + nregs - self.regs.len()) * std::mem::size_of::<Slot>();
                        self.charge(delta)?;
                        self.regs.resize(nbase + nregs, Slot(0));
                    }
                    for i in 0..n {
                        self.regs[nbase + i] = argv[i];
                    }

                    self.frames[fi].ip = ip;
                    let ncode = nf.code.as_slice();
                    self.frames.push(Frame {
                        func,
                        base: nbase,
                        ip: 0,
                        ret_to: dst,
                        nregs,
                    });
                    fi += 1;
                    base = nbase;
                    ip = 0;
                    code = ncode;
                }
                Op::CallHost { func, args, dst } => {
                    let sig = self
                        .host
                        .sig_by_id(func)
                        .map_err(|e| Error::new(e.kind, e.message, Some(instr.span())))?;
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
                    // O(n) 计费结算（3.4）：len/contains/substr/比较内建的
                    // 字符扫描按操作数长度折算 pending，在此结算并检查——
                    // 比较指令（EqStr 等）经由 cmp_str 充电，下一次任何
                    // CallBuiltin/Concat 臂也会清账；纯算术循环零开销。
                    steps = steps.saturating_add(std::mem::take(&mut self.pending));
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
                }
                Op::ContainsTyped {
                    hay,
                    needle,
                    dst,
                    ty,
                } => {
                    let slot = self.contains_typed(base, hay, needle, ty, instr.span())?;
                    self.regs[base + dst as usize] = slot;
                    // 和 CallBuiltin 一样结算 O(n) 扫描的 gas（D-047：嵌套
                    // 结构的逐元素比较必须计费，否则千倍放大）。
                    steps = steps.saturating_add(std::mem::take(&mut self.pending));
                    if steps > max_steps {
                        return Err(err_at("step (gas) limit exceeded", instr.span()));
                    }
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
                Op::GetTuple {
                    dst,
                    base: tbase,
                    index,
                } => {
                    let idx = self.regs[base + tbase as usize].as_heap();
                    let item = match self.heap.get(idx) {
                        Some(HeapObj::Tuple(items)) => {
                            items.get(index).copied().ok_or_else(|| {
                                err_at(format!("tuple index {} out of range", index), instr.span())
                            })?
                        }
                        _ => return Err(err_at("cannot index non-tuple", instr.span())),
                    };
                    self.regs[base + dst as usize] = item;
                }

                // ---- arrays (ADR-018) ----
                Op::NewArr { dst } => {
                    self.charge(0)?;
                    let idx = self.heap.len();
                    self.heap.push(HeapObj::Arr(Vec::new()));
                    self.regs[base + dst as usize] = Slot::heap(idx);
                }
                Op::ArrPush { arr, src } => {
                    let v = self.regs[base + src as usize];
                    let a = self.regs[base + arr as usize].as_heap();
                    self.array_push(a, v, instr.span())?;
                }
                Op::ArrLen { dst, src } => {
                    let idx = self.regs[base + src as usize].as_heap();
                    let n = match self.heap.get(idx) {
                        Some(HeapObj::Arr(items)) => items.len(),
                        _ => return Err(err_at("expected array in slot", instr.span())),
                    };
                    self.regs[base + dst as usize] = Slot::i64(n as i64);
                }
                Op::ArrGet {
                    dst,
                    base: arr,
                    idx,
                } => {
                    let a = self.regs[base + arr as usize].as_heap();
                    let i = self.regs[base + idx as usize].as_i64();
                    let item = match self.heap.get(a) {
                        Some(HeapObj::Arr(items)) => {
                            if i < 0 || i as usize >= items.len() {
                                return Err(err_at(
                                    format!("array index {} out of range (len {})", i, items.len()),
                                    instr.span(),
                                ));
                            }
                            items[i as usize]
                        }
                        _ => return Err(err_at("expected array in slot", instr.span())),
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
    ) -> Result<u64> {
        // 无拷贝比较（D-046 4.3）：旧实现的两次 `to_string` 是纯浪费——
        // NLL 下读操作放块里，借用随块结束释放，写寄存器在块外照常进行。
        // O(n) 扫描按两串长度计 gas（3.4）：返回计费值，由六个比较臂各自
        // 结算（比较指令不在 CallBuiltin 臂内，写 self.pending 会逃逸）。
        let (xlen, ylen) = {
            let x = self.heap_str(self.regs[base + a as usize].as_heap(), span)?;
            let y = self.heap_str(self.regs[base + b as usize].as_heap(), span)?;
            (x.len(), y.len())
        };
        let work = (xlen + ylen) as u64 / 64 + 1;
        let r = {
            let x = self.heap_str(self.regs[base + a as usize].as_heap(), span)?;
            let y = self.heap_str(self.regs[base + b as usize].as_heap(), span)?;
            f(x, y)
        };
        self.regs[base + dst as usize] = Slot(if r { 1 } else { 0 });
        Ok(work)
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

    /// Append one element to an array heap entry (checked before mutating).
    /// Shared by `Op::ArrPush` and the `push` builtin.
    fn array_push(&mut self, arr_idx: usize, v: Slot, span: Span) -> Result<()> {
        if arr_idx >= self.heap.len() || !matches!(self.heap[arr_idx], HeapObj::Arr(_)) {
            return Err(err_at("expected array in slot", span));
        }
        // Quota first: a hostile push loop must die before growing the heap.
        self.charge(std::mem::size_of::<Slot>())?;
        match &mut self.heap[arr_idx] {
            HeapObj::Arr(items) => items.push(v),
            _ => unreachable!("checked above"),
        }
        Ok(())
    }

    /// Structural equality driven by the needle's **static** element type.
    ///
    /// `Slot` is an untagged `u64`: two unequal scalars look exactly like two
    /// heap indices, so the old "try both as heap indices" comparison could
    /// return a wrong answer (equal strings at indices 1 and 2) or recurse
    /// forever on a self-similar nested array (D-047). The type comes from
    /// codegen; recursion depth is bounded by the static type's nesting (and
    /// hence by `MAX_NESTING`), so it cannot cycle. `work` accumulates the
    /// O(n) scan units that `contains_typed` charges as gas.
    fn slots_equal_ty(
        &self,
        a: Slot,
        b: Slot,
        ty: &Ty,
        span: Span,
        work: &mut u64,
    ) -> Result<bool> {
        if a.0 == b.0 {
            // Identical scalars, or literally the same heap entry. F64 needs a
            // value comparison (NaN != NaN, -0.0 == 0.0), and arrays/tuples
            // are structural: a repeated NaN element must still be unequal.
            // Skipping the shortcut here also keeps the VM aligned with
            // `Value`'s derived `PartialEq` on the interpreter path.
            match ty {
                Ty::F64 | Ty::Arr(_) | Ty::Tuple(_) => {}
                _ => return Ok(true),
            }
        }
        match ty {
            // Scalars are stored inline. I64/Bool compare by bits; F64 uses
            // IEEE value equality so it agrees with `==` and `Value::eq`.
            // Unequal bits are never reinterpreted as heap indices.
            Ty::I64 | Ty::Bool => Ok(false),
            Ty::F64 => Ok(a.as_f64() == b.as_f64()),
            Ty::Str => {
                let x = self.heap_str(a.as_heap(), span)?;
                let y = self.heap_str(b.as_heap(), span)?;
                *work += (x.len() + y.len()) as u64;
                Ok(x == y)
            }
            Ty::Handle(_) => {
                let x = self.heap_handle(a.as_heap(), span)?;
                let y = self.heap_handle(b.as_heap(), span)?;
                Ok(x == y)
            }
            Ty::Arr(elem) => {
                let x = match self.heap.get(a.as_heap()) {
                    Some(HeapObj::Arr(v)) => v,
                    _ => return Err(err_at("contains: expected array element", span)),
                };
                let y = match self.heap.get(b.as_heap()) {
                    Some(HeapObj::Arr(v)) => v,
                    _ => return Err(err_at("contains: expected array element", span)),
                };
                if x.len() != y.len() {
                    return Ok(false);
                }
                *work += x.len() as u64 * 8;
                for (p, q) in x.iter().zip(y.iter()) {
                    if !self.slots_equal_ty(*p, *q, elem, span, work)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Ty::Tuple(tys) => {
                let x = match self.heap.get(a.as_heap()) {
                    Some(HeapObj::Tuple(v)) => v,
                    _ => return Err(err_at("contains: expected tuple element", span)),
                };
                let y = match self.heap.get(b.as_heap()) {
                    Some(HeapObj::Tuple(v)) => v,
                    _ => return Err(err_at("contains: expected tuple element", span)),
                };
                if x.len() != y.len() || x.len() != tys.len() {
                    return Ok(false);
                }
                *work += x.len() as u64 * 8;
                for ((p, q), t) in x.iter().zip(y.iter()).zip(tys.iter()) {
                    if !self.slots_equal_ty(*p, *q, t, span, work)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // Cannot escape a successful typecheck (codegen maps `Var` to the
            // i64 default and `contains` rejects `Unit` operands).
            Ty::Var(_) | Ty::Unit => Ok(false),
        }
    }

    /// Typed replacement for the old `CONTAINS` builtin (D-047).
    fn contains_typed(
        &mut self,
        base: usize,
        hay: Reg,
        needle: Reg,
        ty_id: u32,
        span: Span,
    ) -> Result<Slot> {
        let ty = self
            .program
            .types
            .get(ty_id as usize)
            .ok_or_else(|| err_at("contains: invalid element type id", span))?;
        let hay_slot = self.regs[base + hay as usize];
        let needle_slot = self.regs[base + needle as usize];
        let mut work = 0u64;
        let found = match self.heap.get(hay_slot.as_heap()) {
            // String substring containment (unchanged semantics).
            Some(HeapObj::Str(s)) => {
                let n = self.heap_str(needle_slot.as_heap(), span)?;
                work += s.len() as u64;
                s.contains(n)
            }
            // Structural scan. Every element (including nested contents) is
            // charged through `slots_equal_ty`, not just the top-level length.
            Some(HeapObj::Arr(items)) => {
                work += items.len() as u64;
                let mut found = false;
                for e in items {
                    if self.slots_equal_ty(*e, needle_slot, ty, span, &mut work)? {
                        found = true;
                        break;
                    }
                }
                found
            }
            _ => return Err(err_at("contains expects a str or array", span)),
        };
        self.charge_work(work);
        Ok(Slot::i64(if found { 1 } else { 0 }))
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
            (Value::Arr(vals), _) => {
                // Clone (not take): the Rc payload may be shared with other
                // Values the host still holds; taking would empty them.
                let vals = vals.borrow().clone();
                self.charge(vals.len() * std::mem::size_of::<Slot>())?;
                let elem_ty = match ty {
                    Ty::Arr(t) => (**t).clone(),
                    _ => Ty::Unit,
                };
                let mut items = Vec::with_capacity(vals.len());
                for val in vals {
                    items.push(self.value_to_slot(val, &elem_ty)?);
                }
                let idx = self.heap.len();
                self.heap.push(HeapObj::Arr(items));
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
            // `Rc::from(&str)` 会重新分配并 memcpy 整个字符串；堆里本来就
            // 躺着现成的 `Rc<str>`，克隆指针即可（D-046 4.2：此前每次向
            // 宿主传字符串、每次 `Engine::call` 返回字符串都全量复制）。
            Ty::Str => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Str(s)) => Ok(Value::Str(Rc::clone(s))),
                _ => Err(Error::runtime("expected string in slot")),
            },
            Ty::Handle(_) => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Handle(h)) => Ok(Value::Handle(h.clone())),
                _ => Err(Error::runtime("expected handle in slot")),
            },
            Ty::Tuple(ts) => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Tuple(items)) => {
                    // 精确匹配（D-046 5.3）：旧实现 `zip` 按短侧截断——
                    // 堆里 3 元、签名 1 元时返回 `(1)`，后两个元素静默消失，
                    // 且与参考解释器（原样返回）分歧。编译产物由 typecheck
                    // 保证两侧相等，这里只拦不一致的构造。
                    if items.len() != ts.len() {
                        return Err(Error::runtime(format!(
                            "tuple arity mismatch: signature declares {}, value has {}",
                            ts.len(),
                            items.len()
                        )));
                    }
                    let mut vals = Vec::with_capacity(items.len());
                    for (item, t) in items.iter().zip(ts.iter()) {
                        vals.push(self.slot_to_value(*item, t)?);
                    }
                    Ok(Value::Tuple(vals))
                }
                _ => Err(Error::runtime("expected tuple in slot")),
            },
            Ty::Arr(t) => match self.heap.get(slot.as_heap()) {
                Some(HeapObj::Arr(items)) => {
                    let mut vals = Vec::with_capacity(items.len());
                    for item in items {
                        vals.push(self.slot_to_value(*item, t)?);
                    }
                    Ok(Value::Arr(Rc::new(std::cell::RefCell::new(vals))))
                }
                _ => Err(Error::runtime("expected array in slot")),
            },
            Ty::Var(_) => Ok(Value::I64(slot.as_i64())),
        }
    }

    // ---- builtins ----

    fn invoke_builtin(&mut self, base: usize, func: u8, args: &Regs, span: Span) -> Result<Slot> {
        use ir::builtin::*;
        let n = args.len();
        let arity = ir::builtin::arity(func).ok_or_else(|| err_at("unknown builtin", span))?;
        if n != arity {
            return Err(err_at(
                format!(
                    "builtin `{}` expects {arity} args, got {n}",
                    ir::builtin::name(func)
                ),
                span,
            ));
        }
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
                // Polymorphic since ADR-018: str (chars) / tuple / array
                // (elements). The checker restricts calls to these three.
                let idx = self.regs[at(0)].as_heap();
                let (r, work) = match self.heap.get(idx) {
                    Some(HeapObj::Str(s)) => (s.chars().count(), s.len() as u64),
                    Some(HeapObj::Tuple(items)) => (items.len(), 0),
                    Some(HeapObj::Arr(items)) => (items.len(), 0),
                    _ => return Err(err_at("len expects a str, tuple or array", span)),
                };
                self.charge_work(work); // 字符串的 O(n) 扫描计 gas（3.4）
                Ok(Slot::i64(r as i64))
            }
            CONTAINS => {
                // Retired in D-047: untagged slots cannot say whether a bit
                // pattern is a scalar or a heap index, so the old comparison
                // could silently return a wrong answer or recurse forever.
                // VERSION 8 rejects every old cache; a hand-crafted one must
                // be refused too rather than reaching that path again.
                Err(err_at("contains requires typed bytecode; recompile", span))
            }
            SUBSTR => {
                if n != 3 {
                    return Err(err_at("builtin arity", span));
                }
                // O(start+len) 字符定位，不再收集整个 `Vec<char>`（旧实现
                // 每次调用 4 字节/char 的中间分配）；`start + len` 的加法
                // 也随之消失——它在 32 位目标上是真实的溢出点
                // （`i64::MAX as usize` 截断回绕后 st>end → slice panic，
                // D-046 3.10）。O(n) 扫描按源串长度计 gas（3.4）。
                let src_idx = self.regs[at(0)].as_heap();
                let start = self.regs[at(1)].as_i64().max(0) as usize;
                let len = self.regs[at(2)].as_i64().max(0) as usize;
                // 先量长度并计 gas（3.4），释放借用后再取子串。
                let (st, en, work) = {
                    let s = self.heap_str(src_idx, span)?;
                    let st = s
                        .char_indices()
                        .nth(start)
                        .map(|(i, _)| i)
                        .unwrap_or(s.len());
                    let en = s
                        .char_indices()
                        .skip(start)
                        .nth(len)
                        .map(|(i, _)| i)
                        .unwrap_or(s.len());
                    (st, en, s.len() as u64)
                };
                self.charge_work(work);
                let out = {
                    let s = self.heap_str(src_idx, span)?;
                    s[st..en].to_string()
                };
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
            PUSH => {
                if n != 2 {
                    return Err(err_at("builtin arity", span));
                }
                let v = self.regs[at(1)];
                let a = self.regs[at(0)].as_heap();
                self.array_push(a, v, span)?;
                Ok(Slot(0)) // unit result; `push` is a statement in practice
            }
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
