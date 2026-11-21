# miniscript 入门教程（写给第一次接触脚本引擎的你）

> 本教程的**每一段示例代码**都在 `tests/tutorial_examples.rs` 里逐条真实执行过。
> 想亲手跑一遍：`cargo test --test tutorial_examples`。教程说结果是什么，测试就断言什么——不会骗你。

---

## 目录

- [0. miniscript 是什么](#0-miniscript-是什么)
- [1. 五分钟上手](#1-五分钟上手)
- [2. 脚本语言速览](#2-脚本语言速览)
- [3. 宿主集成（Rust 侧）](#3-宿主集成rust-侧)
- [4. 进阶话题](#4-进阶话题)
- [5. 常见坑 FAQ（每一条都是真实踩过的）](#5-常见坑-faq)
- [6. 下一步去哪看](#6-下一步去哪看)

---

## 0. miniscript 是什么

一句话：**一门嵌在 Rust 程序里运行的极简脚本语言**。

用三个问题快速了解它：

**它长什么样？** 像 Rust，但砍掉了所有重的东西：没有分号、变量不用标类型、函数参数不用标类型。看一个例子感受一下：

```text
func sum_to(n) {
    let t = 0
    for i in 0..n {
        t = t + i
    }
    t
}
```

**它为什么存在？** 把「经常要改的逻辑」（风控规则、自动化流程、游戏数值）从 Rust 代码里拆出来。改脚本**不用重新编译宿主程序**——热重载是它的核心卖点。

**它不能做什么？（刻意设计的）** 没有 Map / Array 数据结构、没有闭包、没有异常、没有并发、没有文件/网络/系统访问。脚本想干任何"有副作用"的事，必须通过你（宿主）注册的函数——这既是安全边界，也让所有危险操作都过你的手。

性能参考：比 Rhai（同类动态脚本引擎）快 5–10 倍；大约是手写 Rust 的 1/8（循环类）到 1/28（递归类）。

**本教程有两种读者，按需取用**：
- **只写脚本**（.ms 文件）→ 读 §1、§2、§5 即可。§3 是给写 Rust 注册代码的人看的
- **做宿主集成**（写 Rust 注册代码）→ 加读 §3、§4

涉及的编译器机制（类型怎么推出来的、词法器怎么分词）一律不在教程里讲——**那是引擎替你干的活**，写错了报错信息会引导你改。想深挖去 `PROJECT_PLAN.md`。

---

## 1. 五分钟上手

### 第一步：加依赖

```toml
[dependencies]
miniscript = { path = "../miniscript" }
```

### 第二步：完整的最小程序

```rust
use miniscript::{register_fn, Engine, Value};

fn main() -> miniscript::Result<()> {
    // 1. 创建一个引擎
    let mut engine = Engine::new();

    // 2. 注册宿主函数：之后脚本里就能直接调 log("...")
    //    参数类型 (String) 和返回类型由 Rust 闭包自动推导
    let logs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&logs);
    register_fn!(engine, "log", (String), move |s| {
        sink.borrow_mut().push(s)
    })?;

    // 3. 加载脚本（就是一段字符串）
    //    ⚠️ 脚本顶层只能放函数定义！流程逻辑包进 main()，由宿主调用。
    let script = engine.load_str(
        "func double(x) { x * 2 }\n\
         func main() {\n\
             log(\"开始计算\")\n\
             double(21)\n\
         }",
    )?;

    // 4. 调用脚本里的函数，拿回结果
    let result = engine.call(&script, "double", vec![Value::I64(21)])?;
    assert_eq!(result, Value::I64(42));

    // 也调用 main()——流程逻辑在这里执行
    engine.call(&script, "main", vec![])?;

    println!("double(21) = {}", result);            // double(21) = 42
    println!("脚本日志: {:?}", logs.borrow());       // ["开始计算"]
    Ok(())
}
```

### 记住这四个概念，剩下都是细节

| 概念 | 是什么 | 对应 API |
|---|---|---|
| **Engine** | 脚本的"运行环境"：宿主函数注册表 + 安全限制 | `Engine::new()` |
| **Script** | 编译好的脚本（字节码），可以反复调用、可以热重载 | `engine.load_str(...)` |
| **Value** | 宿主和脚本之间传递的数据 | `Value::I64(42)`、`Value::from("文本")` |
| **注册** | 把你的 Rust 函数暴露给脚本调用 | `register_fn!(engine, "名字", (参数类型), 闭包)` |

调用脚本的固定姿势：**`engine.call(&script, "函数名", vec![参数...])`**。

⚠️ **和 rhai 最大的心智差异**：rhai 的脚本文件是"顶层语句流，整文件即程序"；miniscript 的脚本**顶层只能是函数定义**，可执行逻辑全部写在函数里，由宿主选择调用哪个函数（习惯上定义一个 `main()`）。从 rhai 迁移的第一步就是把顶层语句包进 `func main() { ... }`。

### 第二步半：把示例跑起来

本教程的宿主示例代码拆成了 20 个测试：`cargo test --test tutorial_examples`。

---

## 2. 脚本语言速览

以下 `text` 代码块都是脚本内容。宿主侧的调用方式统一为
`engine.call(&script, "函数名", vec![...])`，测试里逐个验证过返回值。

### 2.1 函数与返回值

```text
// 末位表达式就是返回值（不需要 return）
func add(a, b) {
    a + b
}

// 也可以显式标注返回类型 + 用 return 提前退出
func div_safe(a, b) -> f64 {
    if b == 0.0 {
        return 0.0
    }
    a / b
}

// 没有返回值的函数返回 ()（读作 unit）
func shout(msg: str) {
    log(msg)
    log(msg + "!")
}
```

要点：
- 参数类型**不用写**（编译器推断，见 §3.3）；返回类型**可写可不写**（写了会当校验用）
- 无分号，**换行就是语句结束**
- 支持递归：`func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }`

### 2.2 变量

```text
func demo_vars() {
    let x = 10          // 绑定
    x = x + 5           // 重新赋值（= 是赋值，== 才是比较）
    let y = x * 2       // y = 30
    y
}
```

**变量类型不能中途改变**：

```text
func demo_no_retype(x) {
    x = 1
    x = "s"         // 编译错误：assignment to `x`: type mismatch: expected i64, found str
    x
}
```

### 2.3 控制流

**if / else**——语句形式和表达式形式都支持：

```text
// 语句形式（两个分支会各自求值，整个 if 也是一个"表达式"）
func classify(n) {
    if n < 0 {
        "负数"
    } else if n == 0 {
        "零"
    } else {
        "正数"
    }
}

// 表达式形式：if 的值可以直接赋给变量（else 必须有！）
func max_of(a, b) {
    let bigger = if a > b { a } else { b }
    bigger
}
```

**while / for / break / continue**：

```text
func count_odd_under(n) {
    let c = 0
    for i in 0..n {          // 注意：0..n 是左闭右开！i 取 0 到 n-1
        if i % 2 == 0 {
            continue         // 跳过偶数
        }
        c = c + 1
    }
    c                        // count_odd_under(10) = 5（奇数是 1,3,5,7,9）
}

func first_square_over(n) {
    let i = 0
    while true {             // 死循环没关系，后面讲 gas 会兜底
        if i * i > n {
            break
        }
        i = i + 1
    }
    i                        // first_square_over(30) = 6（6*6=36 > 30）
}
```

⚠️ **`for i in 0..n` 是左闭右开**：`0..100` 迭代 0 到 99。`sum_to(100)` 是 4950 不是 5050。

### 2.4 类型系统

一共就 6 种类型，没有更多：

| 类型 | 字面量例子 | 说明 |
|---|---|---|
| `i64` | `0`、`42`、`-7`、`1234567890123` | 整数（64 位有符号，范围 ±9,223,372,036,854,775,807 ≈ ±922 亿亿——`42` 只是随手举例，**不是上限**） |
| `f64` | `3.14`、`2e3` | 浮点数（必须带 `.` 或科学计数法） |
| `bool` | `true` / `false` | 布尔 |
| `str` | `"文本"`、`"中文\n换行"` | 字符串（UTF-8） |
| 元组 | `(1, "a", true)` | 一次携带多个值，见 §2.5 |
| `()` | `()` | unit，"没有值"的占位 |

**类型推断怎么工作**：你几乎不用写类型，编译器从三个来源推出来——
1. 字面量：`let a = 42` → a 是 i64；`let b = 3.14` → b 是 f64
2. 你注册的宿主函数签名：`register_fn!(engine, "heal", (Opaque<Player>, i64) -> i64, ...)` 让脚本知道 `heal` 收什么、返什么
3. 运算传播：`x + 0.5` 里的 0.5 是 f64，所以 x 必须是 f64

**类型转换**：脚本内不做隐式转换（`1 + 2.5` 直接编译错误）；用显式转换函数：

```text
to_i64(2.9)      // → 2（f64 → i64，向零截断）
to_f64(3)        // → 3.0
to_str(42)       // → "42"（i64/f64/bool/str 都可以转）
```

### 2.5 元组与多返回值（详细）

元组把**几个值打包成一个值**——「几个」编译期就固定，每个位置类型可以不同：

```text
let point = (100, 200)              // (i64, i64)
let person = ("小明", 25, true)     // (str, i64, bool) —— 混合类型合法
let nested = ((1, 2), (3, 4))       // 元组里套元组
let single = (1,)                   // 单元素：必须带尾逗号！(1) 没逗号只是括号分组
let e = ()                          // 这不是"空元组"，是 unit（渲染相似，类型不同）
```

**取值用 `.编号`**——编号是**字面量**（从 0 开始），取第几个在编译期就确定，运行时零开销：

```text
let p = ("小米", 25)
p.0          // "小米"
p.1          // 25
p.2          // ✗ 编译错误：tuple index 2 out of range (len 2)
t.i          // ✗ 编译错误：expected tuple index（i 是变量不行）
p.1.0        // 嵌套取值连点即可 ✓
```

**最重要的用途：多返回值**。配合「标志位」表达「可能失败」（没有异常/空值，这是官方姿势）：

```text
func divmod(a, b) {
    (a / b, a % b)                // 一次返回两个值
}

func safe_div(a: f64, b: f64) {
    if b == 0.0 {
        (false, 0.0)              // (ok, value) 标志位模式
    } else {
        (true, a / b)
    }
}

func use2() {
    let ok = safe_div(10.0, 2.0)
    if ok.0 { ok.1 } else { 0.0 }
}
```

**四条规矩**：
1. **没有解构**——`let (a, b) = t` 不支持，用 `.0`/`.1` 逐个取
2. **没有相等比较**——`t1 == t2` 编译报错；要比较就逐字段 `t1.0 == t2.0 && t1.1 == t2.1`
3. **参数不用标注**——`func first2(t) { t.0 + t.1 }` 直接就能收元组（引擎按你用到的方式自动适配）；宿主传**更长**的元组也行，多出的元素无害。形状不够时**运行时报错**，信息会写清差多少
4. **最多 8 个元素**，超了报 `the maximum is 8`

**元组 ≠ 数组**（miniscript 没有数组）：元组元素个数编译期固定、每位置可不同类型、只能字面量索引；没有 push/遍历。**变长集合的官方替代**：宿主 `Opaque<集合>` + `count` + 带索引 getter，脚本侧 `for i in 0..count`（见 `tests/migration_so_demo.rs`）。

### 2.6 字符串

**只有 `+` 拼接，没有其他花样**：

```text
func report(name: str, score: f64) {
    "玩家 " + name + " 得分 " + to_str(score)
}
// report("小明", 3.5) → "玩家 小明 得分 3.5"
```

操作字符串用内置函数（注意是**函数**，不是 `s.len()` 这种方法调用）：

```text
len("中文")           // → 2（按字符数，不是字节数！）
contains(pw, "123")   // → bool，pw 里含 "123" 吗
substr("中文abc", 0, 2)   // → "中文"（按字符截取，不是字节）
```

⚠️ **`to_str` 的类型要看清**：给 `to_str` 传一个"还没确定类型"的变量，它会被钉成 i64 处理。想格式化浮点数，参数要标注 `: f64`（见 §3.3）。

⚠️ **rhai 的 `trim()`/`replace()` 是原地修改**——miniscript 没有这种语义，从 rhai 迁移时这是必踩的坑（见 PROJECT_PLAN.md D-035）。

### 2.7 注释

```text
// 行注释
/* 块注释，
   可以跨行。不嵌套（C 风格） */
```

### 2.8 内置函数白名单（就这些，没了）

| 函数 | 说明 |
|---|---|
| `abs(x)` / `min(a,b)` / `max(a,b)` | **数值泛型**：实参是 i64 返回 i64，是 f64 返回 f64（一次调用里混型会编译报错） |
| `sqrt(x)` / `floor(x)` / `ceil(x)` | 仅 f64 |
| `pow(a, b)` | 仅 f64 |
| `len(s)` | 字符数（i64） |
| `contains(s, sub)` | 子串判断（bool） |
| `substr(s, start, len)` | 按字符截取（str） |
| `to_i64(x)` / `to_f64(x)` / `to_str(x)` | 显式转换 |

**这些名字是保留字**——你不能定义 `func len(s) { s }`，会直接编译报错。

---

## 3. 宿主集成（Rust 侧）

### 3.1 注册函数：`register_fn!` 宏

签名从 Rust 闭包的类型自动推导，**脚本看到的签名永远和你的闭包一致**（这是设计保证，不是巧合）：

```rust
use miniscript::{register_fn, Engine, Error, Value};

let mut engine = Engine::new();

// 完整形式：参数类型 + -> 返回类型
register_fn!(engine, "add", (i64, i64) -> i64, |a, b| a + b)?;

// 简写：返回类型由闭包推断
register_fn!(engine, "add_short", (i64, i64), |a, b| a + b)?;

// 无参数
register_fn!(engine, "answer", (), || 42)?;

// 可以失败：返回 Result，脚本侧表现为运行时错误（可被宿主捕获）
register_fn!(engine, "fail_fast", (i64) -> Result<i64, Error>, |x| {
    if x < 0 {
        Err(Error::runtime("负数不接受"))
    } else {
        Ok(x)
    }
})?;

// 最多 6 个参数；支持 i64/f64/bool/String/&str/元组/Opaque<T>
```

闭包拿到的是**具体类型**（不是 `Value`），不用自己拆箱。注册失败的错误不会被吞掉（返回 `Result`）；同名重复注册直接报错。

### 3.2 调用脚本与传参

```rust
// Value 的几种构造方式
let a = Value::I64(21);
let b: Value = 21.into();            // From<i64>
let s = Value::from("文本");          // From<&str> / From<String>
let f = Value::F64(2.5);
let flag = Value::from(true);
let tup = Value::Tuple(vec![Value::I64(1), Value::I64(2)]);

// 调用：函数名 + 参数列表
let result = engine.call(&script, "add", vec![a, b])?;
```

### 3.3 类型边界（全教程最重要的一节！）

脚本参数的类型是**推断**出来的。推断不出来的参数**默认按 i64 处理**：

```rust
// 场景 1：函数体里有字符串线索 → 自动推断为 str，没问题
let ok = engine.load_str("func greet(name) { \"hi \" + name }")?;
engine.call(&ok, "greet", vec![Value::from("小明")])?;   // "hi 小明" ✓

// 场景 2：参数根本没被用到（或只做透传）→ 默认 i64 → 传字符串直接报错！
let bad = engine.load_str("func echo(x) { x }")?;
let err = engine.call(&bad, "echo", vec![Value::from("文本")]).unwrap_err();
// err: "argument type mismatch: expected i64, got str.
//        Annotate the parameter (e.g. `x: str`) to fix inference."

// 场景 2 的正确姿势：加类型标注（脚本的"逃生舱"）
let good = engine.load_str("func echo(x: str) { x }")?;
engine.call(&good, "echo", vec![Value::from("文本")])?;   // ✓
```

**为什么这样设计**：如果默默按 i64 解释字符串的内部指针，会算出错误结果还一声不吭。宁可报错逼你标注，也不静默算错（PROJECT_PLAN.md D-008）。

另一个方向是友好的：**f64 参数收到 i64 会自动提升**（`3` 变 `3.0`），不会报错。

### 3.4 错误处理与诊断

每个阶段（词法/语法/类型/运行时/IO）的错误都带源码位置：

```rust
// 单行格式（打日志够用）
let err = engine.call(&script, "go", vec![Value::I64(0)]).unwrap_err();
println!("{}", err);
// [runtime] error at 1:14: division by zero

// 多行报告（给开发者看）：--> 位置 + 源码行 + ^ 指示 + 调用栈
println!("{}", script.report(&err));
//   --> demo.ms:1:14
//    |
//  1 | func go(x) { 100 / x }
//    |              ^
//  = call stack (outermost first):
//      1. go (1:14)
```

列号按**显示宽度**算（tab = 4、中文 = 2），caret 落在你肉眼看到的位置。脚本编译失败的错误也能用 `report` 渲染。

### 3.5 Opaque\<T\>：把宿主对象递给脚本

脚本不能碰 Rust 结构体的字段，但可以**拿着它**传给你注册的函数：

```rust
use miniscript::Opaque;

struct Player {
    name: String,
    hp: i64,
}

let mut engine = Engine::new();
register_fn!(engine, "heal", (Opaque<Player>, i64) -> String, |p, amount| {
    // p 会自动解引用（Deref）成 &Player
    format!("{} 回了 {} 血，现 HP {}", p.name, amount, p.hp + amount)
})?;

let script = engine.load_str("func buff(p, n) { heal(p, n) }")?;

// 把 Player 装进句柄传进去
let p = Value::from(Opaque::new(Player { name: "战士".into(), hp: 100 }));
let result = engine.call(&script, "buff", vec![p, Value::I64(30)])?;
assert_eq!(result, Value::from("战士 回了 30 血，现 HP 130"));
```

安全保证：脚本**无法**构造句柄、读字段、改成别的类型——只能原样传回。句柄校验在边界自动完成（类型名 + 真实 downcast），回调里拿到的引用永远有效。

rhai 的 `&mut Task` 方法调用模式，对应过来是 Opaque + 内部可变性（`RefCell`/`Cell` 字段）+ 普通 setter/getter 函数（实测见 `tests/migration_so_demo.rs`）。

### 3.6 宿主返回元组（2–8 个）与两种检查时机

**返回方向**：闭包返回 Rust 的真元组，引擎自动转成脚本元组：

```rust
register_fn!(engine, "hsl_to_rgb", (i64) -> (i64, i64, i64), |h| (h * 2, h * 3, h * 4))?;
```

**参数方向——双括号的两种读法**。宏的参数列表是 `(类型, 类型, ...)`，每个逗号槽位写一个类型；元组本身也是一个类型，所以要套两层：

```rust
// A：三个独立参数 —— 脚本传 3 个值
register_fn!(engine, "sum3a", (i64, i64, i64) -> i64, |a, b, c| a + b + c)?;
// 脚本：sum3a(1, 2, 3)

// B：一个元组类型的参数 —— 外层括号是参数列表，内层是元组类型
register_fn!(engine, "sum3b", ((i64, i64, i64)) -> i64, |t| t.0 + t.1 + t.2)?;
// 脚本：sum3b((1, 2, 3))   ← 传 1 个实参（元组字面量）
```

**两种元组来源，报错时机不同**（实用结论，机制不用管）：

| 元组来源 | 索引越界 / 形状不对何时报 |
|---|---|
| 宿主函数的**返回值** | **编译错误**（签名写死了形状） |
| 脚本函数的**参数**（未标注） | **运行时错误**（信息会写清差多少） |

```rust
// 端到端：宿主返回 (ok, x, y, size) 四元组，脚本编译期就能查准
register_fn!(engine, "find_btn", (String) -> (bool, i64, i64, i64), |name| {
    if name == "确定" { (true, 100, 200, 50) } else { (false, 0, 0, 0) }
})?;
```

```text
func tap_ok() {
    let hit = find_btn("确定")
    if hit.0 {
        hit.1 + hit.2        // 300
    } else {
        0
    }
}
// hit.4 → 编译错误（签名说好 4 元）；

// 脚本参数不标注也能收元组：func first5(t) { t.0+..+t.4 } →
// 宿主传 5 元及以上都行（多出的无害），不足 5 元运行时报错。
```

**变长的集合**（不知道有几个元素）用下一节的 Opaque 装着返回。

### 3.7 热重载

```rust
// 从文件加载
let mut script = engine.load_file("scripts/greet.ms")?;
engine.call(&script, "greet", vec![Value::from("world")])?;   // "hello world"

// 改了 .ms 文件之后——不重启宿主，原地换字节码：
engine.reload(&mut script, "scripts/greet.ms")?;
engine.call(&script, "greet", vec![Value::from("世界")])?;    // "你好 世界"
```

保证：reload 失败时旧脚本原样保留还能用；正在执行的旧调用跑完不受影响（`call` 拿 `&Script`、`reload` 拿 `&mut Script`，Rust 的借用规则天然防住了并发重载）。

### 3.8 安全三件套（脚本沙箱的墙）

```rust
let engine = Engine::new()
    .with_limits(1_000_000, 64)      // gas 步数 100 万（默认 1000 万）、调用深度 64（默认 256）
    .with_heap_limit(1024 * 1024);   // 脚本内存配额 1 MiB（默认 4 MiB）
```

| 墙 | 防什么 | 超限后果 |
|---|---|---|
| gas（每条指令扣 1 步） | 死循环、超长计算 | `Err("step (gas) limit exceeded")` |
| 调用深度 | 无限递归 | `Err("call depth limit exceeded")` |
| 内存配额 | 无限拼字符串 | `Err("memory limit exceeded ...")` |
| `catch_unwind` 边界 | 脚本/宿主回调 panic | 变成 `Err`，**绝不炸宿主进程**（需要默认的 `panic = "unwind"`） |

另外：整数溢出、除零、元组越界等一律**返回错误而不是崩溃或回绕**。脚本没有任何 I/O 能力——文件、网络、系统调用，全靠你注册。

---

## 4. 进阶话题

### 4.1 字节码缓存（.sbc）

编译结果可以序列化成字节，二次加载跳过"解析+类型检查+代码生成"（约快 5 倍）：

```rust
// 方式 A：内存里往返（源码在内存时最合适——Android assets / 网络下发）
let bytes = engine.script_to_bytes(&script);
let restored = engine.script_from_bytes(&bytes)?;

// 方式 B：源码在内存 + 磁盘缓存文件（缓存过期/损坏会自动重建）
let script = engine.compile_cached(src, "demo", "cache/demo.sbc")?;

// 方式 C：从文件加载 + 旁边的 .sbc 缓存
let script = engine.load_cached("scripts/greet.ms")?;
```

**诚实提醒**（来自真实基准，不粉饰）：磁盘缓存路径反而比直接编译**慢**（两次文件 I/O 约 80µs vs 编译 8µs）。`.sbc` 的价值在「源码已在内存时跳过编译」和「字节码作为独立交付物」，不是磁盘加速。缓存绑定宿主注册表指纹——注册函数变了缓存自动失效，不会调错函数。

### 4.2 线程模型：一个线程一个 Engine

`Engine` / `Script` / `Value` 都**不是 `Send`**（内部用 `Rc`）。这是设计不是缺陷：

```rust
// 每个线程自己注册、自己编译、自己持有 Script——互不干扰
let handles: Vec<_> = (0..4).map(|tid| {
    std::thread::spawn(move || {
        let mut engine = Engine::new();
        let bonus = tid as i64 * 100;
        register_fn!(engine, "add_bonus", (i64) -> i64, move |x| x + bonus).unwrap();
        let script = engine.load_str("func go(x) { add_bonus(x) }").unwrap();
        let v = engine.call(&script, "go", vec![Value::I64(1)]).unwrap();
        match v { Value::I64(n) => n, _ => panic!("expected i64") }
        // ↑ 跨线程只传标量：Value 本身不是 Send，线程内转成 i64/f64/bool 再交出去
    })
}).collect();
// 四个线程各得 101/201/301/401，注册的闭包互不串线
```

`call(&Script)` 和 `reload(&mut Script)` 的借用规则让"边跑边重载"在编译期就不可能发生——不需要任何锁。

### 4.3 性能预期（有实测背书）

| 场景 | 相对手写 Rust | 备注 |
|---|---|---|
| 循环类（sum_to） | ~8.6× | 已进目标区间（目标 3–8×） |
| 递归类（fib） | ~27.6× | 解释器架构税，无内联空间 |
| 编译一个中等脚本 | ~8 µs | 每次调用前重编译都很便宜 |
| 缓存解码 | ~1.5 µs | 比编译快 5.2×（内存路径） |

调优手段：`with_limits` 收紧 gas 能提前掐死慢脚本；真正要原生性能的热点走 AOT/JIT（见 PROJECT_PLAN.md Q11/ADR-014，当前均冻结）。

---

## 5. 常见坑 FAQ（每一条都是真实踩过的）

**Q0：脚本顶层写 `log("...")` 报 `expected Func, found Ident(...)`？**
脚本顶层**只能放函数定义**（`func ...`）。把语句包进 `func main() { ... }`，宿主 `call("main")`。这是与 rhai 最大的心智差异（见 §1）。

**Q1：为什么报 `unexpected character ';'`？**
本语言**没有分号**，换行即语句结束。把 `;` 删掉。

**Q2：传字符串给脚本函数报 `argument type mismatch: expected i64, got str`？**
参数类型没推断出来、默认按 i64 了（§3.3）。给参数加标注：`func echo(x: str) { x }`。

**Q3：`sum_to(100)` 怎么是 4950？**
`for i in 0..n` 左闭右开，i 只到 99。要 5050 用 `0..n+1`。

**Q4：能写 `s.trim()` 这种方法调用吗？**
不能。字符串没有方法，只有内置函数：`len(s)`、`contains(s, sub)`、`substr(s, start, len)`。拼接只有 `+`。

**Q5：`let x = if cond { 1 }` 报错？**
if 作为**表达式**必须有 else（两个分支要能合并成一个类型）。作为语句的 `if` 不需要 else，但那样它不产生值。

**Q6：定义 `func len(s) { s }` 报 `reserved built-in name`？**
内置函数名是保留字，换个别名。这是 Q15 修复：以前能编译但内部调用永远命中内置实现，是个陷阱。

**Q7：`1 + 2.5` 报类型不匹配？**
脚本内不做隐式数值转换。写 `to_f64(1) + 2.5`。

**Q8：嵌套太深报 `nesting too deep (limit 64)`？**
括号/块/运算符链/else-if 累计嵌套上限 64。这不是 bug——再深会让解析器栈溢出直接崩进程（不可恢复），提前报错是安全边界。机器生成的长表达式拆成多条 `let`。

**Q9：函数参数超过 8 个？**
单次调用 / 元组 / 内联操作数上限都是 8。多了请打包成元组或拆函数。

**Q10：脚本死循环把宿主卡死了？**
不会，gas 会掐（默认 1000 万步）。但注意：**宿主函数自己内部阻塞**（比如 `sleep(3600)`）gas 管不着——gas 只在脚本指令间计数。长时间阻塞要靠宿主函数自己实现取消检查。

**Q11：怎么让脚本"抛异常"？**
没有异常。宿主函数返回 `Result`（→ 脚本报错终止），或者返回 `(ok, err_msg)` 元组让脚本自己分支处理（so_demo 迁移 POC 用的是后者，见 `tests/migration_so_demo.rs`）。

**Q12：脚本怎么输出东西？**
注册一个 `log` 函数（本教程 §1 就是这么干的）。脚本本身没有 print/文件/网络/时间/随机数——**全部**经由宿主。

**Q13：整数能有多大？42 是上限吗？**
42 只是表格里随手举的例子。`i64` 是 64 位有符号整数，范围 **±9,223,372,036,854,775,807（约 ±922 亿亿）**，日常任何整数都装得下。两条越界边界，都会**明确报错而不是悄悄算错**：
- 字面量直接写超范围 → 编译报错 `[lex] integer literal overflows i64`
- 运算结果超出范围 → 运行时报错 `[runtime] i64 add overflow`（减/乘/除同理），**不回绕成负数**

冷知识：`i64` 的最小值写不成字面量——`-9223372036854775808` 会先因字面量溢出报错（解析器先读数字再取负），要写 `0 - 9223372036854775807 - 1`。

**Q14：元组的「类型自动适配」「双括号」这些机制我需要学吗？**
**不用。** 只写脚本的话，记住三条就够：元组用 `.0` 取值、不能 `==` 和解构、最多 8 个——写错了报错信息会告诉你怎么改。「至少 N 元推断」「双括号宏语法」分别属于引擎内部和 Rust 注册语法（做宿主集成的人才会碰到 `((i64, i64, i64))` 这种写法，照 §3.6 抄就行）。

---

## 6. 下一步去哪看

| 想了解 | 去看 |
|---|---|
| 语言规格权威定义 | `PROJECT_PLAN.md` 第三章 |
| 每个特性的行为规格（测试即文档） | `tests/` 目录——22 个测试文件按主题分组（loops / operators / type_errors / diagnostics …） |
| 完整宿主示例 | `examples/risk.ms`（风控规则）、`examples/android_cli.rs`（Android 真机 + C ABI） |
| 架构与设计决策 | `PROJECT_PLAN.md` 第七章 ADR（为什么没有 JIT/AOT、为什么无闭包、缓存为什么绑指纹） |
| 把 rhai 宿主迁过来 | `docs/migration/` 两份清单 + `tests/migration_so_demo.rs` 改写 POC |
| C ABI（Java/Kotlin/JNI 集成） | `src/ffi.rs` 模块文档（含 C 头文件草图） |

祝玩得开心。卡住了就翻 `tests/`——那里每个断言都是一个被验证过的真话。
