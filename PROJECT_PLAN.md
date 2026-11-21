# 极简嵌入式脚本语言 · 项目方案与开发记录

> **文档性质**：活文档（living document）。本文件是本项目的单一真相来源。
> **维护规则**：
> - 架构/方案变更 → 更新对应章节，并在「第九章 变更日志」追加记录。
> - 重大技术取舍 → 在「第七章 决策记录」新增一条 ADR，不删旧记录。
> - 开发中遇到的问题与解决 → 在「第十章 开发日志」按日期追加。
> - 待办与开放问题 → 维护「第八章 开放问题」，解决后移至变更日志并标注。
> - 时间线倒序追加（新的记录放最上面）。

**创建日期**：2026-11-20
**当前状态**：阶段 3 功能全部落地（安全边界、`register_fn!` 类型化 FFI、C ABI / JNI 双 ABI 打包、`.sbc` 字节码缓存）。真机验证已于 2026-11-21 在 PLR110（Android 16 / arm64-v8a / SELinux Enforcing）完成（见 Q10 / D-032），产物已完成交叉编译 + ELF/导出符号校验。测试演进：自查修复后 64 项 → 迁移可行性验证后 65 项 → 补 `break` / `continue` 后 77 项 → 补 `/* */` 块注释与类型错误信息方向统一后 107 项 → 补诊断能力（字符列号 / 调用栈 / 源码片段渲染 / 宿主错误类型透传）后 118 项 → 补嵌套深度上限（修栈溢出崩溃）后 126 项 → 补数值内置 i64、C ABI 错误定位、线程/重载语义后 149 项（147 项单测/集成 + 2 个 doctest，另有 1 个 `ignore` 示例），release 构建通过；2026-11-21 静态审查修复两处 P1（if 表达式 else-if 深度守卫、EqHandle 对象身份语义）并解决 Q13/Q14（参考解释器对齐 VM、块值类型统一、validate 硬化），累计新增 10 项回归，**159 项全绿**（157 单测/集成 + 2 doctest + 1 ignore 示例）、release 构建零警告（shell 恢复后实测）；2026-11-21 C ABI 句柄式脚本层落地（ADR-015 / D-033）；同日解决 Q15 + fuzz 冒烟（D-034）、完成 rhai_demo/so_demo 迁移评估与 so_demo 改写 POC（D-035）；同日教程重构为纯脚本教程 + 宿主指南（D-038）、显式化 ADR-016（顶层函数式）并新增公共函数库模式（D-037）；同日 import 加固三线（D-044）；**数组进入语言（ADR-018 / D-045，Q16）**——Python 式引用语义、`for x in arr`、push/len/contains、宿主 `Vec<T>` 边界、Map 暂缓（重启条件见 ADR-018），`.sbc` VERSION 4→5，实测 **240 项全绿、release 零警告**、基线无回退
**项目代号**：`brook`（crate 名，已落地。名字为用户昵称「小河」的英译。曾用名链：`miniscript` → `vitul` → `brook`，见 D-040 / D-043）

---

## 第一章 · 项目概述

### 1.1 一句话定位

一门为 Rust 宿主设计的**极简、静态类型、高性能、可热重载**的嵌入式脚本语言，面向 Android 平台，走纯解释寄存器 VM 单模路线（原规划的 AOT 档已冻结为最低优先级备选，见 ADR-014）。

### 1.2 五个核心目标与对应策略

| # | 目标 | 对应策略 |
|---|---|---|
| 1 | 支持 Android | 纯 Rust 解释器，无 JIT、无动态代码生成；仅依赖 `std`、零第三方 crate，交叉编译到 arm64-v8a / armeabi-v7a 无特殊依赖 |
| 2 | 性能要高 | 静态类型消除运行时 tag；unboxed 槽位无装箱克隆；寄存器 VM 减少分发开销 |
| 3 | 安全 | gas 步数 + 内存配额 + 无 panic 保证 + 无内置 I/O + 不透明句柄；脚本无法触碰宿主内存布局 |
| 4 | 极简 | 砍掉类、trait、闭包、异常、模块、并发；四种标量 + 元组 + 句柄；全类型推断 |
| 5 | Rust 宿主通用脚本 | 宏注册 Rust 函数与类型，函数指针表直调，零序列化；运行时热重载，宿主无需重编译 |

### 1.3 关键结论摘要（多轮讨论达成）

1. **Android 不再一票否决 JIT（事实已修正）**：原表述「Android 强制 W^X + SELinux，第三方应用拿不到 `PROT_EXEC` 内存」经 AOSP 源码核查与真机报告证伪——AOSP 官方 sepolicy 明确 `allow appdomain self:process execmem;`（为 WebView / ART JIT 准备），`app_neverallows.te` 无 execmem 禁令；roto 报告在真实 user 版设备上实测 `mmap RW → mprotect RX` 通过。正确表述：**AOSP 默认允许第三方 app 的 JIT execmem，但 OEM 加固策略有差异，不保证每台设备放行（尤其直接 RWX）**；落地须配 execmem 探测 + VM 回退。详见 ADR-001 修订、Q11、ADR-012 草案。
2. **不打平 Roto，但理由变了**：Roto 不再是「Android 上不可用」，而是「成本/维护权衡下暂不上」。真正竞品仍是 Rhai（动态类型 AST 解释，慢 20–100×）；本方案目标位仍是原生的 3–8×。JIT 实测 fib 9.6× / sum_to 13.2×，已重回候选（Q11）。
3. **AOT 的卖点不是性能超越手写 Rust，而是「让逻辑保持独立可交付物的同时拿到原生性能」**。若脚本由自己人写、与宿主一起发布，AOT 多余，直接写 Rust。当前项目**先不做 AOT**，留作备选。【2026-11-21 注：已按 ADR-014 从路线图移除、冻结为最低优先级备选】
4. **热重载是默认档（VM）的原生能力**，不是妥协；AOT 档与热重载物理互斥（编译进二进制就要重编译）——这一互斥正是 ADR-014 把 AOT 冻结的根本理由。
5. **编译速度澄清**：改脚本触发的是「增量编译」（秒级），不是「完整构建」（分钟~小时）。二者场景不同，勿混淆。

---

## 第二章 · 架构设计

### 2.1 整体架构（双模，共用前端）

```
.script 源码
    ↓
前端：词法/语法解析 + 类型检查（全推断）
    ↓
寄存器字节码（类型特化；D-006：不用 SSA）
    ↓
VM 解释执行 → 热重载
    ↓
零拷贝宿主 FFI（函数指针表 + Opaque 句柄）
    ↓
Rust 宿主应用
```

唯一落地执行路径是寄存器 VM。原规划的 AOT 档已按 ADR-014 移出路线图、冻结为最低优先级备选（设计记录保留在 §2.4）。前端与 FFI 注册表对所有执行路径共用，这一原则不变。

### 2.2 为什么不用 JIT

> 【2026-11-21 注：本节事实前提已被 ADR-001 修订证伪——AOSP 默认允许第三方 app execmem；JIT 现为「成本权衡下的候选」，见 Q11 / ADR-012 草案。当前决策仍是 VM 优先，原文保留作历史。】

- **Android W^X / SELinux**：不允许应用将内存页同时标为可写可执行，JIT 生成机器码后无法执行。
- 系统组件（如 ART）有特权，第三方应用没有。
- 结论：JIT 路线在本项目下不可行，从架构中直接剔除。

### 2.3 VM 档（唯一执行引擎）

- **静态类型消灭 tag 检查**：编译期已知每个槽位类型，`ADD_I64` 直接操作裸 `i64`，零运行时判断。
- **unboxed 栈帧**：函数栈帧按编译期类型布局为 `Vec<u64>`，整数浮点内联；字符串用引用计数，无 GC。
- **寄存器式指令**：`ADD r3, r1, r2` 一条完成，避免栈式 VM 的 push/pop 往返。
- **字节码可序列化**：编译结果可落盘为 `.sbc` 缓存，二次加载跳过解析与类型检查（类似 `.pyc`）。已落地，见 ADR-010；实测磁盘 I/O 主导，推荐源码已在内存时使用 `compile_cached`。
- **模块级增量重载**：只重编改动模块，其余复用缓存。

### 2.4 AOT 档（已冻结 · 最低优先级备选 —— ADR-014）

> 2026-11-21 起从路线图移除。重新评估条件：「外部作者的规则必须纳秒级、且可接受牺牲热重载」的真实需求出现时——且届时应先对比 ADR-012 的 JIT 方案（可保留热重载、可回退 VM）。以下设计记录保留备查：

- 代码生成发生在 `cargo build` 阶段：`build.rs` 读脚本 → 生成 `.rs` → 编进宿主。
- 生成的代码就是普通 Rust，享受 rustc 单态化、内联、零成本抽象。
- **仅对 benchmark 证明为热点的极少数函数启用**（2/8 定律）。
- 实现时务必把生成代码放**独立 crate**，避免改动污染主 crate 触发大范围重编。

### 2.5 热重载机制

```
启动阶段（一次）：宿主启动 → 注册函数/类型 → 类型环境就绪
热循环（反复）：编辑 .script → 解析+类型检查（编译期查宿主签名）→ 生成字节码
                → VM 执行 → 结果/回调宿主 → 改完再来一轮
```

- `engine.reload(&mut script, path)` 重新编译并替换字节码（失败时旧脚本原样保留）。
- 正在执行的旧调用跑完不受影响，新调用用新逻辑。
- **并发语义来自类型系统，不靠锁**：`call` 取 `&Script`，`reload` 取 `&mut Script`，safe Rust 下二者不可能重叠，所以不存在「新调用读到半新半旧字节码」的窗口。已经交出去的 `Script` 克隆（`Rc` 共享堆字符串、深拷贝指令）继续跑旧逻辑。
- **线程模型**：`Engine` / `Script` / `Value` 都 **不是 `Send`/`Sync`**（`Rc` 承载宿主注册表、程序与句柄）。多线程宿主（Android JNI 回调来自任意线程）= **每线程一个 `Engine`**，各自注册、各自持有 `Script`；这比每次调用抢锁便宜，且把「共享可变状态」这一整类问题从设计里删掉了。跨线程只传标量（`i64`/`f64`/`bool`）。
- 注册表不重建；宿主二进制不动。
- Android 上源码可来自 assets / 私有目录 / 网络下发，全程纯解释，不触发 W^X。
- 开发期放宽 gas/配额，发布期开启。

### 2.6 性能预期（待 benchmark 验证）

| 档位 | 相对原生 | 说明 |
|---|---|---|
| VM 档 | 3–8× | 目标位；比 Rhai AST 快 5–10× |
| AOT 档 | 1–1.2× | 已冻结（ADR-014） |
| Rhai Grain | 8–20× | 竞品参考 |
| Rhai AST | 20–100× | 竞品参考 |

> 数字为设计目标而非承诺，需用 Criterion 建基线后实测。

---

## 第三章 · 语言规格

### 3.1 设计原则

1. **Rust 语法骨架，脚本心智**：看起来像 Rust，但无所有权、借用、`mut`、生命周期。
2. **全类型推断**：写起来像动态语言，跑起来是静态类型。
3. **能力白名单**：只给「计算 + 决策」，I/O、内存、系统资源一律不给。

### 3.2 最终语规定稿

| 项目 | 决定 |
|---|---|
| 函数定义 | `func 名(参数) -> 返回类型? { ... }`，**返回类型可省略**（编译器推断） |
| 变量 | `let x = ...`，**无分号**，换行即语句结束 |
| 参数类型 | **全推断，不写** |
| 多返回值 | 元组 `(T1, T2)`，用 `.0` `.1` 取值 |
| 多类型返回 | **不支持**（union 撕裂静态检查）；用「元组 + 标志位」代替 |
| 返回类型标注 | 可选：想写就写（当文档+校验），懒得写就省略 |
| 类型种类 | `i64` / `f64` / `bool` / `str` / 句柄 / 元组 / 数组 `[T]`（共 7 种，数组见 ADR-018） |
| 数组 | `[1, 2, 3]` 字面量、`arr[i]` 下标访问、`for x in arr` 遍历、`push(arr, v)` / `len(arr)` / `contains(arr, v)`；**引用语义**（赋值/传参是别名，Python 式）；元素类型编译期唯一（同构），元素可为任意类型含元组 |
| 控制流 | `if / else`、`while`、`for i in 0..n`（range）与 `for x in arr`（数组遍历）、`break` / `continue` |
| 末位表达式 | 即返回值；也支持显式 `return` 提前退出 |
| 运算符 | `+ - * / %`；`== != < <= > >=`；`&& || !` |
| 注释 | `// 行注释`、`/* 块注释 */`（C 风格，不嵌套；跨行块注释等价于一个换行） |
| 函数 | 支持递归、调用宿主注册函数 |

### 3.3 完整语法示例

```rust
// 订单风控规则 —— 宿主启动时注册了 Order 句柄和函数

func discount(amount, vip) {              // 返回类型省略 → 自动推断
    if vip { amount * 0.8 } else { amount }
}

func divide(a, b) -> (f64, bool) {        // 复杂返回值写清楚，更好读
    if b == 0 { return (0.0, false) }
    return (a / b, true)
}

func sum_to(n) {
    let total = 0
    for i in 0..n {
        total = total + i
    }
    total
}

func check(order, limit) {
    let total = order_total(order)        // 宿主函数直接调用
    let vip   = is_vip(order)
    let d     = discount(total, vip)
    let result = divide(d, limit)
    if result.1 { result.0 } else { 0.0 }
}
```

### 3.4 类型系统

- **推断规则**：
  1. 字面量定类型：`42`→i64，`0.8`→f64，`true`→bool，`"x"`→str
  2. 宿主函数签名是种子：`fn(Order)->f64` 让参数与返回值类型确定
  3. 运算向上传播：`amount * 0.8` 中 `0.8` 是 f64，故 `amount` 必须是 f64
- **脚本表达式不做隐式转换**：跨类型用显式转换函数 `to_i64` / `to_f64` / `to_str`；唯一例外是宿主调用边界允许 `i64 → f64` 提升（见 ADR-006），其它不匹配一律报错
- **推断冲突**：如 `x = 42` 后又 `x = "abc"`，加载时报错并定位。可选局部标注 `let x: f64 = 1` 作为逃生舱（非常态）。

### 3.5 标准库（白名单，全部无副作用）

| 类别 | 函数 |
|---|---|
| 数学 | `abs` `min` `max` `sqrt` `floor` `ceil` `pow` |
| 字符串 | `len` `contains` `substr`（`len`/`contains` 同时支持数组：`len(arr)` 元素数、`contains(arr, v)` 元素查找） |
| 数组 | `push(arr, v)` 追加元素（**原地修改**，返回 `()`；白名单里唯一的副作用函数，见 ADR-018） |
| 转换 | `to_i64` `to_f64` `to_str`（`to_str` 仅支持 i64 / f64 / bool / str） |

**没有** print、文件、网络、时间、随机数。脚本做任何事必须通过宿主注册函数——既是安全边界，也保证确定性可复现。

### 3.6 安全语义

- **整数溢出**：按错误终止（非 wrap、非 panic），返回 `Err` 给宿主
- **除零**：返回错误，不产生 inf
- **递归深度 / 循环次数**：VM 栈深上限 + gas 计数，超限立即终止
  - **gas（max_steps）语义**：每条指令扣减一步，`max_steps` 就是指令数预算，默认 10_000_000。`Engine::with_limits(max_steps, max_depth)` 按此解释。曾尝试「只对控制流步计费」以减少检查次数，实测负收益并回退（D-022）——原因是散进各分派臂的条件计费污染分支预测，反而比循环顶部一条无条件检查更慢
- **字符串长度 / 内存**：单字符串与总分配内存均有上限
- **无 panic 保证**：eval 边界用 `catch_unwind`，对外承诺脚本不会 panic 宿主
- **Opaque 句柄**：宿主类型脚本无法构造、无法读字段，只能回传给注册函数

### 3.7 宿主侧 API（已落地）

```rust
use brook::{register_fn, Engine, Opaque, Value};

let mut engine = Engine::new();
// 类型化注册：脚本签名由 Rust 类型推导，回调拿到的是具体类型而非 Box<Value>
register_fn!(engine, "order_total", (Opaque<Order>) -> f64, |o| o.total)?;
register_fn!(engine, "is_vip",      (Opaque<Order>) -> bool, |o| o.vip)?;
// 短形式可省略返回类型；也支持 Result<T, Error> 让宿主函数失败
register_fn!(engine, "clamp", (f64, f64), |x, hi| if x > hi { hi } else { x })?;

let mut script = engine.load_file("rules/risk.bk")?;
let order = Value::from(Opaque::new(Order { /* ... */ }));
let result = engine.call(&script, "check", vec![order, Value::F64(1000.0)])?;
engine.reload(&mut script, "rules/risk.bk")?;           // 热重载，宿主不动

// 字节码缓存：源码已在内存时（Android assets / 网络下发）只读缓存文件
let cached = engine.compile_cached(&src, "<assets>", "rules/risk.sbc")?;
let bytes = engine.script_to_bytes(&cached);            // 可分发 / 内存缓存
let restored = engine.script_from_bytes(&bytes)?;
```

- `register_fn!` 宏按 arity（0–6）展开到 `register_typedN`，底层仍是 slot 直调；原始 raw API `Engine::register_fn(name, params, ret, f)` 保留做逃生舱
- 签名不再可能「写得和回调不一致」：类型检查器、codegen、VM 用的是同一份 Rust 推导结果
- Android JNI 侧另有 C ABI：`ms_call_i64(source, func, arg, out)` / `ms_last_error(buf, len)` / `ms_last_error_line()` / `ms_last_error_col()` / `ms_abi_version()`。**错误协议两步且零跨边界分配**：`ms_call_i64` 返错误码，C 侧再从**调用方自己的 buffer** 取消息、从两个 getter 取 1-based 行列（`0` 表示无位置）。错误码分阶段：`3 = MS_ERR_ENGINE`（编译不过 / 返回值不是 i64 这类契约不符）、`5 = MS_ERR_RUNTIME`（未知函数、参数个数/类型、gas、堆、溢出、除零、宿主回调）。新增的码与 getter 是**增量导出**，老的 C 调用方仍用 `!= MS_OK` 判断失败，故 `ms_abi_version` 保持 1
- **C ABI 句柄式脚本层（ADR-015）**：`ms_script_create` / `ms_script_create_cached`（接线 `compile_cached`，Android assets / 网络下发场景）返回不透明句柄，保留编译产物与源码；`ms_script_call` 以 `MsVal`（tag + i64 + f64 + 字符串 ptr/len 扁平结构）传参，返回标量或字符串（写入调用方 buffer；不足时报 `MS_ERR_BUFFER=6` 并在 `out.len` 带回所需大小供增长重试）；`ms_script_reload` 原地热重载（失败旧字节码原样保留）；`ms_script_free` 释放。**句柄绑定创建线程**——跨线程使用/释放返回 `MS_ERR_THREAD=7` 而非 UB，异线程 free 不取所有权（属主线程仍可释放，杜绝双重释放与竞争）；失败经 `ms_last_report` 提供多行诊断（`-->` + caret + 调用栈）。全部为增量导出，`ms_abi_version` 保持 1；句柄与元组结果不能跨边界（契约错误），单次调用参数上限 8（`MAX_OPERANDS`）
- **错误诊断三级**：`Error::to_string()`（单行，`[type] error at 2:18: ...`，日志与 C ABI 用）→ `Error::report(src, name)` / `Script::report(&err)`（多行：`--> 文件:行:列` + 源码行 + caret + 调用栈）。列号是**字符列**，caret 按显示宽度对齐（tab=4、CJK=2）。运行时错误带调用栈（外层在前，超过 12 帧省略中段）；宿主函数返回的 `ErrorKind` 原样透传，不再压成 `runtime`

### 3.8 语法形式化（核心产生式，待实现时细化）

```
program    := import_decl* func_decl*
import_decl := "import" string            // 仅文件头；编译期载入合并（ADR-017）
func_decl  := "func" ident "(" (param ("," param)*)? ")" ("->" type)? block
stmt       := "let" ident "=" expr | expr | "return" expr?
            | if_stmt | while_stmt | for_stmt | block
expr       := literal | ident | call | unary | expr binop expr
            | "if" expr block "else" block | "(" expr ("," expr)* ")"
call       := ident "(" (expr ("," expr)*)? ")"
```

手工递归下降解析器即可，约几百行，无需 parser generator。

**嵌套深度上限**：块、括号、一元、二元链累计不得超过 `MAX_NESTING = 64`（`parser.rs`），超限报 `nesting too deep (limit 64)` 并带位置。这条限制是**安全边界**而不是语法糖：解析、类型检查、代码生成、乃至 AST 的 `Drop` 全是递归的，深嵌套源码会让进程栈溢出 abort（`catch_unwind` 抓不住）。之所以必须在解析期拦：AST 若已造出来，报错之后析构本身就会再次溢出。代价是 `a+b+c+…` 这类长链按项数计入深度，超出请拆成多条 `let`（见 Q12）。

### 3.9 能力边界（明确画死）

| 层级 | 内容 | 状态 |
|---|---|---|
| 类型基础 | i64 / f64 / bool / str / Opaque 句柄 | v1 核心 |
| 表达式 | 算术、比较、逻辑、let / 赋值 | v1 核心 |
| 控制流 | if/else、while、for range、break/continue | v1 核心 |
| 函数 | 定义、调用、递归、return、元组返回 | v1 核心 |
| 集合 | 数组 `[T]`：字面量 / 下标 / for-in / push / len / contains，引用语义（ADR-018） | v1.2 |
| 宿主互操作 | 注册函数直调、不透明句柄、类型编译期校验、`Vec<T>` 边界往返 | v1 核心 |
| 扩展 | 字典（Map，暂缓——重启条件见 ADR-018）、字符串插值、下标赋值 | v2（含分配开销） |
| 模块复用 | `import "名字"` 约定式合并（无命名空间，见 ADR-017） | v1.1 |
| 永久不支持 | 动态类型、闭包、类/继承、异常、命名空间模块、运行时链接、并发、eval、union 类型 | — |

---

## 第四章 · 实施计划

> 每阶段结束需更新本章进度，并在变更日志记录。

### 阶段 1 · 前端 + AST 解释器（打基础）✅ 已完成 2026-11-20

- [x] 词法分析器（token 定义、数字/字符串/标识符/关键字/运算符）
- [x] 递归下降解析器（产出 AST，含赋值语句、if 表达式、元组）
- [x] 类型检查器 / 推断引擎（含宿主签名种子、冲突检测、默认值收敛）
- [x] AST 解释器（含控制流、递归、gas/栈深限制、溢出与除零错误）
- [x] 宿主 API（register_fn / register_type / Opaque 句柄）+ 热重载
- [x] Criterion benchmark 框架 + 手写 Rust 基线（基线数据见第十章）
- [x] 示例脚本：fib、风控规则；集成测试 8 项 + 词法单测 6 项，全绿
- **交付物**：`brook` crate，14 个测试通过，性能基线已采集
- **注意**：实测 AST 解释器约 765× 慢于原生（fib18: 4.58ms vs 6.0µs），符合预期；阶段 2 VM 需将其压到 3–8×

### 阶段 2 · 寄存器 VM（性能攻坚）✅ 已完成 2026-11-20

- [x] AST → 类型特化字节码降级（`codegen.rs`，寄存器式，非 SSA——见第十章 D-006）
- [x] 类型特化指令集（`ir.rs`：AddI64/AddF64/LtStr/… 编译期定死操作数类型）
- [x] VM 执行器（`vm.rs`：8 字节 Copy 槽位、显式帧栈、扁平寄存器文件 + `base` 偏移；帧池方案已由 ADR-008 取代并删除）
- [x] 宿主函数按 slot id 直调 + 边界类型校验（防推断默认值不匹配静默算错）
- [x] 白名单内置函数类型特化分派（`to_str` 按实参类型选 id；`abs`/`min`/`max` 按实参类型选 i64 / f64 变体，见 Q7）
- [x] 20 个测试全绿（6 单测 + 14 集成，其中 6 项为 VM 专属新增）
- [x] Android arm64 交叉编译通过
- [x] benchmark 采集（fib18 / sum_to200 / compile，数据见第十章）
- [x] 热路径优化第二轮（扁平寄存器文件、局部 ip/code、融合比较分支、立即数加法、紧凑指令）——fib 476µs→211µs，见第十章
- [ ] IR 优化：常量折叠、死代码消除（未做，暂不需要——见 Q8）
- [x] 字节码序列化 / 反序列化 `.sbc` 缓存（已在阶段 3 落地，见第十章末段；含内存级 API 与实测数据）
- **交付物**：VM 档在 fib 场景相对 AST 解释器约 22×（4.58ms → 211µs）；循环场景未采集 AST 基线。相对手写 Rust：fib 27.6× / sum_to 8.6×（见 Q8）

### 阶段 3 · 安全边界 + 宿主互操作 + Android 验证 ✅ 功能完成（真机运行待设备）

- [x] Opaque 句柄机制（`Handle` 携带 Rust 类型名，脚本只能原样回传，无法构造或读字段）
- [x] gas 计数（每条指令扣减，默认 1000 万步；「只按控制流步计费」的优化实测负收益已回退，见第十章 D-022）
- [x] 栈深上限（默认 256 层，解释器递归不增长宿主栈）
- [x] 内存配额（`with_heap_limit`，默认 4 MiB，字符串/元组分配超限即终止）
- [x] 无 panic 保证（`Engine::call` 用 `catch_unwind` 收口；寄存器溢出、参数超过 8 个改为编译错误）
- [x] 热重载（`reload` + 原子替换，正在执行的调用不受影响）
- [x] 交叉编译到 Android arm64-v8a / armeabi-v7a 通过（核心库零平台依赖）
- [x] FFI 注册宏：`register_fn!` + `ScriptType` / `FromValue` / `HostResult` + `Opaque<T>`，签名从 Rust 类型推导，手写 `Vec<Ty>` 不再是必需（raw API 保留）
- [x] `.sbc` 字节码缓存：零依赖二进制格式，源码 hash 校验 + host registry fingerprint 绑定 + 全量边界检查；`script_to_bytes` / `script_from_bytes` / `compile_cached` / `load_cached` / `load_sbc`
- [x] C ABI 边界：`ms_call_i64` / `ms_last_error` / `ms_abi_version`，`catch_unwind` 保证 panic 不跨 `extern "C"`
- [x] `cargo ndk` 打包：arm64-v8a + armeabi-v7a 的 `libbrook.so`（jniLibs 布局），以及两个 ABI 的原生 CLI；ELF 架构与导出符号均已校验
- [x] 真机/模拟器运行验证（2026-11-21 PLR110 通过，见 Q10 / D-032）：smoke test、扩展脚本（递归/循环/字符串/元组）、除零错误定位、gas 止损全部通过；`scripts/android_build.sh` 顺带修复 Git Bash 路径改写问题
- **交付物**：完整可用的宿主集成方案（Rust 侧宏注册、Android 侧 JNI .so 与 C ABI）；自查修复后 63 项测试全绿、release 构建通过、双 ABI 交叉编译通过。**真机运行是唯一未验证项**，不粉饰【2026-11-21 注：已验证，见 Q10】

### 阶段 4 · AOT 后端（已移出路线图 · 冻结为最低优先级备选 —— ADR-014）

> 2026-11-21 起不再作为路线图阶段保留，设计记录见 §2.4。原启动条件备查：出现「外部作者写的规则必须跑在纳秒级热路径且可接受牺牲热重载」的真实需求——届时应先重估 ADR-012 的 JIT 方案（可保留热重载、可回退 VM），而非 AOT。

- [ ] IR → Rust 源码 codegen
- [ ] 独立 crate 生成策略（避免大范围重编）
- [ ] `build.rs` 集成
- [ ] 热点函数 `#[aot]` 标注机制
- [ ] 两档性能对比 benchmark

---

## 第五章 · 技术选型记录

| 项目 | 选择 | 理由 |
|---|---|---|
| 语言实现 | 纯 Rust | 与宿主同源、仅依赖 `std` 且零第三方 crate、Android 交叉编译无障碍 |
| 解析器 | 手写递归下降 | 语法极简，无需生成工具，减少构建依赖 |
| 值表示 | unboxed 栈帧 `Vec<u64>` | 消除装箱/类型 tag，性能关键 |
| 字符串 | 引用计数（`Rc<str>`），无 GC | 极简、可预测、无第三方依赖 |
| 指令集 | 寄存器式 | 比栈式指令数少，减少分发开销 |
| 基准测试 | Criterion | Rust 生态标准，结果可比 |
| AOT 后端（已冻结） | 生成 Rust 源码交 rustc | 不引入 LLVM/Cranelift 依赖，享受 rustc 全部优化（ADR-014：移出路线图，仅备查） |
| 宿主 FFI | `macro_rules!` 类型化注册 | 零依赖、无 proc-macro crate；签名由 Rust 类型推导，杜绝与回调漂移（ADR-009） |
| 字节码缓存 | 手写二进制 `.sbc` | 零依赖、显式字段与全量边界校验；绑定 host registry fingerprint（ADR-010） |
| Android 交付 | `cdylib` + `cargo ndk` | jniLibs 双 ABI `.so` + C ABI，保留 `panic = "unwind"`（ADR-011） |

---

## 第六章 · 竞品对比

| 引擎 | 类型 | 执行方式 | 相对性能 | Android | 与本方案关系 |
|---|---|---|---|---|---|
| 本方案 VM | 静态推断 | 寄存器 VM | 3–8× | ✅ | — |
| 本方案 AOT | 静态推断 | 编译进宿主 | 1–1.2× | ✅（构建期） | 已冻结（ADR-014） |
| Rhai | 动态 | AST 解释 | 20–100× | ✅ | 真正竞品，本方案更快 |
| Rhai Grain | 动态 | 字节码 VM | 8–20× | ✅ | 竞品的 VM 档 |
| Roto | 静态 | Cranelift JIT | 1.2–5× | ❌（JIT 受限） | 非竞品（平台不同） |
| Lua / wasm3 | 动态 | 解释 / 轻量 JIT | 中 | ✅ | FFI 需序列化，非 Rust 原生手感 |

---

## 第七章 · 决策记录（ADR）

> 按编号追加，记录重大取舍及理由。已确定的决策不删除。

### ADR-001 · 放弃 JIT 路线
- **日期**：2026-11-20
- **状态**：已修订（2026-11-20：事实前提证伪；**决策维持 VM 优先**，JIT 改为成本权衡下的候选）
- **决策**：不采用 JIT（含 Cranelift）作为执行后端
- **理由（原始，保留作历史）**：Android W^X + SELinux 使第三方应用无法执行动态生成的机器码；Roto 的 JIT 在本项目平台下不可用
- **影响（原始）**：性能上限锁定在 VM 档 3–8×；换来极简实现、安全与跨平台一致性
- **修订（2026-11-20，证据见 Q11 / ADR-012 草案）**：原「理由」作为绝对命题**是错的**。AOSP 官方 sepolicy 自 2012 年起即有 `allow appdomain self:process execmem;`（commit `ad7df7b`，Stephen Smalley；说明为 "Dalvik Compiler JIT Mapping"，`public/app.te` 注释 "WebView and other application-specific JIT compilers"），当前 main 的 `app_neverallows.te` 全文无 execmem 禁令；带 `neverallow … self:process execmem` 的是 perfetto 等特定系统组件。roto 真机报告在一台 user 版设备（`debuggable=false`、`targetSdk=36`、SELinux enforcing）实测策略 B（`mmap RW` → `mprotect RX`）通过。报告 §9「ColorOS 太宽松、不能外推到 AOSP」的推断方向也反了：策略 B 本就是 AOSP 设计本意，真正依赖 OEM 的是直接 RWX（有策略 B 兜底）。**新表述**：AOSP 默认允许 app JIT，OEM 加固存在差异，需探测 + 回退。**决策不变**：当前仍以 VM 为唯一落地后端；JIT 不再是「平台不可行」被排除，而是「依赖 / 编译时间 / 双后端维护」权衡下暂不上。

### ADR-002 · 双模架构，先做 VM 档
- **日期**：2026-11-20
- **状态**：已确定
- **决策**：架构保留 VM + AOT 双后端，但当前阶段只实现 VM，AOT 留作备选
- **理由**：热重载是默认体验；AOT 仅在真实热点需求出现时才有价值；先做减法符合「极简」目标
- **影响**：阶段 4 可整体跳过，不影响前三个阶段的完整性
- **修订（2026-11-21，ADR-014）**：AOT 从路线图移除、冻结为最低优先级备选——「双模」实际为「单模 VM + 冻结的 AOT 设计记录」。若未来需要解释器之上的性能，优先重估 ADR-012 的 JIT 路线（可保留热重载）而非 AOT

### ADR-003 · 全类型推断，参数不标注，返回类型可选
- **日期**：2026-11-20
- **状态**：已确定
- **决策**：函数参数类型全推断；返回类型标注可选（写则校验，不写则推断）；变量用 `let` 无分号
- **理由**：用户记不住类型名，但希望代码清晰；可选标注调和「省心」与「清晰」两个诉求
- **影响**：推断引擎是编译器核心模块，需重点保证推断冲突时报错准确

### ADR-004 · 不支持 union / 可选值，多返回用元组
- **日期**：2026-11-20
- **状态**：已确定（数组部分 2026-11-21 由 ADR-018 修订）
- **决策**：不支持同一位置返回不同类型；多结果用「元组 + 标志位」表达；不引入 `T?` 可选类型
- **理由**：union 撕裂静态类型检查，损害性能与安全；可选值是额外概念，与极简目标冲突
- **影响**：类型系统固定为 6 种；标准库函数（如 `parse`）需返回 `(f64, bool)` 形态。~~6 种~~ → 7 种（+数组），见 ADR-018

### ADR-018 · 数组进入语言（ADR-004 修订）：Python 式引用语义，Map 暂缓
- **日期**：2026-11-21
- **状态**：已确定（D-045 实施）
- **决策**：
  1. **数组 `[T]` 进入语言**：同构数组（元素类型编译期唯一），元素可以是任何类型**含元组**——`[(str, str, bool, i64)]` 覆盖「数组 of 记录」
  2. **引用语义（Python 式）**：`let b = a` 后 `b` 与 `a` 是同一数组（别名）；函数传参传引用，被调函数内 `push` 对调用者可见
  3. **Map 暂缓**：不是永久拒绝。重启条件写死：① 出现动态键查表真实调用点（`config[用户输入的键]`）；② 记录字段 > 8（元组装不下）；③ 需要键的动态枚举。当前两宿主全量盘点 = 零调用点
- **理由（推翻 ADR-004 原否决的三条证据）**：
  1. **证据**：so_demo 50 脚本（fails/skipped/used/visible/hits 全是 push 积累）+ rhai_demo browser_task_archive（9 个数组变量、`for r in rows` 200 行迭代）——集合操作是脚本的主流形态而非边缘用法；没有数组每一处都要 Opaque+getter / 字符串拼接 / 拆变量，**每个变通的复杂度超过数组特性本身**（D-035 POC 实测）
  2. **GC 障碍消失了**：ADR-004 当年否决的核心技术假设是「引用语义 → 循环引用 → 必须 GC」。本语言**没有闭包、没有引用类型**，循环引用**不可构造**（`push(a, a)` 被类型系统 occurs 检查拒绝——`e == Arr(e)` 是 infinite type）→ 无泄漏 → 不需要 GC → 引用语义 sound
  3. **VM 堆索引模型让引用语义几乎免费**：槽位本来存堆索引，`Mov` 拷贝槽位 = 同一堆对象别名 = 天然 Python 语义；`push` 原地改堆对象即可。参考解释器用 `Value::Arr(Rc<RefCell<Vec<Value>>>)` 对齐同一语义
- **为什么是「数组先、Map 缓」**：rhai 的 Map 无处不在是因为 rhai **没有元组**——Map 是 rhai 对「结构化记录」的替代品。盘点 browser_task_archive 全部 Map 用法：`no_hit()` 占位、`pick_css`/`download_css` 返回值、`r["target_id"]` 字段访问——**全部是固定字段记录，键全是字面量**，元组 + 元组数组 100% 覆盖。真正的动态键关联容器调用点为零。按「每个特性都要有调用点证据」原则（与 D-041 同源），Map 缓
- **代价**：token/lexer `[` `]`；AST `Array`/`DynIndex`/`ForIter`；typecheck `Ty::Arr` + push/len/contains 特判；4 个 opcode（NewArr/ArrPush/ArrLen/ArrGet）+ PUSH builtin；VM 堆 `HeapObj::Arr` + 结构相等；cache `write_ty` tag 9 + **VERSION 4→5**；host `Vec<T>` 边界；eval.rs 同步。约 +900 行含测试
- **明确不做（v2 候选，出现调用点再议）**：`arr[i] = x` 下标赋值、数组 `==`（与元组同策略拒绝）、`pop/insert/remove`、数组插值/解构
- **C ABI**：数组不能跨 C 边界（`MsVal` 是扁平结构，v1 明确报错）；Rust 宿主边界 `Vec<T>` 自由往返

### ADR-005 · for 循环纳入 v1
- **日期**：2026-11-20
- **状态**：已确认（Q1 解决）
- **决策**：`for i in 0..n` 放入 v1
- **理由**：属常规脚本核心功能，缺了写循环啰嗦；实现成本可控
- **影响**：阶段 1 已实现并通过测试

### ADR-006 · 类型推断边界的务实处理
- **日期**：2026-11-20
- **状态**：已确定（阶段 1 实现）
- **背景**：纯脚本内推断看不到宿主调用点的实参类型。`func add(a,b){a+b}` 的参数无法唯一确定是 i64 还是 f64；`func first(p){p.0}` 的 p 无法知道是元组。
- **决策**：
  1. 未约束变量收敛为默认类型 `i64`
  2. `.N` 索引操作反向推断 base 为至少 N+1 元的元组
  3. 需要其他类型时用可选标注（逃生舱）
- **理由**：在不牺牲"无标注"体验的前提下覆盖绝大多数脚本；宿主可调用的函数签名由实参在运行时 coerce
- **影响**：`add(a,b)` From调用宿主传 f64 时会被 coerce 为 i64——这是已知限制（Q6）。阶段 2 在宿主调用边界增加类型校验：默认值与实参不符时明确报错并提示加标注，杜绝静默算错

### ADR-007 · 类型特化寄存器 VM（阶段 2 核心架构）
- **日期**：2026-11-20
- **状态**：已实现
- **背景**：阶段 1 AST 解释器 765× 慢于原生（fib18: 4.58ms vs 6µs），三大瓶颈是 boxed Value、HashMap 作用域查找、递归 ast 遍历。
- **决策**：
  1. 编译期把变量名映射到寄存器号，运行时零名称查找
  2. 用类型检查的 resolved signature 把每个运算定死为 `AddI64`/`AddF64`/`LtStr` 等特化 opcode，运行时零 tag 判断
  3. 8 字节 `Copy` 槽位（标量按位存，堆对象存索引），零 clone
  4. 显式帧栈（该 ADR 原文为帧池复用；该方案后被 ADR-008 的扁平寄存器文件 + `base` 偏移取代）
  5. 保留 `eval.rs` 作为参考实现，对外 API 零改动
- **理由**：Android 禁用 JIT，解释器要做到接近原生只能靠消除一切运行时间接开销；类型特化是最大头【2026-11-20 注：此前提已由 ADR-001 修订推翻，见 Q11；类型特化本身的收益结论不受影响】
- **影响**：相比 AST 解释器提速 6–16×（fib 765×→67×，循环场景 30×）；未达 3–8× 目标，原因是纯解释 VM 的架构税仍在——要突破需 JIT/AOT，而 JIT 被 Android 排除、AOT 牺牲热重载（见第二章）。此为该约束组合下的合理落点【2026-11-20 注：「JIT 被 Android 排除」已证伪，见 ADR-001 修订 / Q11】

### ADR-008 · 热路径第二轮优化：扁平寄存器 + 融合分支（取代帧池方案）
- **日期**：2026-11-20
- **状态**：已实现
- **背景**：ADR-007 的帧池方案把 fib 从 4.58ms 做到 476µs（67×），但剖析显示剩余开销不在算术，而在**每步的间接寻址与指令拷贝**：`self.frames.last().unwrap().regs[r]` 每次读写都要过 Vec 尾指针 + 边界检查；`Op` 24 字节 + `Span` 32 字节 = 每条指令拷贝 56 字节；循环条件还要多一次"比较→落寄存器→再分派跳转"。
- **决策**：
  1. **扁平寄存器文件**：所有帧共用 `Vec<Slot>`，帧只记 `base` 偏移；寄存器访问退化为一次索引，帧分配是偏移自增，帧池被删除
  2. **局部化 `ip` / `base` / `code`**：调度循环把它们放在局部变量，仅在 call/ret 时写回帧或重载
  3. **融合比较分支**：`JmpIf{Lt,Le,Gt,Ge,Eq,Ne}{I64,F64}` 12 条指令；codegen 把 `if/while/for` 的条件比较直接改写成"跳转当谓词成立"，比较结果不再落寄存器
  4. **立即数加法**：`AddI64Imm`，`for` 自增从 3 条指令（const/add/mov）压到 1 条；`x ± 1` 折叠为立即数
  5. **紧凑指令**：`Instr` 的位置信息改为 `line: u32 + col: u32`（原为 4×usize 的 `Span`），单步拷贝从 56 字节降到 32 字节
- **理由**：解释器的成本几乎全在"每执行一条指令的固定开销"上，因此优化的正确方向不是少算，而是少分派、少拷贝、少间接
- **影响**：fib 476µs→211µs（2.25×，67×→27.6×），循环 8.45µs→2.41µs（3.5×，30×→8.6×）。**循环类工作负载已进入 3–8× 目标区间的边缘**；纯递归仍偏高，因为每次调用都要真实压帧，无内联空间——这是解释器架构税的残留，只能靠 JIT/AOT 消除

### ADR-009 · 类型化 FFI 注册用 `macro_rules!`，不引入 proc-macro
- **日期**：2026-11-20
- **状态**：已实现
- **决策**：新增 `host.rs`（`ScriptType` / `FromValue` / `HostResult` / `Opaque<T>`）+ `Engine::register_typed0..6` + 导出的 `register_fn!` 宏；签名由 Rust 类型推导，原始 raw API `register_fn(name, Vec<Ty>, Ty, f)` 保留为逃生舱
- **理由**：手写 `Vec<Ty>` 与回调签名漂移是 D-008、D-011 两次静默错误的共同根因；让类型系统成为唯一真相来源才能根治。按 arity 生成方法是因为 Rust 没有稳定的「把 tuple 展开成闭包实参」语法；`macro_rules!` 足够表达，且不需 proc-macro crate、不改构建依赖、保持零第三方依赖
- **影响**：支持 0–6 参数、标量 / `String` / `Rc<str>` / `&'static str`（仅返回） / `()` / 二元组 / `Opaque<T>` / `Result<T, Error>`；宏返回 `Result<()>`，注册失败不会被静默忽略；同名宿主函数重复注册现在直接返回 `Err`；`Opaque<T>` 在边界校验类型名与 downcast，回调内 `Deref` 取 `&T` 不会再 panic

### ADR-010 · `.sbc` 缓存落地，但不作为默认「磁盘加速」路径
- **日期**：2026-11-20
- **状态**：已实现（Q9 结论同步更新）
- **决策**：手写零依赖二进制格式（magic `VTBC` + version + 源码 FNV-1a hash + host registry fingerprint + 完整字节码/签名表/常量池）；所有字段显式定长编码，当前 71 个 opcode（v1 为 65，⑤ 立即数分支后扩到 71）由**同一张宏表**生成 encode/decode，杜绝 tag 漂移；解码全程边界检查，损坏文件返回 `Err` 而非 panic
- **理由**：引入 serde/derive 会破坏「零第三方依赖」；更关键的是 `CallHost` 指令把注册表 slot id 编进字节码，缓存跨注册表复用会**静默调错宿主函数**，所以 fingerprint 必须逐项比对（名字 + id + 签名 + 集合）
- **实测**（Criterion quick，10 samples，release）：内存 `script_from_bytes` **1.52 µs** vs 内存 `load_str` 编译 **7.96 µs**（5.2×）；但磁盘路径 `load_file` 97 µs、`load_sbc` 83 µs、`load_cached` **164 µs**（读源码 + 读缓存两次 I/O）。**结论：Windows 上文件 I/O 主导，磁盘缓存不是性能特性**；它的价值是「源码已在内存时跳过编译」与「字节码作为独立可分发交付物」
- **影响**：热路径推荐 `compile_cached(src, name, cache_path)`（Android assets / 网络下发，只读一个缓存文件）或 `script_from_bytes`（纯内存）；`load_cached` 定位为便捷 API 并在文档中注明代价。不粉饰数据

### ADR-011 · Android 交付形态：C ABI + `cargo ndk` 双 ABI `.so`
- **日期**：2026-11-20
- **状态**：已实现（真机运行待设备）
- **决策**：`Cargo.toml` 增 `cdylib`；`ffi.rs` 导出最小 C ABI（`ms_call_i64` / `ms_last_error` / `ms_abi_version`，无 I/O、无宿主回调、全部 `catch_unwind` 收口）；`cargo ndk -t arm64-v8a -t armeabi-v7a -o jniLibs` 产出 `.so`；`examples/android_cli.rs` 为原生 smoke-test 可执行文件，`scripts/android_build.sh` 一键构建并在有设备时 push + 运行
- **理由**：JNI 需要真实可加载的 `.so` 与稳定符号；纯解释库零平台依赖，Android 禁用 JIT 的约束不受影响【2026-11-20 注：此前提已由 ADR-001 修订推翻，见 Q11；C ABI 交付形态本身不变】；C ABI 保持极小面，避免把宿主回调/句柄跨过 FFI 边界扩大攻击面
- **影响**：产物 ELF 校验通过（AArch64 / ARM，PIE 与 shared object 均正确），`ms_*` 符号可见；本机 `adb devices` 为空且 SDK 无 emulator/AVD，**真机运行未验证**，脚本保留一键验证路径。`panic = "unwind"` 仍是 `Engine::call` 与 FFI 边界共同前提

### ADR-012 · 重估 JIT 路线（草案，待决策）
- **日期**：2026-11-20
- **状态**：草案，**未采纳**；当前落地后端仍只有 VM
- **背景**：ADR-001 排除 JIT 的唯一理由是「Android 不可行」，该事实前提已被 AOSP 源码与真机报告证伪（Q11）。成本/收益账本随之翻转：包体净增 +1.81 MiB/ABI（可忽略）；fib(20) 9.6×、sum_to(200) 13.2×；JIT 完整编译 721 µs，分别 2 次 / 445 次调用回本。
- **候选方案**：VM 保持默认与回退路径；增加可选 JIT 后端——启动时探测 execmem（`mmap RW` → `mprotect RX`；失败再试 `mmap RWX`；两者皆败则纯 VM），探测通过且脚本被判定为热点时走 Cranelift JIT。脚本 reload 只需重新编译，不破坏「热重载」卖点。
- **未决成本**：Cranelift + 十余子 crate 依赖（BSD、纯 Rust）；Android 交叉编译 +17 分钟（thin LTO）；双后端测试面；OEM 加固差异下的探测兜底；ARM 真机的编译速度与性能未测；JIT 自有运行时代码生成带来的攻击面需要重新评估安全边界。
- **触发条件**：真实业务的递归/循环脚本延迟预算无法由 VM 满足时启动原型；否则维持现状。
- **验证清单**：① ARM 真机 execmem 探测矩阵（含不同 OEM / targetSdk）；② 冷启动耗时与峰值内存；③ 非 fib 的业务脚本收益；④ JIT → VM 回退路径正确性与 panic 边界。

### ADR-013 · 从 rhai 迁移的可行性：宿主适配可行，语言缺口是边界
- **修订（2026-11-21，ADR-017）**：本 ADR 记录的「`import` 触碰引擎」开放项**已解除**——约定式 import 落地（见 ADR-017）。rhai_demo 的 5 个 `import "common" as lib` 脚本迁移路径：common.rhai → common.bk，`lib::函数` 调用改为直接调用（无命名空间，撞名编译报错兜底）。
- **日期**：2026-11-20
- **状态**：已验证（概念验证，非生产迁移）
- **背景**：存在用本项目替换 `rhai_demo`（34 个脚本、约 208 个 `register_fn`、大量 Map/Array 返回）的诉求。问题被界定为「宿主能力不变，脚本可改，能否完全迁移」。
- **验证方法**：`tests/migration_browser_pick.rs` 把最难的 `browser_pick.rhai`（同时踩中 Map 字段、方法调用、动态数组、try/catch、错误对象五类缺口）按 A 方案改写为 `.bk`：mock 浏览器能力不变，只改注册层，期望值照抄原脚本 12 组断言。全绿（失败列表长度 0），release 全量测试无回归。
- **结论**：
  - ✅ 不动引擎内核即可覆盖五个缺口：Map→`Opaque<Pick>` + 访问函数；方法调用→普通函数/内置；动态数组→`Opaque<StrList>` + `list_*`；try/catch 错误对象→错误码塞进 `err` 字段；顶层语句→包进 `main()`。
  - ⚠️ 实测两条静态类型代价：① 宿主返回值仅支持到 **2 元组**，3 元组需改用 `Opaque` 承载；② 脚本函数参数类型必须固定，rhai 的多态 `check(got,want)` 需统一 `to_str` 后按字符串比较，或加 `: str` 标注。
  - ❌ 真正无法靠注册层绕过的是 `break/continue` 与 `import` 模块：前者要改写控制流或给引擎加两个关键字（小改），后者靠宿主预拼接 lib 源码规避；若要原生支持则触碰引擎。
- **工作量实测**：原 rhai 脚本 187 行 → 改写 .bk 95 行；宿主侧新增 24 个注册函数（访问器 + 列表适配器 + mock）。注意这是**单个最难脚本 + mock 宿主**的量，全量 34 脚本的真实成本远高于此。
- **决策**：不新增 Map/List/方法调用语法（会摧毁极简定位）；迁移走「宿主适配层 + 脚本改写」。是否整套迁移属业务决策，本 ADR 只证明技术可行性与边界。
- **修订（2026-11-21，D-035）**：本 ADR 记录的两条静态类型代价之一——「宿主返回值仅支持到 2 元组」——已被 host.rs 桥接层扩展解除（`impl_tuple_n!` 覆盖 2–8 元；语言本身一直支持 N 元组，缺口在桥接层）。变长记录集仍走 Opaque + count/indexed getter。rhai_demo 与 android_so_demo 的全量暴露函数清单与分层可行性评估见 `docs/migration/`，so_demo 两个真实脚本（t_wifi、bind_verify）的忠实改写 POC 见 `tests/migration_so_demo.rs`。

### ADR-014 · AOT 移出路线图，冻结为最低优先级备选
- **日期**：2026-11-21
- **状态**：已确定（ADR-002 随之修订）
- **决策**：阶段 4（AOT 后端）从活跃路线图中移除，降为冻结的最低优先级选项；§2.4 的设计记录与原启动条件保留备查，不删除。
- **理由**：
  1. **AOT 与热重载物理互斥**（编译进宿主就要重编译），而热重载是本项目的默认卖点——这是 ADR-002 立论时就存在的根本张力
  2. **JIT 账本翻转后 AOT 失去独特优势**（Q11 / ADR-012：fib 9.6× / sum_to 13.2×、冷启动 2 次调用回本、包体 +1.81 MiB/ABI 可忽略）。AOT 原有的「不引入重依赖」卖点，在「依赖/包体不可接受」这一反 JIT 论据被证伪后不再成立；且 JIT 可保留热重载、可回退 VM，全面优于 AOT——未来若真需要解释器之上的性能，应先重估 JIT 而非 AOT
  3. **无真实需求触发**：159 项测试 + 真机验证收官（D-032），循环类工作负载已进 3–8× 目标区间（Q8），AOT 的启动条件（外部作者规则要纳秒级）从未出现
- **影响**：第四章不再保留活跃的「阶段 4」；§1.1 定位改为单模 VM、§2.1 架构图去掉 AOT 分叉、§2.6/§5/§6 表格 AOT 行标注冻结；「永久不做」边界不变——重新评估的前提仍是 ADR-014 的触发条件，且顺序上先 JIT 后 AOT

### ADR-015 · C ABI 增加句柄式脚本层（保留编译产物、MsVal 调用、热重载、多行 report）
- **日期**：2026-11-21
- **状态**：已实现（宿主 166 项测试 + PLR110 真机验证通过）
- **背景**：`ms_call_i64` 每次调用都重新编译整个脚本（smoke-test 形态），真实 Android 宿主需要保留编译产物；`compile_cached` 的 assets/网络下发路径没有 C 入口；错误诊断止步单行消息，`Script::report` 的多行能力（--> + caret + 调用栈）出不了 C 边界。
- **决策**：增量导出六个新符号——`ms_script_create` / `ms_script_create_cached` / `ms_script_call` / `ms_script_reload` / `ms_script_free` / `ms_last_report`——加 `MsVal`/`MsValOut` 两个 `repr(C)` 结构与两个新错误码（`MS_ERR_BUFFER=6`、`MS_ERR_THREAD=7`）。`ms_abi_version` 保持 1（纯增量导出，只测 `!= MS_OK` 的老调用方不受影响）。
- **设计要点**：
  1. `MsVal` 用**扁平 struct**（tag + i64 + f64 + ptr）而非 union——Java/JNI 侧可直接构造；字符串实参以 `(ptr, len)` 传入（不要求 NUL 结尾，`byte[]` 直接映射），字符串结果写入调用方 buffer（NUL 结尾），不足时报 `MS_ERR_BUFFER` 并在 `out.len` 带回所需大小，调用方增长重试——与错误协议同款两步式
  2. **`MsScript` 线程归属**：记录创建线程 id，所有入口校验。引擎 `Rc` 承载、刻意 `!Send` 是设计规则（见 threading 语义），C 侧把误用从 UB 变成干净错误；异线程 free **不取所有权**（分配保持有效，属主线程仍可释放），杜绝双重释放与数据竞争
  3. 句柄保留源码文本：运行时错误经 `Script::report` 渲染多行诊断；一次性 `ms_call_i64` 也顺手接上（source 就在手边）
  4. **句柄不跨 C ABI 边界**（与 ADR-011 的最小攻击面一致）；句柄/元组结果返回 `MS_ERR_ENGINE` 契约错误；单次调用参数上限 8（`MAX_OPERANDS`）
  5. i64 实参向 f64 形参自动提升，与 Rust API 的 `coerce` 语义一致
- **测试**：宿主新增 7 项 FFI 测试（生命周期/热重载/失败重载保留旧字节码、类型矩阵、字符串跨边界含 CJK 与 buffer 重试、create_cached 三次往返含失效重建、错误与 report 渲染、契约与 null 矩阵、线程归属）；`android_cli` 扩展句柄段，PLR110 真机全过（缓存落盘 806 字节、二次运行命中、多参数、字符串重试、错误路径、reload 后 add3=7、free/null-free）
- **影响**：ffi.rs / tests/ffi.rs / examples/android_cli.rs / §3.7；无现有 API 或 `.sbc` 格式变化

### ADR-016 · 顶层只允许函数定义（不做顶层语句流）
- **日期**：2026-11-21
- **状态**：已确定（阶段 1 起的隐式选择，本 ADR 显式化并补全论证）
- **决策**：`program := func_decl*`——顶层不接受任何语句（let / 调用 / 表达式），入口由宿主按名调用（名字由宿主任意约定，习惯 `main`）。
- **理由**：
  1. **调度权归宿主**：嵌入引擎的价值就是宿主能调度。函数入口 = 一份脚本是一组能力（宿主按触发源挑函数、带参、反复调）；语句流 = 执行退化为"从头跑一遍"，调度逻辑反向耦合进每个脚本（靠读触发上下文在顶层分支）
  2. **热重载语义干净**：重载 = 换函数集合，「执行到一半的状态」概念不存在（每次 call 全新 VM）；语句流的重载语义（丢进度或协程级复杂度）在常驻型顶层脚本下必然撞墙
  3. **可复现性**：每次 call 全新实例 + 全新 gas，第 999 次与第 1 次一致——自动化任务硬需求；语句流 + 持久 scope 存在顶层状态累积
  4. **安全模型一句话**：gas/深度/配额全部按调用计费；语句流的长驻脚本把预算语义变成脆弱假设（rhai_demo pdf_watch 明文依赖"2 百万步挂 50 小时"，inventory 证据）
  5. **并发**：无顶层共享可变状态 → per-thread Engine 的并发故事成立；语句流顶层变量即跨线程共享状态
  6. **全线简化**：`program := func_decl*` 一条文法 → 类型检查 / `.sbc` / validate / 缓存全部以函数集合为单位
- **代价**：每脚本需 main 包裹（rhai 迁移机械成本，34 个脚本）；无顶层常量（替代：宿主零参函数，so_demo 9 颜色常量 → 9 个函数）；ad-hoc 手感弱于 Python
- **关键洞察（糖的方向性）**：语句流可以从函数入口模型**廉价模拟**（宿主把顶层语句包进 main，迁移 POC 实证）；反之函数入口无法从语句流引擎廉价长出（需新增按名调用机制）。选下位原语，糖留给宿主
- **证据**：rhai_demo 34 脚本全为任务型；其 REPL 为命令式（非表达式求值）；so_demo 的 ARGS 注入 50 脚本 0 使用——顶层流的伴生需求实际为零
- **影响**：checker 重复定义报错、`.sbc` 格式、reload 语义、per-thread 并发模型均以此为基础；文档表述见 TUTORIAL.md §1（两条结构规则）

### ADR-017 · 约定式 import：脚本级公共函数复用
- **日期**：2026-11-21
- **状态**：已实现（`tests/import_stmt.rs` 11 项 + 全量回归）
- **背景**：ADR-013 将 `import` 记录为「触碰引擎的开放项」；D-041 暴露其反面——没有该能力时教程无法教函数复用，且用户实测"如果你的宿主提供了"假设句式造成半天误导。用户定性：迁移前落地、**脚本作者零配置**（像 Go 同目录那样，放个文件就能用）。
- **决策**：
  1. **语法**：`import "名字"` 仅文件头（所有 func 之前）；引入的函数进全局函数集，**无命名空间**（直接 `retry(3)`，不写 `common.retry(3)`——撞名编译报错兜底）
  2. **解析（约定式）**：当前脚本所在目录的 `<名字>.bk`；子路径允许（`import "lib/net"` → `lib/net.bk`，相对**导入者**目录）；`..` / 绝对路径 / `\` / 盘符全部编译期禁止（永远锁在脚本目录树内——与 so_demo fs 护栏同一哲学）
  3. **逃生舱**：`set_import_dir`（便捷式）与 `set_import_resolver`（回调，assets/DB/加密源码）覆盖特殊部署；未配置时 load_str 里的 import 明确报错（D-041 规则：不存在就说不存在）
  4. **实现**：文本拼接进主流（各单元按拼接视图行号偏移解析，`Lexer::new_with_offset`）→ checker/codegen/cache 全线复用；loaded/stack 双集合（重复 import 幂等去重 vs A→B→A 真循环报错）；上限：单次编译 ≤32 单元、链深 ≤8、单份 ≤1 MiB
  5. **缓存**：`.sbc` v4 追加依赖清单（解析路径 + hash），`load_cached`/`compile_cached` 加载时逐一校验——任一依赖改一字，依赖脚本缓存自动失效重建
- **为什么不做命名空间 / 运行时链接**（记录，非永久拒绝）：命名空间要动 parser/checker/codegen 三层，收益只是名字前缀——全局集 + 撞名报错覆盖需求（so_demo 50 脚本实测撞名极少）；运行时链接省下的重编译以微秒计，跨单元签名解析/调用号空间/跨单元报错以天计。等真实需求触发再议
- **代价**：parser +30 行 / loader +120 行 / cache +40 行；`.sbc` VERSION 3→4（追加依赖清单段）；教程 §4.8 新章节
- **证据**：`tests/import_stmt.rs` 11 项（同目录/子路径/相对导入者解析/顺序无关/重复幂等/循环报错/撞名报错/路径护栏/load_str 报错/resolver 逃生舱/缓存失效/热重载）+ 全量回归
- **影响**：parser.rs / lexer.rs / token.rs / ast.rs / engine.rs / cache.rs（VERSION 4）/ 文档；C ABI 无变化；**与 D-041 的关系**：能力落地后 §4.8 才成为合法章节——D-041 的「不写不存在的能力」规则持续有效
- **修订（2026-11-21，D-044 加固）**：① deps 清单追加 is_file 标记（resolver 依赖无路径可校验——原 deps_fresh 按路径读盘必然失败导致缓存永不命中，真 bug）；② 逃逸防护（canonicalize 包含性检查防 symlink/junction 出树）；③ Windows 保留设备名 / 控制字符 / 组件尾点尾空格拒绝；④ cache decode 拒绝 `..` 组件依赖与坏 flag（防恶意 .sbc 借校验探测路径）

---

## 第八章 · 开放问题

> 待解决的问题。解决后移至变更日志，并在此标注「已解决 + 日期」。

| 编号 | 问题 | 状态 | 备注 |
|---|---|---|---|
| Q1 | `for i in 0..n` 放 v1 还是 v2？ | 已解决（v1） | 阶段 1 已实现并测试通过 |
| Q2 | 字符串仅 `+` 拼接是否满足需求（不做插值模板）？ | 已解决（仅 +） | 阶段 1 按此实现 |
| Q3 | 整数溢出按错误终止是否符合使用场景？ | 已解决（错误终止） | 阶段 1 按此实现，见第十章 D-003 |
| Q4 | 项目代号定名 | 已解决 | 初版定名 `miniscript`；2026-11-21 两轮更名现为 `brook`（用户昵称「小河」的英译，D-040 / D-043） |
| Q5 | 是否引入可选值 `T?` | 倾向否 | 见 ADR-004，除非出现强需求 |
| Q6 | 全推断边界：宿主调用点的参数类型脚本内不可见 | 部分缓解 | 默认 I64 收敛 + 索引反推 + 边界类型校验（不匹配明确报错，见 D-008）；根治需"宿主声明脚本函数签名"。2026-11-21：索引反推补齐**续推**语义——已部分推断成短元组的变量遇到更大索引时扩长而非误报越界（D-035，此前 `func f(t) { t.0 + t.1 }` 编译失败） |
| Q7 | 内置数学函数 `abs/min/max` 当前仅接受 f64，i64 需 `to_f64` 转换 | 已解决 | `abs`/`min`/`max` 改为数值泛型：实参全 i64 则结果 i64（`ir::builtin` 追加 16/17/18 三个 id，`numeric_id` 在 codegen 选变体，**不加 opcode、不动 `.sbc` VERSION**）；`abs(i64::MIN)` 走 `checked_abs` 报 `i64 abs overflow` 而不是 Rust panic。`sqrt/floor/ceil/pow` **保持仅 f64**——整数开方没有精确 i64 结果，静默截断比报错更糟 |
| Q8 | 性能与 3–8× 目标：当前 fib 27.6× / 循环 8.6×（优化后） | 循环达标，递归仍有差距 | 两轮优化把 fib 从 765×→211µs（27.6×）。循环类工作负载已进入目标区间；纯递归是解释器最不利场景（无内联空间）。进一步突破只剩 JIT 一条路（Q11 / ADR-012 草案）；AOT 已按 ADR-014 冻结移出——牺牲热重载，且 JIT 账本翻转后不再有独特优势 |
| Q9 | IR 优化（常量折叠 / 死代码消除）、字节码 `.sbc` 缓存 | 已解决（结论见 ADR-010） | `.sbc` 已落地：内存解码 1.52µs vs 编译 7.96µs（5.2×），但磁盘路径受文件 I/O 主导无优势，故不作默认加速；IR 优化仍推迟（融合分支/立即数加法已吃掉大部分收益） |
| Q10 | Android 真机 / 模拟器运行验证 | 已解决（2026-11-21 真机） | PLR110 真机（Android 16、arm64-v8a、SELinux Enforcing）四项实测通过：默认 smoke test (160, true)、扩展脚本（递归/循环/字符串/元组，(5254, true)）、除零错误带定位与调用栈、gas 上限止损无限循环；双 ABI .so 重建后 ELF/导出符号复验通过。顺带修复 android_build.sh 在 Git Bash 下 push 被 MSYS 路径改写的问题 |
| Q11 | 是否重估 JIT 后端（AOSP 可行性 / 包体 / 性能 / 冷启动） | 待决策（证据已齐，倾向做原型） | 五轮证据：① AOSP sepolicy 允许 `appdomain` execmem、roto 真机报告策略 B 通过；② Cranelift 受控实验 strip 后净增 **+1.81 MiB/ABI**（6.4×），可忽略；③ fib(20) JIT 42.8 µs vs VM 411.5 µs = **9.6×**；④ JIT 完整编译 721 µs vs VM 编译 8.6 µs / 缓存解码 1.8 µs，fib **2 次**调用回本（含 Runtime 初始化 3.2 次）；⑤ sum_to(200) JIT 131 ns vs VM 1.73 µs = **13.2×**，**445 次**回本（含初始化 740 次）。剩余成本：Cranelift 依赖、Android 编译 +17 分钟、双后端维护、execmem 探测 + VM 回退、ARM 真机未测。草案见 ADR-012。 |
| Q12 | 嵌套上限 64 是否会卡住机器生成的长公式（如 60+ 项评分式） | 已解决（暂定 64） | 加限制前该场景是**进程崩溃**（栈溢出），现在变成清晰报错，属于净收益。若后续确有 `a+b+…` 长链需求，正确解法是把 typecheck / codegen 对左深二元链的处理改为显式栈（不动 `Drop`，仍需约 2k 的链长上限），可把上限抬到数千；当前不做，避免为一个未出现的需求引入三处递归改写 |
| Q13 | eval.rs 参考解释器与 VM 行为分歧：`==`/`!=`/排序对 bool / str / 句柄落入 `as_f64` 报错（VM 有 EqBool/EqStr 等正确实现）；宿主错误 `ErrorKind` 被压平为 runtime（VM 侧已按 D-028 透传） | 已解决（2026-11-21） | eval_binary 补 bool / 句柄 / 字符串三个分派臂（实现时发现字符串 `+` 拼接同样落入浮点路径，范围比审查记录更大）；eval_call 用 `Error::new(e.kind, …)` 透传宿主错误类型；tests/eval_parity.rs 4 项把两引擎钉在同一语义，实测全绿 |
| Q14 | typecheck 与 codegen 对「无 else 的 if 语句 / 末位赋值语句」的块值类型分歧（typecheck 记 Unit，codegen 传播分支/变量类型），当前被 sig 边界掩盖为静默丢值（`func f(c) { if c { 1 } }` 返回 Unit）；另有 cache.rs `validate_wire` 严格度低于 validate.rs（off-by-one 被第二道校验兜住）、validate 不校验 `CallScript` 实参个数（损坏 .sbc 走 catch_unwind 报 internal panic 而非干净消息） | 已解决（2026-11-21） | codegen 统一为 Unit（对齐 checker / Rust 语义；边界行为零变化，消除 `let x = { … }` 场景的 codegen env 类型漂移）；validate 校验 CallScript 实参**不超过被调窗口**（结构安全界，防 argv 拷贝越界 panic；语义 arity 留给 checker——首轮误用 `== params.len()` 被 cache.rs 的 opcode roundtrip 测试抓回后修正）；validate_wire 收紧至与 validate.rs 同级（register 用 >=、跳转 target 严格 <）。实测全绿（含修正后的 validate.rs 4 项） |
| Q15 | 语言语义毛边：① 内置名遮蔽——`func abs(x) {…}` 可通过编译，但脚本内部调用命中内置实现，只有作为入口点被宿主调用才走用户函数（与宿主同名直接报错不对称）；② `()` 字面量产生 `Ty::Tuple([])` 而非 `Ty::Unit`，两者渲染同为 `()` 但互不 unify | 已解决（2026-11-21） | ① typecheck Pass 1 增加内置名保留检查（措辞 "is a reserved built-in name"，与宿主碰撞的 "already defined by the host" 区分）；② `()` 字面量在 typecheck / codegen / eval / `parse_type` 四处统一归一为 Unit，`.sbc` VERSION 2→3 强制重建缓存（wire 格式未变，防旧缓存以 `Tuple([])` 语义存活）；新增 tests/language_semantics.rs 3 项，全量 173 项实测无存量脚本依赖旧语义 |
| Q16 | 是否给语言加 Map/Array（用户 2026-11-21 提议：它们是基础设施而非单个功能，没有就要东拼西凑甚至让宿主配合） | 已解决（2026-11-21，ADR-018） | **数组进入语言**（v1.2，Python 式引用语义——VM 堆索引模型天然支持别名，无闭包 → 循环引用不可构造 → 无需 GC，ADR-004 的否决前提失效）；**Map 暂缓**——盘点 browser_task_archive 全部 Map 用法均为固定字段记录（键全是字面量），元组 + 元组数组 100% 覆盖，动态键查表调用点为零；重启条件写进 ADR-018（动态键 / 字段 > 8 / 键枚举） |

---

## 第九章 · 变更日志

> 倒序追加。格式：`日期 · 变更内容 · 影响范围`

- **2026-11-21** · **数组进入语言（ADR-018 / D-045，Q16 解决）**：决策链——用户提出 Map/Array 是基础设施而非单个功能（"没有就要东拼西凑甚至让宿主配合"）；盘点两宿主全量脚本后定性：**数组必须加**（so_demo 50 脚本 push/for-in 无处不在 + browser_task_archive 9 个数组变量），**Map 暂缓**（browser_task_archive 全部 Map 用法实为固定字段记录、键全是字面量，元组 + 元组数组 100% 覆盖，动态键调用点为零；重启条件写入 ADR-018）。语义：**Python 式引用**（赋值/传参是别名）——VM 堆索引模型下 `Mov` 拷贝槽位即别名，几乎零成本；循环引用不可构造（`push(a, a)` 是 infinite type 被 occurs 检查拒绝）→ 不需要 GC → ADR-004 当年否决的前提失效。实现：token/lexer `[` `]`；AST `Array`/`DynIndex`/`ForIter`（`for x in arr` 与 range 泛化并存）；typecheck `Ty::Arr` + push/len/contains 元素类型特判（**len/contains 对未约束变量保持旧行为钉 str**——否则 `func f(s){len(s)}` 参数被钉成 [i64]、宿主传 str 被边界拒绝，回归被 tutorial_examples 抓回）；4 opcode（NewArr/ArrPush/ArrLen/ArrGet，cache wire tag 72–75）+ PUSH builtin（id 19，白名单唯一副作用函数）+ `HeapObj::Arr` 堆对象 + push 逐元素计费（配额先查后分配）+ slots_equal 结构相等；cache `write_ty` tag 9 + **VERSION 4→5**；host `Vec<T>` 三件套（ScriptType/FromValue/HostResult，边界=快照）+ `type_matches` 数组递归对账 + C ABI 明确拒绝数组结果；eval.rs 参考解释器同步（`Value::Arr(Rc<RefCell<Vec<Value>>>)` 共享 payload，两引擎别名语义一致）。测试：新增 tests/arrays.rs 18 项（双引擎同断言防分叉：字面量/下标/len/for-in/push 推断/别名/传参引用/元组数组/嵌套/越界/编译期错误/`push(a,a)` infinite type/配额/宿主 Vec 返回与接收/缓存往返/迭代中 push）；存量 type_errors 1 项更新（len 多态化后报错措辞 `len expects …, found i64`，期望在前方向不变）。**240 项全绿、release 零警告**，基线 11 位基准无回退（fib 129µs / 循环 1.25µs / compile (memory) 28µs 与 D-044 水位一致）· 第三章 / 第八章 Q16 / 第九章 / ty.rs / token.rs / lexer.rs / value.rs / ast.rs / parser.rs / typecheck.rs / ir.rs / codegen.rs / vm.rs / validate.rs / cache.rs / host.rs / engine.rs / ffi.rs / eval.rs / tests/arrays.rs / tests/type_errors.rs / tests/tutorial_examples.rs / docs/TUTORIAL.md / docs/HOST_GUIDE.md

- **2026-11-21** · **性能/稳定/安全三线加固（D-044）**：① 性能——Criterion 基线（before-import-perf）+ loader 行号计数 O(n²)→O(n) 增量维护 + 基准位 11（import 编译）；基线对比三连测确认无回退（首轮 rust 基线自漂 +4.5% 证实机器噪声 ±5%，compile (memory) 专项 5 秒窗复测 p=0.98 无变化）。② 稳定——`tests/import_fuzz.rs` 4 项：随机 import 图 300 轮永不 panic、深度/数量上限实测触发、逃逸尝试全拒绝、Windows 设备名拒绝（测试自身踩中 `.vt`/`.bk` 扩展名陷阱——改名后的肌肉记忆错误）。③ 安全——**deps_fresh 正确性修复**：resolver 来源依赖无路径可读，原实现按路径读盘必然失败→缓存永不命中（is_file 标记区分，resolver 依赖失效由宿主 resolver 自行决定）；import 逃逸加固（canonicalize 包含性检查防 symlink/junction 出树 + Windows 保留设备名 CON/NUL/... + 控制字符 + 组件尾点/尾空格拒绝）；cache decode 拒绝 `..` 组件与坏 flag 依赖（防恶意 .sbc 借依赖校验做路径探测）。测试 217→**222 项全绿、零警告** · engine.rs / cache.rs / tests/import_fuzz.rs / tests/import_stmt.rs
- **2026-11-21** · **项目更名：`vitul` → `brook`（D-043）**：用户昵称「小河」的英译——crates.io `brook` 已被占用（BrookGPU 学术撞名），**用户明确接受放弃发布资格**（本地项目）；扩展名 `.vt` → `.bk`。历史条目中的旧名随全文替换统一，唯 Q4 / D-040 / 代号行三处回改为历史表述保真。测试 206→217 项（import 11 项），全绿零警告 · 全部文件
- **2026-11-21** · **约定式 import 落地（ADR-017 / D-042）**：语言新增 `import "名字"` 语句（仅文件头）——引擎从脚本所在目录（或 set_import_dir/set_import_resolver 配置）载入库源码，文本拼接合并编译；重复 import 幂等去重、循环 import 编译报错、撞名报错、`..`/绝对路径编译期禁止（护栏引擎强制）；`.sbc` v4 追加依赖清单（路径+hash），加载时逐一校验，任一依赖改一字依赖脚本缓存自动失效；load_str 写 import 明确报错（D-041 规则）。parser +30 / loader +120 / cache +40 / 测试 11 项，**217 项全绿、零警告**；教程 §4.7→§4.8 重写为真实能力、HOST_GUIDE §12 重写（拼接降为备选）· 第三章 3.8、第七章 ADR-017、第七章 ADR-013 修订注、第九章、parser.rs / lexer.rs / token.rs / ast.rs / engine.rs / cache.rs / tests/import_stmt.rs
- **2026-11-21** · **教程规则收紧：不写不存在的能力（D-041）**：用户按教程 §4.7「公共函数库（如果你的宿主提供了）」实测失败——该能力当前不存在，假设句式（"如果你的宿主提供了"）误导用户浪费半天。修正：§4.7 删除；§4.6 收尾直说「可调用函数 = 13 内置 + 宿主注册，无公共库、无跨脚本调用；复用逻辑 = 复制粘贴或请宿主做成宿主函数」；§11 import 行同步「当前不支持」；HOST_GUIDE §12/12.1 加定位警示（两模式均为宿主侧自行实现，非语言能力，别写进脚本教程）。**教训**：教程只写当前存在的能力；"如果宿主提供了"这类假设句式一律禁止——读者没有义务分辨哪些是假设哪些是现实。测试 26 项教程测试全过 · docs/TUTORIAL.md、docs/HOST_GUIDE.md
- **2026-11-21** · **项目更名：`miniscript` → `vitul`（D-040；vitul 与 .vt 后续更名 brook / .bk，见 D-043）**：动机——crates.io 的 `miniscript` 已被比特币生态 rust-miniscript 占用（发布资格阻塞）。候选经 crates.io 可用性批量核查 + 撞名搜索三轮筛选（用户先后提出 Verse——与 Epic Games 的 Fortnite/UEFN 脚本语言撞名、Spar——crates.io 已占用且撞学术 Spar 并行语言），最终定名 **`vitul`**（crates.io 可用、技术领域零撞名——搜索仅人名与实体图书馆），扩展名 `.ms` → `.vt`，字节码魔数 `MSBC` → `VTBC`（旧缓存自然失效，格式布局不变）。全链路替换 32 文件 ~180 处（Cargo/源码/测试/文档/脚本）；C ABI 的 `ms_*` 符号与 `MS_ERR_*`/`MS_VAL_*` 常量**保留不改**（ABI 稳定优先，前缀作为历史遗留记录）。验证：206 项测试全绿、零警告 · 全部文件
- **2026-11-21** · **第二轮全盘复审（D-039）**：对自己全天新写的代码做新鲜眼光复查——**修复 1 处**：`msval_to_value` 的 STR 分支缺空指针检查（`len > 0 && s == null` 会 `from_raw_parts(null)`——调用契约虽允许 UB，但 ffi.rs 其他指针入口全部有 null 检查，防御姿态不一致）+ 测试钉死；教程 §2.4 浮点字面量措辞澄清（`.5` 不支持——点号开头是元组索引语法，需 `0.5`）。**复查干净项**：typecheck pass 1 三重检查顺序、lexer 指数门控位置、parser If 臂深度计数、`impl_tuple_n` 单一来源（impl_tuple2 已被替换无残留）、Failure::of 无残留引用、无 TODO、0 警告。测试 205→**206 项全绿** · ffi.rs / tests/ffi.rs / docs/TUTORIAL.md
- **2026-11-21** · **ADR-016 显式化：顶层只允许函数定义（不做顶层语句流）**：阶段 1 起的隐式选择补全论证并立 ADR——六条理由（调度权归宿主 / 热重载语义干净 / 可复现性 / 安全模型按调用计费 / 并发无顶层共享状态 / 全线简化）、代价（main 包裹 / 无顶层常量 / ad-hoc 手感弱）、关键洞察（**糖的方向性**：语句流可从函数入口廉价模拟，反之不行——选下位原语，糖留给宿主）、证据链（rhai_demo 34 脚本全任务型 / REPL 命令式 / ARGS 注入 0 使用）· 第七章、TUTORIAL.md §1
- **2026-11-21** · **教程按读者重构：纯脚本教程 + 独立宿主指南（D-038）**：用户反馈"脚本用户不需要宿主/Rust 知识，宿主内容喧宾夺主"——TUTORIAL.md 重写为**纯脚本教程**（§1 脚本结构平铺图与执行模型 / §2 语言基础含运算符全表与整数边界 / §3 控制流 / §4 函数 / §5 元组详解 / §6 字符串 / §7 内置函数完整参考 / §8 报错对照表 13 条 / §9 完整巡检示例 docs/patrol.bk / §10 速查表 / §11 刻意没有的东西及替代方案），宿主内容全部迁至新文件 **docs/HOST_GUIDE.md**；§1 三次澄清（函数平铺不嵌套含结构图、入口名由宿主决定非强制 main、顶层无语句流的设计原因）均来自用户提问。配套测试 26 项（patrol.bk 用 include_str! 原样执行）。测试 199→**205 项全绿、零警告** · docs/TUTORIAL.md、docs/HOST_GUIDE.md、docs/patrol.bk、tests/tutorial_examples.rs
- **2026-11-21** · **公共函数库模式落地 + checker 重复定义措辞区分（D-037）**：回应「1.bk 公共函数、其他脚本复用」——语言无 `import`（ADR-013 已记录为开放项），官方姿势是**宿主拼接**（lib 源码 + 业务源码 → 一份编译单元），教程新增 §4.4 + `shared_library_via_host_concat` 测试（顺序无关 / 同名冲突报错 / 缓存与热重载适配）。顺带修 checker：pass 1 之前宿主碰撞与脚本内部重复定义走同一条报错（措辞 "already defined by the host" 在拼接场景误导）——Checker 记录 `host_names`，两类碰撞分开措辞（"by the host" vs "duplicate definition"）。测试 199→**208 项全绿、零警告** · docs/TUTORIAL.md §4.4、typecheck.rs、tests/tutorial_examples.rs
- **2026-11-21** · **教程元组详解 + 修复嵌套索引的词法缺陷（D-036）**：用户反馈 i64 表格易误读（42 像上限）→ 表格澄清 + FAQ Q13（字面量/运算溢出均报错不回绕 + 最小值写不成字面量的冷知识）+ `integer_boundaries` 测试。教程 §2.5 扩为元组详解（单元素尾逗号、嵌套连点、无解构、禁相等、至少 N 元、标志位模式、与数组差异）。**扩写时抓到真实词法缺陷**：`p.1.0` 里 `1.0` 被数字规则整体读成浮点字面量，嵌套元组索引链（`t.0.1`）在现有引擎中根本无法表达——lexer 增加 `prev_dot` 状态（Dot 后的数字只按整数读，不吞 `.小数`/指数；普通位置 `1.5`/`2e3` 不受影响）。测试 197→**199 项全绿、零警告** · 第三章、docs/TUTORIAL.md、lexer.rs、tests/tutorial_examples.rs
- **2026-11-21** · **新增入门教程 docs/TUTORIAL.md + 可执行示例集（D-036）**：面向小白的中文教程——定位/五分钟上手/语言速览（函数、变量、控制流、类型、元组、字符串、内置白名单）/宿主集成（register_fn!、类型边界与默认 i64 防线、错误诊断、Opaque、N 元组、热重载、安全三件套）/进阶（.sbc 缓存的诚实定位、线程模型、性能）/12 条 FAQ。配套 `tests/tutorial_examples.rs` 20 项测试逐条执行教程示例（断言钉死每个声称的返回值）——**教程与代码永不过期**。写作过程中教程自身三轮被测试纠错：顶层语句不存在（quick_start 首版踩坑，已升级为教程 Q0/§1 重点提醒）、`Error::report` 双参数签名、线程返回 `Value` 不 Send（跨线程只传标量的实例教学）；另记录两个口径：codegen 的"the maximum is 8"对 >8 实参调用可达（typecheck 对 9 参数函数放行后由 codegen 拦截）、编译错 `x +` 的位置在被吃掉的换行后暴露（3:1）。测试 177→**197 项全绿、零警告** · docs/TUTORIAL.md、tests/tutorial_examples.rs
- **2026-11-21** · **rhai_demo / android_so_demo 迁移评估 + so_demo 改写 POC（D-035）**：两个宿主全量暴露面盘点（rhai_demo：229 注册 / ~197 名 / 45 Map + 22 Array 返回；so_demo：232 重载 / 190 名 / 19 Map + ~55 Array 返回），完整清单入 `docs/migration/{rhai_demo,so_demo}_inventory.md`。① host.rs `impl_tuple_n!` 把宿主元组桥接从 2 元扩到 **2–8 元**（ADR-013「仅 2 元组」实为桥接层缺口而非语言缺口，正式解除）；② typecheck 修复索引反推的**续推**缺口（`func f(t) { t.0 + t.1 }` 此前在第二索引误报越界，现按 ADR-006「至少 N+1 元」扩长）；③ `tests/migration_so_demo.rs` 4 项 POC——t_wifi（标量面 + OK/ERR 契约 + 编译错/宿主 Err 双失败通道）、bind_verify 忠实改写（Map→元组、Array<Map>→Opaque+indexed getter、try/catch→(ok,err) 探针，9 行日志逐一断言）、Task 句柄（内部可变性）、2–8 元组往返与类型错误。测试 173→**177 项全绿、零警告** · 第七章 ADR-013 修订、第八章 Q6、第九章、docs/migration/、host.rs / typecheck.rs / tests/migration_so_demo.rs
- **2026-11-21** · **Q15 解决 + 零依赖 fuzz 冒烟（D-034）**：① typecheck Pass 1 增加**内置名保留检查**——`func abs(x){…}` 现在报 `[type] function … is a reserved built-in name`（与宿主碰撞措辞区分；此前内部调用静默命中内置、只有入口点可达，行为不对称）；② `()` 字面量在 typecheck / codegen（`ConstUnit` 不再分配空元组）/ eval / `parse_type`（`: ()` 注解即 Unit 类型）**四处归一为 Unit**，`.sbc` **VERSION 2→3** 强制重建（wire 不变，防旧缓存带旧语义存活）；③ 新增 `tests/fuzz_smoke.rs` 4 项**零依赖模糊冒烟**：5000 轮 .sbc 字节变异（翻转/截断/拼接/覆写）解码永不 panic、magic/version/截断头门控、3000 轮随机 token 源码编译永不 panic、200 轮随机长度深度压力（含 else-if 表达式阶梯——即 D-030 修过的那条路径）必被 MAX_NESTING 拦截；④ 新增 `tests/language_semantics.rs` 3 项固定新语义。测试 166→**173 项全绿、零警告**，既有脚本无一依赖旧语义（迁移/集成测试全过）· 第三章 3.8、第八章 Q15、第九章、typecheck.rs / codegen.rs / eval.rs / parser.rs / cache.rs / tests
- **2026-11-21** · **C ABI 句柄式脚本层（ADR-015 / D-033）**：新增 `ms_script_create` / `ms_script_create_cached`（接线 `compile_cached`）/ `ms_script_call`（`MsVal` 标量+字符串跨边界，i64→f64 自动提升）/ `ms_script_reload`（原地热重载，失败保留旧字节码）/ `ms_script_free` 与 `ms_last_report`（多行诊断过 C 边界）六个**增量导出**；`MS_ERR_BUFFER=6`（buffer 不足带所需大小）/ `MS_ERR_THREAD=7`（句柄绑定创建线程，跨线程拒绝且异线程 free 不取所有权），`ms_abi_version` 保持 1；句柄/元组结果不可跨边界（ADR-011 最小攻击面延续）。宿主新增 7 项 FFI 测试——**166 项全绿、零警告**；`android_cli` 扩展句柄段，PLR110 真机全过（缓存落盘并二次命中、多参数、字符串增长重试、错误定位、reload 后新逻辑生效、free 语义）· 第三章 3.7、第七章 ADR-015、第九章、ffi.rs / tests/ffi.rs / examples/android_cli.rs
- **2026-11-21** · **AOT 移出路线图（ADR-014）**：阶段 4 冻结为最低优先级备选，不再作为活跃路线图阶段；§1.1 定位改为单模 VM、§1.2/§2.6/§5/§6 的 AOT 表述标注冻结、§2.4 保留设计记录并注明重估条件、ADR-002 增加修订注、Q8 备注改为「突破只剩 JIT 一条路」；顺带修正 §2.1 架构图残留的 SSA 表述（D-006 已否决 SSA）并给 §2.2 补 ADR-001 修订注（原「JIT 不可行」前提已证伪）。源码零 AOT 引用（grep 确认），无代码改动 · 第一章、第二章、第四章、第五章、第六章、第七章 ADR-002/ADR-014、第八章 Q8
- **2026-11-21** · **Q10 真机验证完成**（阶段 3 收官）：PLR110 真机（Android 16、arm64-v8a、SELinux Enforcing）四项实测——① 默认 smoke test：完整管线（宿主 Opaque 句柄 + 类型化回调 + 元组返回）输出 (160, true)；② 扩展脚本：递归 fib(12)=144 + 循环 sum_to(100)=4950（左闭右开区间，验证者期望值算错被实测纠正）+ 字符串拼接 + 元组索引 + to_f64 → (5254, true)；③ 除零错误带定位（9:18）与调用栈，exit=1；④ gas 计数器止损 `while true` 无限循环，exit=1；顺带确认 D-008 边界校验（未用参数默认 i64 收到 Handle 被拒）在真机生效。双 ABI .so 重建后 ELF 架构与 5 个 ms_* 导出符号复验通过；`scripts/android_build.sh` 修复 Git Bash 下 /data 路径被 MSYS 改写导致 push 失败（MSYS_NO_PATHCONV=1，Linux/macOS 无副作用）。测试遗留已清理（设备 4 文件 + 本地临时脚本）· 第一章、第四章、第八章 Q10、第九章、scripts/android_build.sh
- **2026-11-21** · Q13/Q14 落地（D-031）：① eval.rs 参考解释器对齐 VM——`eval_binary` 补 bool 等值 / 句柄对象身份 / 字符串 `+` 与比较分派臂（此前四类运算全部落入 `as_f64` 报错），`eval_call` 宿主错误保留原 `ErrorKind`（对齐 D-028）；② codegen 消除与 typecheck 的块值类型分歧——无 else 的 if 语句与末位赋值统一返回 Unit（边界行为零变化，消除 BlockExpr 场景的 codegen env 类型漂移）；③ validate.rs 校验 `CallScript` 实参不超过被调窗口 nregs（结构安全界，防 argv 拷贝越界 panic；语义 arity 留给 checker——首轮误用 == params.len() 的语义约束被 cache.rs 的 every_opcode_roundtrips 测试抓回，修正为结构不变量）；④ cache.rs `validate_wire` 收紧至与 validate.rs 同级（register 用 >=、跳转 target 严格 <）；⑤ 顺手修正 codegen 编译期错误误标 `ErrorKind::Runtime`（影响 C ABI 错误码映射）、Engine 过时注释与注册失败提示语、lexer 非 ASCII 报错乱码（解码完整 UTF-8 序列）；新增 tests/eval_parity.rs 4 项 + tests/validate.rs 窗口检查 1 项；shell 恢复后实测 **159 项全绿**（157 单测/集成 + 2 doctest）、release 零警告 · 第三章、第八章 Q13/Q14/Q15、第九章、eval.rs / codegen.rs / validate.rs / cache.rs / engine.rs / lexer.rs / tests
- **2026-11-21** · 代码审查修复两处缺陷：① `parse_primary` 的 if 表达式臂补 `enter()` 深度守卫，堵住表达式位置 `else if` 链绕过 MAX_NESTING 的无界递归（栈溢出 abort、catch_unwind 抓不住；语句位置原本已有守卫，但 depth.rs 未覆盖表达式路径）；② `Op::EqHandle`/`NeHandle` 由比较堆槽位下标改为经 `Handle` 的 `PartialEq`（type_name + `Rc::ptr_eq`）比较对象身份，同一宿主对象经两个实参/多次宿主返回不再误判为不等，非 handle 槽位明确报错；新增 5 项回归测试（depth.rs 表达式阶梯正反两用例 + handle_equality.rs 4 项），**本机 shell 故障未运行验证** · 第三章 3.8、第八章 Q13/Q14、第九章、parser.rs / vm.rs / ir.rs / tests/depth.rs / tests/handle_equality.rs
- **2026-11-20** · 线程模型与热重载语义落文档并补测试：新增 `tests/threading.rs` 7 项（8 线程各自 `Engine` 注册不同闭包状态互不干扰、同源并发编译、reload 后新调用生效、克隆保留旧字节码、reload 失败旧脚本可用、reload 后诊断引用**新**源码、句柄留在本线程）；修掉 `Engine::reload` 漏更新 `Script::source` 的缺陷（重载后 `report` 会用旧源码配新行号，caret 指错位置）；`Engine` / `reload` 文档写明 `!Send` 是设计而非疏漏——`call(&Script)` 与 `reload(&mut Script)` 在 safe Rust 下不可能重叠，因此不需要锁也没有「半新半旧」窗口，多线程宿主一律每线程一个 `Engine`；测试数 142→149 · 第二章、第九章、engine.rs / tests/threading.rs
- **2026-11-20** · C ABI 错误模型增补：`ffi.rs` 的 `thread_local` 改为结构化 `LastError`（消息 + 行列），新增 `ms_last_error_line` / `ms_last_error_col` 导出，C 侧不再只能解析字符串；错误码按**阶段**区分，新增 `MS_ERR_RUNTIME = 5`（未知函数/参数个数与类型/gas/堆/溢出/除零/宿主回调），`MS_ERR_ENGINE = 3` 收窄为「编译不过或返回值不是 i64」；消息仍写进**调用方 buffer**，跨边界零分配；`ms_abi_version` 保持 1（纯增量导出，老调用方仍靠 `!= MS_OK` 判失败）；`jniLibs` 双 ABI `.so` 已重建并校验导出符号；测试数 140→142 · 第三章、第九章、ffi.rs / tests/ffi.rs / jniLibs
- **2026-11-20** · 数值内置函数 i64 支持（Q7）：`abs`/`min`/`max` 改为数值泛型，实参全 i64 则结果 i64，`ir::builtin` 追加 `ABS_I64/MIN_I64/MAX_I64 = 16/17/18`，codegen 用 `numeric_id` 选变体、`builtin_ret(func)` 取返回类型；**不加 opcode、`.sbc` VERSION 不变**（旧缓存里不可能出现新 id，旧解码器遇未知 id 仍被 `validate` 拒绝）；`abs(i64::MIN)` 用 `checked_abs` 报 `i64 abs overflow`（Rust 的 `abs` 在此 panic）；参考解释器同步。顺带把缓存解码里写死的 `TO_STR_STR` 上界换成 `builtin::name(id) != "?builtin"`，避免下次加 id 时再踩同一个坑；`sqrt`/`floor`/`ceil`/`pow` 维持仅 f64；整数实参会把未定参数推成 i64（与 `x + 1` 一致），无证据时仍回落 f64。测试数 126→140 · 第三章、第八章 Q7、ir.rs / typecheck.rs / codegen.rs / vm.rs / eval.rs / cache.rs / tests/builtins_numeric.rs
- **2026-11-20** · 修掉**栈溢出崩溃**：解析是递归下降，typecheck / codegen / AST 的 `Drop` 同样递归，深嵌套源码（1000 层括号、200 项 `1+1+…`、200 条 `else if`）会让进程 `STATUS_STACK_OVERFLOW` 直接 abort，`catch_unwind` 抓不住；现在 `parser.rs` 引入 `MAX_NESTING = 64` 与统一的 `enter()` 计数（块、括号、一元、二元链均计入，链式每个算子 +1，因为左结合链虽是循环构造但树深随项数增长），超限报 `nesting too deep (limit 64)` 并带 `file:line:col`；不在解析期拦住的话，AST 先被造出来，报错之后析构本身就会再次溢出。**性能排查（原判定有误，已纠正）**：首版把 `format!` 的错误构造直接写在 `enter()` 里，`compile fib.bk` 从 8.0µs 涨到 9.2µs（+15%，A/B 关掉守卫确认可复现：8.44µs）——函数体积膨胀后递归下降的热函数不再内联，代价远大于「一次自增比较」。抽成 `#[cold] #[inline(never)] fn depth_error()` 后回到 8.39µs，守卫实际免费；VM 侧 fib(18) 158.2µs / sum_to(200) 1.60µs 与基线持平。教训：给热函数加错误分支时，错误构造必须外提为冷函数，否则「只多一个 if」的直觉不成立。测试数 118→126 · 第三章、第九章、parser.rs / tests/depth.rs
- **2026-11-20** · 诊断能力补齐：`lexer.rs` 的 `col` 改为**字符列**（UTF-8 续字节不再计数），修掉含中文的行报错列号偏移；`Error` 新增 `trace`（`TraceFrame`）与 `Error::report`，VM 在 `run` 失败后回放帧栈生成调用栈（外层在前，热路径零成本）；新增 `src/diag.rs` 渲染 `--> 文件:行:列` + 源码行 + 按显示宽度（tab=4、CJK=2）对齐的 caret，深栈自动省略中段；`Script` 保留 `source` 并新增 `Script::report`，缓存命中同样可渲染；宿主函数返回的错误保留原 `ErrorKind`，不再一律压成 `runtime`。基准无回退（fib(18) 144.7µs vs 基线 157.9µs，sum_to 1.54µs vs 1.63µs）；测试数 107→118 · 第三章、第九章、error.rs / diag.rs / vm.rs / engine.rs / lexer.rs / cache.rs / tests/diagnostics.rs
- **2026-11-20** · 语法与诊断增补：支持 `/* */` 块注释（C 风格不嵌套；跨行的块注释补发一个 `Newline`，否则会把前后两条语句粘连）；统一类型错误信息方向为 `expected <期望>, found <实际>`，覆盖 `if` / `while` 条件、`&&` / `||`（区分左右操作数）、`!`、`let` 标注、赋值、`for` range、函数返回值、脚本调用与内置函数参数；测试数 77→107 · 第三章、lexer.rs / typecheck.rs / tests/operators.rs / tests/type_errors.rs
- **2026-11-20** · 语法增补 `break` / `continue`：`Stmt` 增两个发散语句，typecheck 按 `loop_depth` 拒绝循环外使用，codegen 用循环上下文栈回填跳转（`for` 的 `continue` 落在增量之前，否则索引不变会死循环）；**不新增 opcode**、`.sbc` 格式与版本不变；参考解释器同步支持语句位置、表达式位置报明确错误；测试数 65→77 · 第三章、parser.rs / typecheck.rs / codegen.rs / eval.rs / token.rs / tests/loops.rs
- **2026-11-20** · rhai 迁移可行性验证：新增 `tests/migration_browser_pick.rs`，把最难的 browser_pick 脚本按「宿主适配层」方案改写为 .bk 并跑通 12 组断言；新增 ADR-013 记录可行性与静态类型边界；测试数 64→65 · 第七章、第九章、tests/migration_browser_pick.rs
- **2026-11-20** · JIT 路线证据复核与决策记录：AOSP sepolicy 源码核查（ADR-001 事实前提证伪）、Cranelift 包体受控实验（+1.81 MiB/ABI）、fib(20) 同口径 9.6×、冷启动 721 µs / 2 次回本、sum_to(200) 13.2× / 445 次回本；修正 1.3 / ADR-001、新增 Q11 与 ADR-012 草案 · 第一章、第七章、第八章、第十章
- **2026-11-20** · 性能逐项优化（续4·终）：⑥b 预分配 regs（`Vm::new` 按 max_nregs×max_depth、带 2MiB 上限预留容量，免深递归重分配）双向严格 A/B 信号不一致、Rust 基线自身抖动 4%，判定噪声主导、回退（D-025）· 第十章、vm.rs
- **2026-11-20** · 性能逐项优化（续3）：①b 尾位置 Ret 复制经字节码实证在本仓库 fib/sum_to 均不命中、跳过；⑥a call/ret 瘦身（Frame 16B + 去 argv + nregs 局部 + 直接索引）实现后严格 A/B fib 完全持平、回退，新增 `tests/frame_packing.rs` 4 项 · 第十章、vm.rs / tests
- **2026-11-20** · 性能逐项优化（续2）：⑤ `JmpIf*I64Imm` 融合立即数比较（opcode 65→71、`cache::VERSION` 1→2，fib 177.60→157.93µs / A/B 1.10×，sum_to 不变）· 第四章、第十章、ir.rs / codegen.rs / vm.rs / cache.rs / validate.rs / tests/imm_branch.rs
- **2026-11-20** · 性能逐项优化（续）：①a Assign dst 直写（sum_to 5→4 条指令）、② `validate` 出生点校验、③ 寄存器访问改 unchecked（fib 206.55→177.60µs、sum_to 2.08→1.63µs）；④「gas 只按控制流步计费」实测负收益已回退（D-022）· 第四章、第十章、codegen.rs / validate.rs / vm.rs / engine.rs / tests
- **2026-11-20** · 阶段 3 收尾：`.sbc` 字节码缓存 + 内存级 API + Criterion 实测（ADR-010），Q9 结论更新 · 第四章、ADR-010、第八章、cache.rs / engine.rs / benches/engine.rs / tests/cache.rs
- **2026-11-20** · Android 打包落地：`cargo ndk` 双 ABI `.so` + C ABI + 原生 CLI + 一键脚本（ADR-011）· 第四章、ADR-011、Cargo.toml / ffi.rs / examples/android_cli.rs / scripts / tests/ffi.rs
- **2026-11-20** · FFI 注册宏落地：`register_fn!` + `ScriptType`/`FromValue`/`HostResult` + `Opaque<T>`（ADR-009），根除手写签名漂移 · 第三章、ADR-009、host.rs / engine.rs / tests/host_macro.rs
- **2026-11-20** · 阶段 3 安全边界落地：`catch_unwind` panic 边界、内存配额、溢出/参数上限改为编译错误；修复字符串 `+` 被降级为整数加法的静默错误；补齐 `Concat` 指令 · 第四章、ADR 无、engine.rs / vm.rs / codegen.rs / typecheck.rs
- **2026-11-20** · 热路径第二轮优化：扁平寄存器文件、局部 ip/code、融合比较分支、立即数加法、紧凑指令（ADR-008），fib 476µs→211µs · 第四章、ADR-008、vm.rs / ir.rs / codegen.rs
- **2026-11-20** · 阶段 2 完成：类型特化寄存器 VM，20 测试全绿，性能数据见第十章 · 第四章、ADR-007、代码库
- **2026-11-20** · 新增宿主调用边界类型校验：推断默认值与实参不符时明确报错，杜绝静默算错 · ADR-006、engine.rs
- **2026-11-20** · 修复 `if` 分支类型合并 bug：未 resolve 的 Var 比较不相等导致返回类型退化 Unit（阶段 1 遗留，VM 边界转换时暴露）· typecheck.rs
- **2026-11-20** · 阶段 1 完成：`brook` crate 落地，14 测试通过，性能基线采集（见第十章）· 第四章、代码库
- **2026-11-20** · 语法增补赋值语句 `x = expr`（阶段 1 实现时发现循环体必需）· 第三章、ADR 无
- **2026-11-20** · 文档创建，固化多轮讨论的全部架构与语言方案 · 全项目
- **2026-11-20** · 语法最终定稿：`func` + `let` + 无分号 + 参数无标注 + 返回类型可选 + 元组多返回 · 第三章
- **2026-11-20** · 确定先不做 AOT，仅实现 VM 档 · 第二章、第四章、ADR-002

---

## 第十章 · 开发日志

> 动手开发后，按日期追加遇到的问题、解决方案、临时决策。格式示例见下。

### 记录模板

```
#### YYYY-MM-DD · 简短标题
- **问题**：遇到了什么
- **排查**：怎么定位的
- **解决**：最终方案
- **影响**：改了哪些文件 / 是否影响既有决策
```

### 2026-11-20 · 阶段 1 实施记录

**成果**：`brook` crate 从零落地。模块：`span` `error` `token` `lexer` `ast` `parser` `ty` `typecheck` `value` `eval` `engine`。14 个测试全绿（词法 6 + 集成 8）。

**性能基线**（Criterion，fib(18)，release 档 LTO=thin）：

| 项 | 耗时 | 相对原生 |
|---|---|---|
| 手写 Rust fib(18) | 6.0 µs | 1× |
| brook AST fib(18) | 4.58 ms | ≈765× |
| 编译 fib.bk（解析+类型检查） | 7.5 µs | — |

765× 符合 AST 解释器预期（boxed Value + HashMap 作用域 + 每次 clone）。阶段 2 目标：寄存器 VM + unboxed 槽位压到 3–8×。

**遇到的问题与解决**：

- **D-001 · 双字符运算符吞掉后一个字符**：lexer 中 `==`/`..`/`->` 等分支各自 bump 两次，函数末尾又统一 bump 一次，导致 `0..n` 的 `n` 被吞，`parse_block` 报 `expected LBrace found Ident(total)`。解决：双字符分支只 bump 一次，末尾统一消费第二个字符。教训：词法器的 bump 职责要单一。
- **D-002 · 缺少赋值语句**：风控示例的循环体 `total = total + i` 无法解析（只有 `let`）。解决：AST 增 `Stmt::Assign`，parser 用 `is_assignment_ahead()` 前瞻 `ident =`，typecheck 统一类型、eval 向内层作用域查找并更新。
- **D-003 · 全推断边界**：`add(a,b){a+b}` 报 `cannot infer type`。根因是脚本看不到宿主调用点类型。解决：见 ADR-006（默认 I64 收敛 + 索引反推元组）。这是设计层面发现，非 bug。
- **D-004 · 内置数学函数仅 f64**：`abs/min/max/sqrt/...` 当前签名固定 f64，i64 需先 `to_f64`。已记入 Q7，阶段 2 可考虑数值重载（按实参类型分派）。【2026-11-20 已落地：`abs`/`min`/`max` 支持 i64（追加 builtin id 16/17/18，`checked_abs` 兜 `i64::MIN`）；`sqrt`/`floor`/`ceil`/`pow` 维持仅 f64，理由见 Q7】
- **D-005 · 测试预期写错**：3 个词法单测误以为源码末尾无换行也会产生 Newline token。实际词法器行为正确（无换行则不产生），修正测试预期。

**默认采用（Q1–Q3 用户未单独回复，按方案暂定值实施）**：for 放 v1、字符串仅 `+` 拼接、整数溢出按错误终止。均可后续调整。

**Android 验证（提前）**：`cargo build --target aarch64-linux-android --lib` 通过。核心库零平台依赖，阶段 3 交叉编译无障碍。

**下一步**：阶段 2 已完成，见下方 2026-11-20 阶段 2 实施记录。

### 2026-11-20 · 阶段 2 实施记录

**成果**：类型特化寄存器 VM 落地。新增模块 `ir`（指令集）、`codegen`（AST→字节码）、`vm`（执行器）。对外 API 零改动（`Engine::call/reload/register_fn` 签名不变），`eval.rs` 保留为参考实现。20 个测试全绿（词法 6 + 集成 14，其中 6 项为 VM 专属：字符串比较、短路、gas 限制、嵌套调用+内置、元组过宿主、推断不匹配防护）。

**性能数据**（Criterion，release 档；同机对比）：

| 场景 | 手写 Rust | brook VM | 相对 Rust | 阶段 1 AST |
|---|---|---|---|---|
| fib(18) 递归 | 7.1 µs | 476 µs | ≈67× | 4.58 ms（765×）|
| sum_to(200) 循环 | 277 ns | 8.45 µs | ≈30× | — |
| 编译 fib.bk | — | 7.9 µs | — | 7.5 µs |

- 相比 AST 解释器提速 **6.4×（递归）**；VM 相比原生 **30–67×**。
- **未达 3–8× 目标**（已记入 Q8）。原因：纯解释 VM 的架构税（取指/分派/间接索引）仍在；递归场景更差是因每次调用的帧开销。要突破须 JIT/AOT，但 JIT 被 Android 排除、AOT 牺牲热重载——这是约束组合下的合理落点，不粉饰。【2026-11-20 注：此处「JIT 被 Android 排除」已证伪，见 ADR-001 修订 / Q11，后续性能路线改按 ADR-012 草案评估】
- 优化里程碑（历史）：初版 VM 859µs → 当时帧池方案后 476µs（-45%）。该方案已被 ADR-008 的扁平寄存器文件 + `base` 偏移取代；当前实现不再有帧池。

**遇到的问题与解决**：

- **D-006 · 决定用寄存器字节码而非 SSA IR**：SSA 的 phi 节点和栈式字节码在此规模下复杂度收益不成正比；直接把类型信息 threaded 进指令（类型特化）拿到了主要收益。codegen 用 label + 回填处理控制流。
- **D-007 · `Stmt::If` 返回值类型退化为 Unit**：`check()` 返回 Unit 而非元组。根因是类型检查器的 `merge_branch_types` 直接比较两个未 resolve 的 `Ty`，元组内元素的 `Var` 尚未统一时不相等，误判为分支类型冲突→Unit。阶段 1 解释器不看 sig.ret 故未暴露，VM 依赖它做边界转换才显形。解决：改为 `merge_branches` 方法，先 unify 两分支再 resolve。
- **D-008 · 推断默认值导致静默算错**：`func cmp(a,b){if a<b...}` 只做比较、无字符串种子，参数默认 I64；宿主传 str 时 VM 按 I64 解释堆索引，返回错误结果且不报错。解决：在 `Engine::call` 边界加 `type_matches` 校验，不匹配明确报错并提示加参数标注（`a: str`）。元组参数放宽为「长度≥推断值、已知元素匹配」，兼容 `first(p){p.0}` 的反推。
- **D-009 · LLVM 过度优化 benchmark**：朴素 `rust_sum` 被折叠成等差数列公式（1.5ns，失真）。解决：累加器加 `black_box` 阻止优化，得到真实的 277ns。
- **D-010 · host 调用参数按签名解释**：初版用 untyped 猜测槽位类型，f64/bool 参数会被误读为 i64。解决：`CallHost` 按 func id 从注册表查签名 params，逐参数 `slot_to_value(ty)` 重建。

**Android 验证**：`cargo build --target aarch64-linux-android --lib` 通过（阶段 2 新模块无平台依赖）。

**下一步**：Q8 性能决策——先在实际业务场景测延迟预算，若够用则进入阶段 3（安全边界加固：catch_unwind、内存配额、`.sbc` 缓存）；若不够用再评估领域特化（避免 JIT/AOT 与热重载的冲突）。

### 2026-11-20 · 热路径优化第二轮 + 阶段 3 安全边界

#### O-001 优化：扁平寄存器文件（最大单项收益）

- **问题**：帧池把 fib 从 859µs 降到 476µs 后，剖析显示热点不再是算术，而是寄存器访问路径：`self.frames.last().unwrap().regs[r as usize]`——每次读写都要经过 Vec 尾指针、`Option::unwrap`、二次索引。
- **解决**：所有帧共用一个 `Vec<Slot>`，`Frame` 只保留 `base` 偏移。寄存器访问变成 `self.regs[base + r]` 一次索引；压帧变成偏移自增 + 越界时 `resize`。帧池（`Vec<Vec<Slot>>`）随之删除——它本来就是为绕开"每帧一个 Vec"而存在的补丁。
- **结果**：fib 476µs → 223µs。

#### O-002 优化：局部化 ip / base / code

- **问题**：每步都要 `frames[fi].func` / `frames[fi].ip` 两次带边界检查的索引，再 `program.get(func)` 又一次，最后 `f.code.get(ip)` 再一次。
- **解决**：调度循环把 `ip`、`base`、`code` 提到局部变量，只在 call/ret 时写回帧或重载。关键技巧是 `let program: &'a Program = self.program;`——`program` 是 `&'a` 共享引用，拷出后不再借用 `self`，于是循环体可以自由可变借用 registers/heap/frames，不必拆结构体。
- **注意**：`Ret` 必须保存"返回目标寄存器"，这个值放在**子帧**的 `ret_to` 里，出栈前取出、再用父帧的 `base` 写回。

#### O-003 优化：融合比较分支 + 立即数加法

- **问题**：`while i < n` 每轮执行 `LtI64 cond,a,b` + `JumpIfFalse cond` 两条指令；`for` 的 `i = i + 1` 更是 `ConstI64 1` + `AddI64` + `Mov` 三条。
- **解决**：
  - 12 条 `JmpIf{Lt,Le,Gt,Ge,Eq,Ne}{I64,F64}`，语义统一为"谓词成立则跳转"。codegen 先正常编译条件，再**把最后一条比较指令原地改写成融合跳转**（`fuse_branch`）——不做指令删除，因此无需重映射跳转目标；比较结果寄存器在 if/while/for 中必然只被该分支消费，丢弃安全。
  - `AddI64Imm` 让 `for` 自增降到 1 条；`x ± 1` 在 AST 层折叠为立即数。
- **结果**：fib 223µs → 201µs；循环 2.53µs → 2.37µs。

#### O-004 优化：紧凑指令

- **问题**：`Instr` 含 `Span`（4 × usize = 32 字节）+ `Op`（24 字节）= 56 字节，每条指令都要整体拷入寄存器。
- **解决**：位置信息改为 `line: u32 + col: u32`，`Instr` → 32 字节；`Instr::span()` 按需重建 `Span` 供报错使用。
- **权衡**：丢失字节偏移 `start/end`，只剩行列号。诊断只用到行列，可接受。

#### D-011 缺陷：字符串 `+` 被降级成整数加法（严重）

- **发现**：写内存配额测试时，`pad(s, n) { out = out + s }` 返回了错误结果而非报错。
- **根因**：语言规格写了"字符串支持 `+` 拼接"，但 codegen 只有 `AddI64`/`AddF64`，`BinOp::Add` 遇到 `Ty::Str` 落进 `_ =>` 分支生成 `AddI64`，等于把两个堆索引相加。类型检查器也没拦住：函数体的推断早于调用点，`s` 当时还是未解析的 `Var`，而"Var 放行"的逃生分支让 `Str+Str` 通过了算术检查。
- **解决**：新增 `Op::Concat`，codegen 在数值路径之前先处理 `Ty::Str`（`+` → Concat；`-*/%` → 明确报错；比较照旧落到 `LtStr` 等）；类型检查器对 `Add + Str` 返回 `Ty::Str`。
- **教训**：**"Var 放行"这类宽松分支 + 一次性推断顺序 = 静默错误**。凡是宽松放行，下游必须有能力在类型最终确定时再判一次；codegen 就是那道兜底。

#### D-012 加固：panic 边界

- **做法**：`Engine::call` 用 `catch_unwind(AssertUnwindSafe(..))` 包住 VM 执行，把 panic 变成 `Err(Error::runtime("script aborted: internal panic ..."))`。
- **依赖**：需要 `panic = "unwind"`（默认）。`panic = "abort"` 下仍是进程终止，已在 `Cargo.toml` 注明。
- **测试**：注册一个 `panic!()` 的宿主函数，断言 `call` 返回 `Err` 而非展开。

#### D-013 加固：内存配额

- **做法**：`Vm` 记 `heap_bytes`，`MakeTuple` / `value_to_slot` / `intern_runtime_str`（含 `substr`、`to_str`、`Concat`）在**分配前**校验；超限返回 `Err("memory limit exceeded")`。默认 4 MiB，`Engine::with_heap_limit` 可调。
- **理由**：gas 只约束步数，不约束单步的分配量；没有配额，循环里的 `substr` 能在 gas 耗尽前吃掉大量内存。

#### D-014 加固：把 panic 从编译器里赶出去

- `fresh()` 寄存器溢出由 `expect` 改为置 `reg_overflow` 标志，`compile_func` 结束时返回 `Error::type_error("function too complex")`。
- `Regs::new` 的 `assert!` 可能被 9 个实参的调用触发，codegen 在 `compile_call` / `Expr::Tuple` 前置检查并报编译错误。

#### 本轮数据（release，Criterion，3s 采样）

| 场景 | 原生 Rust | 阶段 2（帧池） | 本轮优化后 | 当前比值 |
|---|---|---|---|---|
| fib(18) 递归 | 7.4 µs | 476 µs | **211 µs** | 27.6× |
| sum_to(200) 循环 | 280 ns | 8.45 µs | **2.41 µs** | 8.6× |
| 编译 fib.bk | — | 8.0 µs | 7.8 µs | — |

**结论**：循环类工作负载已经摸到 3–8× 目标区间的边缘；纯递归停在 27.6×，因为每次调用都要真实压帧，没有内联空间——这是解释器架构税的残值，无法靠"再优化几条指令"消除。要继续压迫只能上 JIT/AOT，而这两条路与 Android/热重载的冲突已在 ADR-001/002 定论。

**验证（该轮当时的状态）**：24 个测试全绿（6 单测 + 18 集成）；Android arm64-v8a 与 armeabi-v7a 交叉编译均通过；release 编译零警告。

**下一步**：FFI 注册宏（把 `vec![Ty::F64, ...]` 的手写签名消掉）+ `cargo ndk` 打包到真机跑示例脚本 —— 均已完成，见下方收尾记录；真机运行仍待设备（Q10）。

### 2026-11-20 · 阶段 3 收尾：FFI 宏 / Android 打包 / `.sbc` 缓存

**成果**

1. **类型化宿主 API**：新增 `src/host.rs`（`ScriptType` / `FromValue` / `HostResult` / `Opaque<T>`）、`Engine::register_typed0..6`、导出宏 `register_fn!`。签名从 Rust 类型推导，回调拿具体类型；支持 0–6 参数、fallible `Result<T, Error>`、句柄、字符串、二元组。raw API 保留。
2. **Android 交付**：`Cargo.toml` 加 `cdylib`；`src/ffi.rs` 导出 C ABI（`ms_call_i64` / `ms_last_error` / `ms_abi_version`，全部 `catch_unwind` 收口）；`examples/android_cli.rs` 原生 smoke test；`scripts/android_build.sh` 一键构建 + 有设备时 push 运行。安装 `cargo-ndk v4.1.2` 后打包出 `jniLibs/{arm64-v8a,armeabi-v7a}/libbrook.so`（759K / 538K）与两个 ABI 的 CLI（767K / 572K）。`readelf` 校验 ELF64-AArch64 / ELF32-ARM，符号表可见三个 `ms_*` 导出。
3. **`.sbc` 字节码缓存**：新增 `src/cache.rs`，零依赖二进制格式；header 含 magic/version/源码 FNV-1a hash/host registry fingerprint；当时 65 个 opcode 由单张宏表生成 encode/decode（当前 71，见 ADR-010）；解码全程边界校验（寄存器、跳转目标、字符串 id、函数 id、builtin id、尾部字节）。API：`script_to_bytes` / `script_from_bytes` / `save_sbc` / `load_sbc` / `compile_cached` / `load_cached` / `reload_cached`。
4. **测试（当时的阶段快照）**：从 24 增至 **40**（6 词法单测 + 6 缓存 + 3 FFI + 6 宏注册 + 18 集成 + 1 doctest），其中「全部 opcode 往返一致」是防 tag 漂移的硬护栏；当前测试数见文件头「当前状态」。

**性能数据**（Criterion，release，quick 10 samples，同机）

| 场景 | 耗时 | 说明 |
|---|---|---|
| 内存编译 fib.bk（`load_str`） | 7.96 µs | 前端全流程 |
| 内存解码 .sbc（`script_from_bytes`） | **1.52 µs** | 比编译快 **5.2×** |
| `load_file`（读源 + 编译） | 97.11 µs | 一次文件 I/O |
| `load_sbc`（读缓存 + 解码） | 83.20 µs | 一次文件 I/O |
| `load_cached`（读源 + 读缓存） | **163.94 µs** | 两次 I/O，反而最慢 |
| fib(18) VM / 原生 | 206.77 µs / 7.26 µs | 28.5×，与上轮一致 |
| sum_to(200) VM / 原生 | 2.35 µs / 0.28 µs | 8.4×，与上轮一致 |

**结论**：文件 I/O（约 80 µs）比编译（8 µs）贵一个数量级，所以 `.sbc` **不是磁盘加速特性**；它的价值在「源码已在内存时跳过编译」（Android assets / 网络下发）与「字节码作为独立可分发交付物」。已按此分层 API 并写入 ADR-010，不粉饰数据。

**遇到的问题与解决**

- **D-015 · `macro_rules!` 不能展开为 match arm**：初版 `enc_arm!/dec_arm!` 每个 arm 一个宏调用，编译报 `macros cannot expand to match arms`。解决：改为单张 `op_table!` 表一次生成**整个** match 表达式；把 `w` 标识符作为宏参数传入，使解码表达式与函数参数处于同一卫生上下文。附带收益：tag 只有一处定义。
- **D-016 · `Result` 别名冲突**：`impl HostResult for Result<T, Error>` 撞上 crate 内 `pub type Result<T>`，报 E0107。解决：写全路径 `std::result::Result<T, Error>`。
- **D-017 · 缓存校验误报**：`validate_wire` 把 `CallScript`/`CallHost`/`CallBuiltin` 的 `r[1]`（函数 id）当寄存器校验，报 `register out of range`。解决：tag 60–62 跳过 `r[1]`。
- **D-018 · cargo-ndk "No usable artifacts"**：用 `--config lib.crate-type=[...]` 覆盖未被 cargo-ndk 的 artifact 探测采纳。解决：在 `Cargo.toml` 持久化 `[lib] crate-type = ["rlib", "cdylib"]`；Windows 侧 `cargo test` 与 release 构建均不受影响。
- **D-019 · rustc 偶发 `STATUS_ACCESS_VIOLATION`**：首次 Android 交叉编译并行构建依赖时崩溃，`-j 1` 重试通过，属工具链偶发现象，非代码问题。
- **D-020 · `.sbc` 命中比直接编译慢**：见上表。定位为两次文件读取主导，不是解码慢。决策：保留严格 hash 校验（不用 mtime/size 偷换正确性），改为提供 `compile_cached` 内存源码路径，并在文档中明确 `load_cached` 只是便捷 API。
- **D-021 · 脚本语法不含分号**：缓存测试脚本误写 `let p = ... ; p.0`，词法器报 `unexpected character ';'`。修正为换行分隔——语言规格「无分号」再次验证有效。
- **D-022 · gas 按控制流计费负收益**：设想「直线指令不扣 gas、只扣分支/调用」能省检查。集中判断版 fib 252µs、内联到 17 臂版 fib 236µs，均慢于每指令版 191µs；Criterion 命名基线 A/B（同机各 25 样本）确认 fib 慢 29%、sum_to 慢 71%，且 v4 轮 Rust 基线更快，排除噪声。**排查**：每指令检查是循环顶部无条件直线代码、预测器近 100% 命中；17 个带冷 return 的判断反而污染分派臂的预测与 i-cache。**解决**：回退到每指令扣减，`max_steps` 语义文档同步改回。**影响**：省检查的直觉不等于省时间——热循环里「稳定的无条件操作」比「分散的条件操作」便宜，后续优化需实测而非推理。
- **D-023 · 交接稿的「fib 尾位置 Ret」在本仓库是幻觉**：交接稿建议 ①b 消除 fib 的共享 Ret（叶 5→2 条），但字节码 dump 显示 `fib.bk` 用显式 `return n`，`compile_if` 结果 `r1` 从不被读、共享 Ret 属于尾部 `fib(n-1)+fib(n-2)`；`sum_to` 是 for 循环也不命中。**排查**：dump 两份字节码（真实 fib + 尾位置 if 改写版），确认后者 10 条→8 条但前者根本不产生该模式。**解决**：跳过 ①b，不引入 remap 表 / 跨块判定 / 副作用落点判定的静默错值风险。**影响**：外部建议必须对照本仓库实际产物核实，不能照搬基准假设。
- **D-024 · ⑥a call/ret 瘦身零收益回退**：Frame 压 16B + 去 argv 暂存 + nregs 局部 + CallHost 直接索引，双模式全绿（新增 `tests/frame_packing.rs` 4 项）。**排查**：Criterion 命名基线严格 A/B（同会话紧邻两次，各 30 样本）fib 157.06→157.10µs 完全持平；sum_to 的 1.68 vs 1.84 差异出现在不执行 CallScript 的循环上，属布局噪声；Rust 基线同期波动 22% 证明机器噪声大。**解决**：fib 是同函数递归、窗口恒定，收益被调用开销淹没；为零收益引入 `u32↔usize` 转换与 base/ip 溢出面不划算，回退。**影响**：微架构瘦身对「指令数 × 固定开销」瓶颈无感，真正有效的仍是 ①a/③/⑤ 这类减少指令或消除检查的项。
- **D-025 · ⑥b 预分配 regs 双向 A/B 信号矛盾回退**：`Vm::new` 按 `max_nregs × max_depth`（上限 2MiB）`with_capacity` 预留 regs，免深递归重分配，debug + release 54 项全绿。**排查**：Criterion 命名基线双向 A/B（各 100 样本）——正向 pre→post fib 151.60→142.47µs（−5.1%）看似正收益；反向 post→pre fib 146.92 vs 151.73µs（p=0.86 不显著）信号消失。同期 Rust 基线自身两向摆动 −1.4%/+4.0%（5.4 个百分点），无关基准（compile/decode，不触及 `Vm::new`）摆幅 ±37%。**解决**：两方向符号相反、控制基线抖动超过被测效应，判为机器噪声主导，回退。交接稿亦预警本项很可能负收益。**影响**：性能结论必须双向验证——单向「改善」可能是基线漂移；当控制基线自身抖动 > 被测效应时，任何单向结论都不可信。性能逐项优化八项至此全部评估完毕。

- **D-026 · JIT 路线证据复核推翻 ADR-001 的事实前提**：roto 真机报告 + AOSP sepolicy 官方源码（`allow appdomain self:process execmem;`，commit `ad7df7b` / `public/app.te`；`app_neverallows.te` 无 execmem 禁令）证明「Android 无法执行动态生成的机器码」是错的；报告 §9「ColorOS 太宽松、不能外推到 AOSP」方向也反了——策略 B 本就是 AOSP 设计本意，真正依赖 OEM 的是直接 RWX（有策略 B 兜底）。**排查**：为避开二手博客误导，以官方源码原文为准；用受控实验量化成本——最小 cdylib（aarch64、strip）0.28→2.09 MiB（+1.81 MiB / 6.4×），真实项目 roto 3.87 MiB vs brook 0.76 MiB。性能同口径：fib(20) VM 411.5 µs vs JIT 42.8 µs（9.6×）；sum_to(200) VM 1.73 µs vs JIT 131 ns（13.2×，**反转了「循环场景收益小」的预判**）。冷启动：JIT 完整编译 721 µs（其中解析+类型检查 200 µs）、Runtime 初始化 471 µs；VM 编译 8.6 µs / 缓存解码 1.8 µs → fib 2 次（含初始化 3.2 次）、sum_to 445 次（含初始化 740 次）调用回本。**解决**：修正 1.3 与 ADR-001 事实表述、新增 Q11 与 ADR-012 草案；架构决策仍维持 VM 优先，JIT 从「平台不可行」改为「成本权衡暂不上」。**影响**：排除性事实断言必须回到一手源码核对；成本直觉（包体）经受控实验证伪后，整个架构权衡可能翻转。所有临时基准文件与 `Cargo.toml` 改动均已还原，两个项目源码零残留。

- **D-027 · 深度自查修复静默错值与边界绕过**：按自查优先级依次修复并逐项全量回归。① `lexer.rs` 字符串字面量改为收集原始字节后一次性 UTF-8 解码，修复中文字符串乱码与 `len("中文")==6`；② `codegen.rs` 让发散 if 分支继承非发散分支类型，修复 `then { return ... } else { f64 }` 后续运算按整数解释得到 `inf` 的静默错值；③ `typecheck.rs` 禁止 Tuple/Unit 的 `==`/`!=`，避免把 heap index 当值比较；④ `to_str` 限制为 i64/f64/bool/str，避免输出内部堆索引；⑤ `vm.rs` 每个 heap entry 计入固定开销，堵住零长字符串/空对象绕过 `with_heap_limit`；⑥ 宿主函数重名注册直接报错，避免 `.sbc` fingerprint 与孤儿 slot 不一致；⑦ `compile_cached` 缓存命中覆盖 `Script.name` 为本次调用名；⑧ 补除零、取模零、`max_depth`、UTF-8、发散分支、Tuple/Unit 相等、`to_str` 边界、空对象配额、重复注册、缓存名、crate 级快速上手 doctest 共 11 类回归与文档测试。**影响**：性能相关代码除 heap 计费定点修补外未动，优化结论不变；测试数从 54 增至 64。临时探针文件均已删除，最终 release 全量 64 项通过。

- **D-028 · 诊断能力四件套（性能零影响、维护面可控）**：① **字符列号**——`lexer.rs::bump` 只在 UTF-8 首字节推进 `col`，续字节不计数。此前含中文的行按字节算列，`"中文" + 1` 报 2:22（人眼是 2:18），照着报错数永远数不准。② **调用栈**——`Error.trace: Vec<TraceFrame>`，由 `Vm::call` 在 `run` 返回 `Err` 后回放帧栈生成：最内层用错误自身的 span，外层用被调用点保存的恢复偏移（`code[ip-1]` 正是 call 指令）。**热路径零成本**：成功路径多一个 `match`，帧栈只在已经失败时才走一遍。③ **源码片段渲染**——新增 `src/diag.rs`（约 190 行，独立模块）：`Error::report(src, name)` / `Script::report(&err)` 输出 `--> 文件:行:列` + 源码行 + caret；caret 按**显示宽度**对齐（tab=4、CJK=2），深栈超过 12 帧省略中段。`Script` 新增 `source: Rc<str>`，缓存命中时由 `compile_cached` 补回，`.sbc` 本身不存源码。④ **宿主错误类型透传**——`CallHost` 不再把宿主错误压成 `ErrorKind::Runtime`。**为什么现在做**：这四项只碰错误路径和编译期，`Display` 保持单行（C ABI 错误缓冲区形状不变），`.sbc` 格式与版本不动，因此不触性能、不改缓存兼容性。**为什么不做**：运行时 `Span.start/end` 恒为 0（指令只存 line/col 两个 u32，是为分派循环体积换来的），所以运行时错误只有位置、没有高亮范围——这是设计权衡，不补。**验证**：Criterion 无回退（fib(18) 144.7µs vs 文档基线 157.9µs，sum_to(200) 1.54µs vs 1.63µs，编译 8.02µs vs 7.96µs，均在噪声内）；测试 107→118（新增 `tests/diagnostics.rs` 11 项：字符列号、ASCII 回归、四层调用栈位置精确断言、深栈截断、tab 与 CJK 的 caret 对齐、缓存脚本可渲染、宿主 `ErrorKind` 透传）。

- **D-029 · 热函数里的错误构造必须冷提起（嵌套深度守卫的性能教训）**：给 `Parser` 加 `MAX_NESTING` 时，第一版把 `format!` 的错误构造直接写在 `enter()` 内。逻辑上「成功路径只多一次自增与比较」，但 `compile fib.bk` 实测 8.0→9.2 µs（+15%）。A/B 关掉守卫复测得 8.44 µs，确认差异来自守卫而非噪声——`enter()` 里塞进 `format!` 后函数体积超出内联阈值，递归下降的热函数不再内联，代价远超那次比较。**解决**：错误构造外提为 `#[cold] #[inline(never)] fn depth_error()`，`enter()` 加 `#[inline]`，回到 8.39 µs，守卫实际免费。**影响**：这类判断不能靠静态推理，必须 A/B——尤其当 `format!`/`vec!`/字符串拼接出现在被高频调用的函数里时；同一函数只要有一条冷分支把体积顶过阈值，整条热路径都会陪葬。同时保留一条方法论：A/B 要「关掉被测物」而不是「改别处」，否则测到的是机器漂移（本机 `compile` 基准历史摆幅曾达 ±37%，见 D-025）。

**验证（当时的阶段快照）**：当时 40 个测试全绿；`cargo build --release --lib` 零警告；`cargo ndk` 双 ABI 交叉编译通过；`readelf` 校验 ELF 架构与导出符号；Windows 本机 `cargo run --example android_cli` 输出 `brook 0.1.0 / x86_64: check(Order{200,vip}, 1000) = (160, true)`，脚本逻辑等价验证通过。**本机 `adb devices` 为空、SDK 无 emulator 二进制与 AVD，Android 真机/模拟器运行未执行**（Q10）。


**下一步**：① 连设备或装好模拟器后执行 `./scripts/android_build.sh` 完成 Q10；② 如需 Java/Kotlin 层，可在现有 C ABI 上做 JNI 桥（`System.loadLibrary` + native 方法）；③ Q8 的递归/循环性能改按 Q11 与 ADR-012 草案重新评估 JIT（原 ADR-001「Android 不可行」的事实前提已证伪），阶段 4 AOT 仍是备选。

---

### 性能逐项优化（2026-11-20，一次一项、每项跑 Criterion 后才定下一项）

采用交接稿的执行序：①a → ② → ③ → ④ → ⑤ → ①b → ⑥a → ⑥b。原则：不改语言语义、不顺手改风格、每项单独提交判断。

**①a · Assign 目标寄存器直写**（已落地）
- **问题**：`x = x + 1` 先算到临时寄存器再 `Mov` 回 x，循环体多一条指令。
- **做法**：新增 `compile_expr_into(expr, hint)`，让 Binary/Unary/Call/字面量直接把结果写进目标槽；`IfExpr/Tuple/Index/BlockExpr` 忽略 hint 走原路由调用方补 `Mov`。`compile_binary`/`compile_call` 加 `dst_hint: Option<Reg>`，子表达式仍走 `compile_expr`，hint 不向下传。所有 handler 先读操作数后写 dst，同槽读写安全。
- **影响**：sum_to 稳态 5→4 条指令、nregs 6→5；fib 未变（实际源码用显式 `return`，交接稿假设的共享 Ret 不适用）。Criterion 2.35→2.12µs。新增 `tests/dst_hint.rs` 4 项。

**② · validate(program) 出生点校验**（已落地）
- **问题**：unchecked 寄存器访问和裸函数索引需要前置保证，否则损坏的 `.sbc` 会 UB。
- **做法**：新增 `src/validate.rs`，覆盖当时全部 65 opcode（当前 71）的 `Reg < nregs`、跳转 `target < code.len()`（严格，不许 ==）、`FuncId`/`ConstStr.id`/`CallBuiltin.id`/`Regs.len() <= 8`。接在 `Engine::compile` 与 `cache::decode` 两个 Program 出生点，**不放 `Vm::new`**（每 call 新建，O(code) 会吃热路径）。
- **影响**：测试 44→47；安全项，性能无显著变化。新增 `tests/validate.rs` 3 项。

**③ · 寄存器访问 unchecked**（已落地）
- **问题**：每次寄存器读写都有边界检查，sum_to 稳态 6 读 3 写 = 9 次检查。
- **做法**：`src/vm.rs` 新增 `RegFile` 新类型，`Index`/`IndexMut` 用 `get_unchecked` + `debug_assert`，`Deref` 转发 `len`/`resize`/`clear`。分发循环约 100 行访问代码零改动，`code.get(ip)` 指令抓取保留（9:1 取舍，避免整类 ip 越界 UB）。安全前提：② 保证 `Reg < nregs`，`call`/`CallScript` 保证 `base + nregs <= len()`。
- **影响**：debug + release 双模式 47 项全绿。Criterion fib 206.55→**177.60µs**（1.16×）、sum_to 2.08→**1.63µs**（1.28×）；sum_to 折原生 5.8× 进 3–8× 目标区。

**④ · gas 只按控制流步计费（回退，D-022）**
- **设想**：计费从每指令挪到分支/call，直线指令不扣，减少检查次数。
- **实测**：两种实现（集中 `is_metered` 判断 / 内联到 17 个分派臂）都负收益。Criterion 命名基线 A/B、同机连续各采样 25 次：fib 191.91→247.99µs（慢 29%）、sum_to 1.64→2.81µs（慢 71%）。v4 那轮 Rust 基线反而更快（7.71 vs 8.63µs），排除机器噪声。
- **结论**：③ 的每指令检查是循环顶部一条**无条件、可静态预测**的直线代码，分支预测器近 100% 命中，成本接近 0；④ 把 17 个带 `return Err` 冷路径和 span 计算的判断散进分派臂，污染分支预测与指令缓存布局，抵不过省掉的检查。已回退到每指令扣减，`max_steps` 语义文档同步改回。

**⑤ · `JmpIf*I64Imm` 融合立即数比较**（已落地）
- **问题**：`if n < 2` 这类「变量 OP 整数字面量」的条件，旧路径先 `ConstI64 r1,2` 把字面量装寄存器，再 `JmpIfGeI64 r0,r1`——多一条指令、多一个寄存器、多一次读。
- **做法**：在 `compile_cond_false` 里**先匹配 AST 形状**（`Binary{cmp, lhs, Expr::Int(n)}` 且 `is_i64_cmp`），直接发射取反的 `JmpIf*I64Imm { a: lr, imm: n }`，字面量根本不物化。照 `AddI64Imm` 模式，`vm.rs` 加 6 个分派臂（一次寄存器读 + 立即数）。`patch_jump` / `validate.rs` 同步识别 6 变体。`cache.rs` opcode 表 65→71（tag 66–71，不重编号），Wire 加 `imm2: u64` 同时容纳 target 与立即数，**`VERSION` 1→2**（旧 `.sbc` 正确失效）。类型安全由 `unify(lt, Int)` 保证 lhs 必为 I64。
- **影响**：fib 条件 2 条→1 条、nregs 11→9、整函数 14→13 条；`cmp`/`gt` 同样 2→1。Criterion 命名基线 A/B：fib 170.43→154.92µs（1.10×，同机 30 样本）；正常态复测 177.60→157.93µs。**sum_to 不变**（`for i in 0..n` 是变量边界，不融合，符合交接稿预期）。新增 `tests/imm_branch.rs` 3 项（6 种比较 + 边界/负立即数 + 回边 patch + fib 递归）。

**①b · 尾位置 Ret 复制（跳过）**
- **交接稿设想**：if 结果直接喂 Ret 时两分支各自 Ret 自己的值，省掉共享 result + 两条 Mov，fib 叶 5→2 条。
- **本仓库不成立**：`fib.bk` 用显式 `return n`，`if n<2` 的 then 分支自己 Ret；函数返回值来自尾部 `fib(n-1)+fib(n-2)`。`compile_if` 结果寄存器 `r1` 从不被读，共享 Ret 属于尾部表达式——fib 根本不产生「尾位置 if 喂 Ret」模式。`sum_to` 是 `for` 循环，同样不命中。字节码 dump 实证（见下方开发日志）。
- **结论**：收益为零却要引入 remap 表 / 跨基本块判定 / 副作用落点判定这类最容易静默算错的改动，不划算。跳过，把预算留给命中热路径的项。

**⑥a · call/ret 瘦身（实现并严格 A/B 后回退）**
- **实现**：Frame 字段 `usize`→`u16/u32` 压到 16B（原 40B）；去 CallScript 的 argv 暂存（先 resize 再原地拷，源 `[base,nbase)` 与目标 `[nbase,nbase+n)` 不重叠由 validate 保证）；`nregs` 提局部变量并在 Ret 恢复；`program.get(func)` 校验后改直接索引。
- **验证**：新增 `tests/frame_packing.rs` 4 项（不同 nregs 的嵌套/兄弟/混合递归/多参数），debug + release 双模式全绿。
- **实测**：Criterion 命名基线严格 A/B（同会话，`v6a_ctrl`=pre 紧邻 `v6a_after2`=post，各 30 样本）：**fib 157.06→157.10µs（完全持平）**；sum_to 的 1.68 vs 1.84 差异出现在不执行任何 CallScript 的循环基准上，属代码布局噪声。Rust 基线同期 8.32→6.79µs 波动 22%，证明机器噪声大。
- **结论**：fib 是同函数递归、窗口大小恒定，Frame 压缩与去 argv 的收益被调用开销淹没，测不出。为零收益改动引入 `u32↔usize` 转换与 base/ip 理论溢出面不划算——回退到 argv 暂存版本。`frame_packing.rs` 作为通用正确性测试保留。

**⑥b · 预分配 regs（实现并双向 A/B 后回退，D-025）**
- **实现**：`Vm::new` 由固定 `Vec::with_capacity(64)` 改为按本程序 `max_nregs × max_depth` 预留（上限 256K 槽 = 2 MiB；`RegFile(Vec::with_capacity(reserve))`，下限 64）。`with_capacity` 只保留地址空间、不触碰页，浅调用不付 memset；超限的程序回退到原有按需 resize 分支（保留作安全网，上限内永不触发）。目标：消掉深递归时 `CallScript` 的 Vec 重分配。
- **验证（当时的阶段快照）**：debug + release 双模式 54 项全绿（含 `frame_packing.rs` 不同 nregs 的嵌套/兄弟/混合递归）。
- **实测**：Criterion 命名基线**双向** A/B（各 100 样本，同会话）。正向 `v6b_ctrl`(pre) → post：fib 151.60→142.47µs（−5.1%）、sum_to 1.53→1.50µs（−2.6%），看似正收益。**反向** `v6b_after`(post) → pre：fib 146.92 vs 151.73µs（−0.2%，p=0.86 不显著），信号消失。同期 Rust 基线正向 −1.4%、反向 +4.0%（同一份未改动代码摆动 5.4 个百分点），无关基准（compile/decode，`Vm::new` 根本不触及）摆幅达 ±37%。
- **结论**：两方向符号相反、控制基线自身抖动超过被测效应，判定为机器噪声主导，无可信收益。交接稿亦预警本项很可能负收益（预分配在浅调用上多占内存、深递归重分配本就稀少）。按 ④ / ⑥a 同一纪律回退，`vm.rs` 恢复 `with_capacity(64)`。预分配作为「已知无效」候选归档，不再重试。

**验证（当时的阶段快照）**：当时 54 项测试全绿（⑤ 后 + `frame_packing.rs` 4 项）；release 构建通过；Android 双 ABI `.so` 重新打包通过。**已落地净收益项**：①a / ② / ③ / ⑤；④ / ⑥a / ⑥b 经 A/B 实测回退，①b 跳过。
**下一步**：交接稿八项已全部评估完毕——落地 ①a / ② / ③ / ⑤，回退 ④ / ⑥a / ⑥b，跳过 ①b。解释器微优化告一段落；递归/循环的进一步提速改走 Q11 / ADR-012 草案的 JIT 路线评估（同口径 fib 9.6×、sum_to 13.2×，冷启动分别 2 / 445 次调用回本），AOT 仍为备选且与热重载互斥。

### JIT 路线复核（2026-11-20，五轮证据，决策草案见 ADR-012）

| 轮次 | 问题 | 方法 | 结论 |
|---|---|---|---|
| 1 | Android 到底让不让第三方 app 做 JIT | AOSP 官方 sepolicy 源码 + roto 真机报告 | AOSP 明确 `allow appdomain self:process execmem;`（commit `ad7df7b` / `public/app.te`，为 WebView / ART JIT 准备）；`app_neverallows.te` 全文无 execmem 禁令；真机策略 B 通过。原「无法执行」命题证伪 |
| 2 | Cranelift 包体代价多大 | 两个最小 cdylib、同 aarch64 / `lto=thin` / `codegen-units=1`，唯一差别是真实调用 JIT | strip 后 0.28→2.09 MiB，净增 **+1.81 MiB/ABI（6.4×）**；双 ABI 约 +3 MiB，可忽略 |
| 3 | 递归场景快多少 | fib(20)，双方 fib(0)=1 / fib(1)=1 调用树相同，同机 release + Criterion 100 样本 | VM 411.5 µs vs JIT 42.8 µs = **9.6×** |
| 4 | 冷启动会不会吃掉收益 | 同口径测完整编译 / Runtime 初始化 / VM 编译与缓存解码 | JIT 编译 721 µs（解析+类型检查 200 µs）、初始化 471 µs；VM 编译 8.6 µs、缓存解码 1.8 µs；fib **2 次**调用回本（含初始化 3.2 次） |
| 5 | 循环场景是否真没收益 | sum_to(200) 同 while 语义，同机 Criterion 100 样本 | VM 1.73 µs vs JIT 131 ns = **13.2×**（反转了"循环收益小"的预判）；回本 **445 次**（含初始化 740 次） |

**结论**：原先排除 JIT 的四条理由——「Android 不可行」「包体太大」「冷启动太贵」「性能收益不确定」——经一手源码与受控实测后全部不再成立或可忽略，账本明显偏向 JIT。剩余成本只有 Cranelift 依赖、Android 编译 +17 分钟、双后端维护、execmem 探测兜底与 ARM 真机未测。**当前决策**：仍维持 VM 优先（ADR-001 修订 + ADR-012 草案，草案未采纳），待真实业务延迟预算触发原型；不因为数据好看就直接替换解释器。

### 2026-11-21 · 代码审查修复：嵌套深度守卫绕过 + 句柄相等语义（D-030）

**背景**：对全部 19 个源码模块做静态审查（本机 Bash 工具故障，未能运行 cargo），发现两处 P1 缺陷并修复。

- **问题 ①（P1）· if 表达式的 else-if 链绕过 MAX_NESTING**：`tests/depth.rs` 的 `long_else_if_ladder` 只覆盖语句位置（`parse_if_stmt` 有 `enter()`）；表达式位置走 `parse_primary` 的 `If` 臂 → `parse_if_expr` → `parse_primary`，全程无深度计数。`let x = if a {1}` 接 N 个 ` else if a {1}` 的源码每层递归约 2–3 个栈帧且深度计数恒为 0，深链直接进程栈溢出 abort（catch_unwind 抓不住）——正是 MAX_NESTING 要防的场景；typecheck / codegen / AST `Drop` 会沿同一棵树再次递归。
- **解决**：`parse_primary` 的 `If` 臂入口 `self.enter(span)`、成功返回前 `depth -= 1`（错误路径解析器整体丢弃，泄漏无害——沿用 `enter` 既有约定）。每个 if 表达式计 1 层深度，语句位置语义不变。
- **问题 ②（P1）· `Op::EqHandle` 比较的是堆槽位下标而非对象身份**：`value_to_slot` 对每个实参/宿主返回都新 push 一个 `HeapObj::Handle` 且不去重，同一个宿主对象经两个实参传入（或两次宿主调用返回）时落在两个槽位，`a == b` 返回 false——静默错值。而 `value.rs` 的 `Handle: PartialEq`（type_name + `Rc::ptr_eq`）与 `ir.rs` 注释宣称的 "pointer identity" 才是意图语义。
- **解决**：VM 新增 `heap_handle` / `handle_eq`，从堆取出两个 `Handle` 后直接用其 `PartialEq`（`Rc::ptr_eq` + 类型名），非 handle 槽位按既有 `heap_str` 惯例报 `expected handle in slot`。比较走堆访问，非热路径（`fuse_branch` 本就刻意不融合句柄比较）。
- **测试**：`tests/depth.rs` 新增 `long_else_if_expression_ladder_is_rejected`（1000 级表达式阶梯必须报 nesting too deep），`everyday_nesting_still_compiles` 补 20 级 if 表达式阶梯正向用例（含求值断言）；新增 `tests/handle_equality.rs` 4 项（同对象双实参 `==` 为 true / `!=` 为 false、不同对象 `==` 为 false、脚本内拷贝保持同一性）。
- **影响**：parser.rs / vm.rs / ir.rs（注释）/ tests/depth.rs / tests/handle_equality.rs。审查同时发现的 P2/P3 见第八章 Q13/Q14（同日 D-031 已解决；内置名遮蔽与 `()` 字面量两个毛边记为 Q15 待决策）。本机 Bash 工具当时故障，shell 恢复后实测：159 项全绿（含本轮新增回归）。

### 2026-11-21 · Q13/Q14 落地：参考解释器对齐 VM + 块值类型统一 + validate 硬化（D-031）

**Q13（eval.rs 与 VM 分歧）**
- **问题**：`eval_binary` 只有 (I64,I64) 与 `as_f64` 浮点两条路径——bool 等值、str 比较、句柄等值、乃至字符串 `+` 拼接全部落入 `as_f64` 报 "expected f64, found …"（实现时确认拼接也在内，范围比审查记录更大）；`eval_call` 还把宿主错误压成 runtime，而 VM 侧 D-028 已透传 ErrorKind。eval 是公开导出的 `pub mod eval`，宿主直接用它跑合法脚本会得到错误报错。
- **解决**：`eval_binary` 在整数路径后依次补 bool（Eq/Ne）、Handle（Eq/Ne，复用 `Handle: PartialEq`，与 VM 修复后的 EqHandle 语义一致）、Str（`+` 拼接 + 六种比较）三个分派臂；排序落到不支持类型时报 `ordering requires numbers or str`（与 typecheck 措辞一致）。`eval_call` 的 map_err 改用 `Error::new(e.kind, …)` 保留宿主错误类型。
- **测试**：新增 tests/eval_parity.rs——同一脚本分别跑参考解释器与 VM 断言同结果（bool 等值、str 拼接/比较、句柄同一性），另验宿主 Io 错误在两引擎都保留 `ErrorKind::Io`。

**Q14（typecheck/codegen 块值类型分歧 + validate 硬化）**
- **问题**：checker 对「无 else 的 if 语句」与「赋值语句」记 Unit，codegen 却传播 then 分支 / 变量类型——被 sig 边界掩盖成静默丢值，更隐蔽的是 `let x = { y = 2 }` 这类 BlockExpr 场景会把 x 绑定成错误类型，造成 codegen env 与 checker 漂移（虽然 checker 的 Unit 语义会先拒绝误用）。
- **解决**：统一走 checker 语义（与 Rust 语句位置 if 一致）：`compile_if` 在 otherwise 为 None 时返回 Unit；`compile_stmt(Assign)` 返回 `(slot, Unit)`。对边界行为零变化（sig 本来就赢），纯粹消除内部不一致。
- **validate 硬化**：① CallScript 校验**实参数不超过被调窗口 nregs**——损坏 .sbc 的实参数超过窗口时，旧代码在 argv 拷贝处 Vec 越界 panic（靠 catch_unwind 报误导性的 internal panic），现在是干净编译错。**教训**：首轮误用语义约束 `== callee.params.len()`，被 cache.rs 的 every_opcode_roundtrips 测试当场抓回（codec 合成程序合法地不满足语义 arity：CallScript 带 3 实参调 0 参数函数，但 3 ≤ 窗口 8 完全结构安全）；validate 的职责是结构安全，「语义正确性归 checker」这条边界本身也要守住。② cache.rs `validate_wire` 收紧到与 validate.rs 同级：register 用 `>=` nregs、跳转 target 用严格 `<` code_len（此前 off-by-one 被第二道校验兜住，属防御深度不一致）。
- **顺手清理**：codegen 五处（实质不可达的）编译期错误从 `runtime_at` 改为 `type_error`——Runtime kind 会让 C ABI 把编译失败映射成 `MS_ERR_RUNTIME`；Engine 文档删掉过时的 "Cheap to clone"（无 Clone 实现），注册失败提示改为 "host registry is already shared"；lexer 对非法字符解码完整 UTF-8 序列再显示（此前 `c as char` 把中文 lead byte 显示成 Latin-1 乱码）。
- **刻意不做**：内置函数名遮蔽用户脚本函数涉及语言行为变化，与 `()` 字面量产生 `Tuple([])` 一并记入 Q15 待决策，不动。
- **影响**：eval.rs / codegen.rs / validate.rs / cache.rs / engine.rs / lexer.rs / tests/eval_parity.rs / tests/validate.rs。**实测 159 项全绿**（157 单测/集成 + 2 doctest + 1 ignore 示例）、release 构建零警告。

### 2026-11-21 · Q10 真机验证完成 + android_build.sh Git Bash 修复（D-032）

**验证环境**：PLR110 真机（OP6117L1），Android 16，arm64-v8a，SELinux **Enforcing**——第三方 app 运行自有二进制的最严格常规环境。产物为 cargo ndk 重建的双 ABI 版本（含本轮全部修复）。

**四项实测**：

1. **默认 smoke test**：`check(Order{200,vip}, 1000) = (160, true)`，exit=0——宿主 `Opaque<Order>` 句柄 + 类型化回调 + 元组返回的完整管线（lexer → parser → typecheck → codegen → VM）在真机全通。
2. **扩展脚本**：递归 fib(12)=144、循环 sum_to(100)=4950、字符串拼接、元组索引、to_f64、else-if 链 → `(5254, true)`，exit=0。**插曲**：脚本注释把期望值写成 5354，实测 5254——错在验证者而非 VM：`for i in 0..n` 是左闭右开区间，sum_to(100) = 0+…+99 = 4950。范围语义与 Rust 一致，这次"期望算错"反而实测确认了语义。
3. **除零错误路径**：`Error { kind: Runtime, message: "division by zero", span: 9:18, trace: [check] }`，exit=1——错误定位 + 调用栈在真机可用。
4. **gas 止损**：`while true` 无限循环被 10M 步上限终止（`step (gas) limit exceeded`），exit=1，设备未挂死。

**顺带确认的边界校验**：第一版验证脚本 `check(order, limit)` 的函数体未使用参数 → 推断默认收敛 i64 → 收到 Handle 实参被 `argument type mismatch: expected i64, got Order` 拒绝——D-008 防静默算错的边界校验在真机同样生效。脚本补上 `: Order` / `: f64` 标注后按预期走到错误路径本身。

**构建与产物**：cargo ndk 双 ABI 重建（arm64 14.1s / armv7 10.6s），jniLibs 刷新为含全部修复的版本；llvm-readelf 复验 ELF64/AArch64、ELF32/ARM，5 个 `ms_*` 导出符号齐全。

**问题与解决**：`scripts/android_build.sh` 一键路径在 Git Bash 下 push 失败——MSYS 把 `/data/local/tmp/...` 改写成 `C:/Program Files/Git/data/...`（报 remote secure_mkdirs failed）。解决：push 命令加 `MSYS_NO_PATHCONV=1` 前缀（Git-for-Windows 专用变量，Linux/macOS 下无副作用）。**注意**：该变量会连本地路径转换一并关闭，push 的本地源路径需写 Windows 风格（`C:/...`）。

**清理**：设备端 4 个测试文件（android_cli + 3 个 .bk）已删除，本地临时脚本目录已删除；jniLibs / target 为项目产物按原样保留（jniLibs 由修复前旧字节刷新为当前代码，属测试的预期产出）。

### 2026-11-21 · C ABI 句柄式脚本层落地（D-033）

**背景**：审查建议的交付链路补全第一步——`ms_call_i64` 每调用重编译（700µs 级），真实宿主需要保留编译产物；`compile_cached` 的 assets 路径缺 C 入口；`Script::report` 的多行诊断出不了 C 边界。设计与决策见 ADR-015，本条记实现中的问题。

**问题与解决**：

- **D-033-1 · `ms_last_report` 初版声明为 safe 函数**：`cargo` 的 "unnecessary unsafe block" 警告在调用侧抓出来——函数体内做调用方指针写入，却不需要调用方写 unsafe，意味着 Rust 宿主**不经过 unsafe 就能传伪造指针触发 UB**，是健全性漏洞。修为 `pub unsafe extern "C"`，与 `ms_last_error` 协议一致。教训：FFI 每个导出都要明确「调用方必须承诺什么」，安全声明是 API 语义的一部分，不是编译器会给的。
- **D-033-2 · 测试断言两处自摆乌龙**：字符串 `"hi, 中文!"` 字节数心算成 9（实为 4+6+1=11，CJK 按字节计）；null 句柄的预期错误码写成 `MS_ERR_THREAD`（`script_ref` 先查 null → 实为 `MS_ERR_NULL`）。cargo 断言期抓回。
- **D-033-3 · `Failure` 加 `report` 字段连锁**：`ms_call_i64` 里两处裸 struct 字面量缺新字段编译失败；顺带把一次性 API 也接上 report——source 就在手边，失败时即刻渲染，C 侧两种层级的诊断能力对齐。

**真机验证（PLR110，Android 16 / arm64）**：`android_cli` 扩展段全过——`create_cached` 落盘缓存（806 字节）且二次运行命中；`add3(1,2,3)=6` 多参数；`fib(15)=610` 保留字节码递归；`describe(98.6)="score=98"` 字符串结果含**小 buffer 拒绝→增长重试**；`missing()` 错误定位；**reload 后 add3=7**（热重载经 C ABI 生效）；free + null-free。一次性 API 的 `check(Order{200,vip}, 1000) = (160, true)` 照常通过，exit=0。

**清理**：设备端 `android_cli` 与 `ms_cli_cache.sbc` 已删除；jniLibs 刷新为含句柄层的版本（项目产物保留）。

**下一步**：② Q15 语义毛边（内置名遮蔽报编译错、`()` 归一 Unit）；③ 零依赖 fuzz 冒烟；之后按业务信号决定是否启动 rhai 迁移试点 / Q11 JIT 原型。

### 2026-11-21 · Q15 解决 + 零依赖 fuzz 冒烟（D-034）

**Q15①（内置名保留）**：typecheck Pass 1 在宿主碰撞检查后追加 `is_builtin` 检查，报 `[type] function … is a reserved built-in name`。注意 `infer_call` 本就内置优先，所以此前被遮蔽的函数只有入口点可达——这不是理论问题，rhai 迁移如果带进一个叫 `len` 的脚本函数，会在静默错行为里埋很久，编译期拒绝是对的。

**Q15②（`()` 归一 Unit）**：四处统一——typecheck `Expr::Tuple([]) → Ty::Unit`、codegen 空元组改发 `ConstUnit`（不再分配空堆元组）、eval 同步、`parse_type` 的 `: ()` 注解即 Unit 类型。`.sbc` **VERSION 2→3**：wire 格式没变，但旧编译器对含 `()` 的源码产出的 `Tuple([])` 语义会在缓存里存活到源码变更为止，版本门强制重建比「文档提醒」可靠。顺带确认：`Value::ty()` 对宿主传入的空 `Value::Tuple` 仍是 `Tuple([])`——宿主侧边界极少出现，保持不动，`type_matches(Tuple([]), Unit)` 为 false 属预期。

**D-034-1 · 测试脚本自踩 Q7 语义**：`abs(x) + len("ab")` 类型错误——`abs` 无整数证据默认 f64 族（Q7），与 i64 的 `len` 混型被检查器拒绝。**检查器行为完全正确**，改用类型一致的两个函数。又一次「失败的是验证者」。

**fuzz 冒烟的设计取舍**：完整 cargo-fuzz 需要 POSIX 目标，与 Windows 本机摩擦大；改成纯 Rust 确定性循环（xorshift64* 固定种子，失败可精确复现）——5000 轮 `.sbc` 字节变异（翻转/截断/拼接/覆写）+ 头部门控专项 + 3000 轮随机 token 源码 + 200 轮随机长度深度压力（五种嵌套形状，明确覆盖 else-if **表达式**阶梯——D-030 修过、depth.rs 只测过语句阶梯的那条路径）。覆盖的是「变异输入永不 panic / 深嵌套永被拦截」这两类最高频问题；libFuzzer 级别的覆盖将来放 CI。实测全套 0.2s，可以进每次 `cargo test`。

**验证**：173 项全绿（171 单测/集成 + 2 doctest）、零警告；既有全部脚本测试在「内置名保留」与「() 归一」下无回归——包括 rhai 迁移可行性测试，确认无存量依赖。**真机复验**（PLR110）：重建产物两次 smoke test 全过（v3 缓存写入 + 二次命中，pull 校验头 `4d53 4243 0300`）；`func len(s)` 在设备上报 `reserved built-in name`（4:1 定位，exit=1）；`() ` 字面量 + `: ()` 注解经 C ABI 往返显示 `()`。测试遗留已清理。

**下一步**：建议 ③ 完成后进入「rhai 迁移试点（2-3 个真实脚本）或 Q11 JIT 原型」的业务决策点；工程侧剩余项为 CI（test + Android 交叉编译矩阵）与 libFuzzer 级 fuzz 放 CI。

### 2026-11-21 · rhai_demo / android_so_demo 迁移评估 + so_demo 改写 POC（D-035）

**背景**：用户要求对 rhai_demo（PC 宿主，229 注册 / 34 脚本 6905 行）与 android_so_demo（Android .so，232 重载 / 50 脚本）做迁移测试：读源码、盘点暴露函数、评估极简引擎能否接住。两个 Explore 代理产出全量清单（存档 `docs/migration/{rhai_demo,so_demo}_inventory.md`）。

**关键发现（数据驱动）**：
1. **ADR-013 的「2 元组限制」是桥接层缺口，不是语言缺口**——脚本语言一直支持 N 元组（≤8），只是 host.rs 只实现 2 元组桥。`impl_tuple_n!` 宏一处补齐 2–8 元。这条限制解除后，两个宿主的大半 Map 返回可直接元组化。
2. **typecheck 索引反推缺续推**：`func f(t) { t.0 + t.1 }` 在第二索引处误报 `tuple index 1 out of range (len 1)`——首次 `.0` 已把参数反推成 1 元元组，后续更大索引只查不扩。ADR-006 记录的语义是「至少 N+1 元」，实现只做一次。修复：元组长度不足且 base 仍是推断变量时，重写 subst 扩长（运行时 `type_matches` 本就接受更长元组，两边对齐）。POC 测试当场抓到，属真实语言修复非测试问题。
3. **两个宿主画像差异大**：rhai_demo 45 Map + 22 Array 返回、22/34 脚本用 try/catch（253 块）、diag.rhai 1097 行行为回归网；so_demo 19 Map + ~55 Array 返回但脚本侧 **Map 字面量 0、闭包 0、try/catch 仅 2 个实质脚本**、脚本小（最大 128 行）、契约简单（`OK\t`/`ERR\t`，末值丢弃）。
4. **宿主消费方式利好**：rhai_demo 的 runner 对脚本返回值只 `to_string()` 打印（数据走 stdout+文件）；so_demo 末值直接丢弃。**两者都不依赖返回值结构化**——结构化数据全部经由宿主函数交换，这正是元组化/Opaque 化可行的前提。

**POC（tests/migration_so_demo.rs，4 项全过）**：忠实改写 so_demo 两个真实脚本——
- `t_wifi.rhai` → 顶层语句包 `main()`，标量面直接接住；`OK\t/ERR\t` 契约模拟（编译错与宿主 Err 双通道）；root 总闸切换验证同一字节码随宿主状态变化。
- `bind_verify.rhai` → `img_screen_size/img_crop` 的 Map → **(w,h)/(x,y,w,h) 元组**；`img_find_all/img_find_bind` 的 Array<Map> → **Opaque + count/name/x/y 索引 getter**；try/catch(win<0) → **(ok, err) 探针二元组**；`to_string()` → `to_str()`；9 行日志逐一断言与原流程一致。API 形状改写记录：`img_find_bind` 的"伴随模板三元组数组"参数在全部 3 个真实调用点都只传 1 个伴随，标量化属过度泛化的回收。
- Task 句柄 → Opaque + RefCell/Cell 内部可变性（rhai `&mut Task` 方法调用的对应物）。
- 2–8 元组往返直测（返回值、`.N` 索引、元组字面量作宿主实参、FromValue 校验）。

**D-035-1 · 测试脚本自踩桥接语义**：`register_fn!(engine, "sum3", ((i64, i64, i64)) -> i64, …)` 的 `((…))` 被 `$a:ty` 捕获为**一个元组类型参数**——脚本须传元组字面量 `sum3((a, b, c))`。这是宏语法的正确行为（元组作宿主参数正是 FromValue 的用途），测试照此修正。

**量化结论（分层可行性，详见对话报告）**：
- so_demo：**可接住，试点首选**。直接接住 ~120 个标量/str/bool 函数；元组化 ~15 类固定记录；Opaque+getter ~8 类变长记录集；Kotlin/JNI 层零改动。集成层需对齐：线程模型（rhai sync 跨线程缓存 vs brook `!Send` 每线程 Engine）、取消策略（rhai on_progress 协作取消 vs brook gas 硬中止 + 宿主函数内自查）、Scope 常量（9 颜色改零参函数，ARGS 0 使用可砍）。
- rhai_demo：**可接住但为宿主 API v2 工程**。~110 个标量函数直接接住；Task/Page 句柄天然映射 Opaque；~67 个 Map/Array 返回 + exec(Map 入参) 需重设计；253 个 try/catch 与 34 个脚本（6905 行）全部重写；diag.rhai 留作最后回归网；两处隐蔽语义点（字符串原地改、max_operations 长挂假设）。

**验证**：177 项全绿（175 单测/集成 + 2 doctest）、零警告。

---

## 附录 A · 讨论要点回顾

本方案经多轮讨论逐步收敛，关键问答脉络：

1. **「极简 Rhai 能否直逼 Rust、超过 Roto？」** → 极简与高性能目标冲突；Roto 是静态 JIT，方向相反；需换架构。
2. **「五目标方案」** → 提出静态类型 + 寄存器 VM + 可选 AOT 双模；Android 否决 JIT。
3. **「能否像 Rhai 调宿主函数、热重载？」** → VM 档默认支持；注册表一次注册长期有效；reload 毫秒级。
4. **「AOT 怎么回事？」** → IR → Rust 源码 → rustc；生成代码即普通 Rust；与热重载互斥。
5. **「AOT 不就是写宿主里吗，意义何在？」** → 卖点是「逻辑与宿主分离」；自己人写则 AOT 多余；当前项目不做。
6. **「编译速度是秒级还是分钟级？」** → 增量编译秒级、完整构建分钟~小时，二者场景不同。
7. **「不用 AOT 能达 Roto 性能吗？」** → 打平不可能（架构税 2–5×），但 Android 上 Roto 不可用，比较无意义；目标 3–8× 足够。
8. **「语法方案呢？」** → 补上语言设计。
9. **「Rust 语法太复杂，能更简单吗？」** → 砍标注、分号；保留 `let` 与返回类型；返回类型可选；全推断。
10. **「函数返回多个类型怎么办？」** → 区分多值（元组，支持）与多类型（union，不支持）；元组+标志位替代。
