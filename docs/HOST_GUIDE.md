# vitul 宿主集成指南（Rust 侧）

> **读者：会 Rust、要把引擎嵌进自己程序的开发者。** 只想写脚本？看 `docs/TUTORIAL.md`，那份不需要任何 Rust 知识。
>
> 本指南的每段代码都被 `tests/`（尤其 `tests/tutorial_examples.rs`、`tests/migration_so_demo.rs`）真实执行过。

---

## 1. 依赖与最小程序

```toml
[dependencies]
vitul = { path = "../vitul" }
```

```rust
use vitul::{register_fn, Engine, Value};

fn main() -> vitul::Result<()> {
    let mut engine = Engine::new();
    register_fn!(engine, "log", (String), |s| println!("{}", s))?;

    // 脚本顶层只能是函数定义；习惯上定义 main() 由宿主调用
    let script = engine.load_str("func double(x) { x * 2 }\nfunc main() { log(to_str(double(21))) }")?;
    let result = engine.call(&script, "double", vec![Value::I64(21)])?;
    assert_eq!(result, Value::I64(42));
    engine.call(&script, "main", vec![])?;
    Ok(())
}
```

四个概念：**Engine**（注册表 + 限制）、**Script**（编译好的字节码，可反复调用/热重载）、**Value**（边界数据）、**注册**（把 Rust 函数暴露给脚本）。

## 2. 加载与调用

```rust
let script = engine.load_str(src)?;                    // 字符串
let script = engine.load_file("scripts/task.vt")?;     // 文件
let script = engine.compile(src, "task.vt")?;          // 显式命名（报错用）

let v = engine.call(&script, "函数名", vec![Value::I64(1)])?;   // 调用
```

`Value` 构造：`Value::I64(1)`、`21.into()`、`Value::from("str")`、`Value::from(true)`、`Value::F64(2.5)`、`Value::Tuple(vec![...])`。

每次 `call` 都是独立的 VM 实例：gas 按次计费、缓存按 Script 复用（编译一次，调用任意次）。

## 3. 注册函数：`register_fn!`

```rust
register_fn!(engine, "add", (i64, i64) -> i64, |a, b| a + b)?;          // 全写
register_fn!(engine, "add_short", (i64, i64), |a, b| a + b)?;           // 返回类型推断
register_fn!(engine, "answer", (), || 42)?;                             // 无参
register_fn!(engine, "fail_fast", (i64) -> Result<i64, Error>, |x| {
    if x < 0 { Err(Error::runtime("负数不接受")) } else { Ok(x) }
})?;                                                                     // Result → 脚本侧运行时错误
```

- 0–6 参数；支持 i64/f64/bool/String/`Rc<str>`/元组(2–8)/`Opaque<T>`/`Result<T, Error>`
- 闭包拿**具体类型**（不是 Value）；签名从 Rust 类型推导，永远和闭包一致
- 同名重复注册返回 Err；内置函数名（`len`/`abs`/…）是保留字
- raw API `engine.register_fn(name, Vec<Ty>, Ty, f)` 是逃生舱（收 `Value`）

## 4. 类型边界

- 参数类型是脚本编译期**推断**的；推断不出 → **默认 i64**
- 默认 i64 的参数收到 str 等会在边界被拒（报错提示加标注）——防静默算错
- **i64 → f64 自动提升**（唯一隐式转换）；反向不行
- 宿主**返回值**按签名精确转换；形状不符报契约错误

设计原则：宁可报错逼标注，不静默算错（PROJECT_PLAN.md D-008）。

## 5. 错误与诊断

```rust
// 单行（日志/FFI）：[runtime] error at 1:14: division by zero
let err = engine.call(&script, "go", vec![Value::I64(0)]).unwrap_err();
println!("{}", err);

// 多行：--> 文件:行:列 + 源码行 + caret（tab=4、CJK=2）+ 调用栈
println!("{}", script.report(&err));
```

五个阶段：`[lex] [parse] [type] [runtime] [io]`。列按字符算、caret 按显示宽度对齐。

## 6. Opaque\<T\>：不透明句柄

```rust
use vitul::Opaque;

struct Player { name: String, hp: i64 }
register_fn!(engine, "heal", (Opaque<Player>, i64) -> String, |p, amount| {
    format!("{} +{}", p.name, amount)     // p 自动 Deref 成 &Player
})?;

let script = engine.load_str("func buff(p, n) { heal(p, n) }")?;
let p = Value::from(Opaque::new(Player { name: "战士".into(), hp: 100 }));
```

脚本不能构造句柄/读字段/换类型，只能原样传回；边界自动校验。可变状态用 `RefCell`/`Cell` 字段（对应 rhai 的 `&mut` 方法模式，见 `tests/migration_so_demo.rs`）。

## 7. 宿主返回元组（2–8）

```rust
// 闭包返回 Rust 元组 → 自动转 Value::Tuple
register_fn!(engine, "hsl_to_rgb", (i64) -> (i64, i64, i64), |h| (h * 2, h * 3, h * 4))?;

// 双括号 = 一个元组类型的参数（外层参数列表、内层元组类型）
register_fn!(engine, "take3", ((i64, i64, i64)) -> i64, |t| t.0 + t.1 + t.2)?;
```

检查时机：**宿主返回值**的形状编译期已知（签名写死），脚本索引越界是编译错误；**脚本函数参数**（未标注）由使用方式反推「至少 N 元」，运行时对账（≥ 即可，多出的无害）。变长集合 → `Opaque<集合>` + count/indexed getter。

## 8. 热重载

```rust
let mut script = engine.load_file("scripts/task.vt")?;
engine.call(&script, "main", vec![])?;

// 文件改后原地换字节码；失败旧脚本原样保留
engine.reload(&mut script, "scripts/task.vt")?;
```

`call(&Script)` 与 `reload(&mut Script)` 的借用规则天然防并发重载，无需锁。

## 9. 安全三件套

```rust
let engine = Engine::new()
    .with_limits(1_000_000, 64)      // gas 步数、调用深度
    .with_heap_limit(1024 * 1024);   // 脚本内存配额
```

gas 每指令扣 1 步（死循环被掐）；深度防无限递归；配额防超大字符串；`catch_unwind` 保证脚本/宿主回调 panic 不穿透（需 `panic = "unwind"`，见 Cargo.toml 注释）。溢出/除零/越界一律报错不回绕。

## 10. 字节码缓存（.sbc）

```rust
// 内存往返（源码已在内存时：Android assets / 网络下发）
let bytes = engine.script_to_bytes(&script);
let restored = engine.script_from_bytes(&bytes)?;

// 源码在内存 + 磁盘缓存（过期自动重建）
let script = engine.compile_cached(src, "task", "cache/task.sbc")?;

// 文件 + 旁边 .sbc
let script = engine.load_cached("scripts/task.vt")?;
```

诚实基准：磁盘路径比直接编译**慢**（两次 I/O ≈80µs vs 编译 8µs）。价值在「内存源码跳过编译」与「字节码独立交付」。缓存绑定宿主注册表指纹，注册表变了自动失效。格式 VERSION 当前 3。

## 11. 线程模型

`Engine`/`Script`/`Value` 都不是 `Send`（Rc 承载）——**每线程一个 Engine**，各自注册各自持有。跨线程只传标量（i64/f64/bool）。

```rust
std::thread::spawn(move || {
    let mut engine = Engine::new();
    // 注册、load、call 全在线程内完成
});
```

`call(&Script)` 与 `reload(&mut Script)` 借用规则天然互斥，无需锁。多线程宿主（JNI 回调任意线程）= 每线程本地缓存 Engine，或任务队列单线程执行。

## 12. 公共函数库（宿主拼接）

语言无 `import`。脚本作者视角见 TUTORIAL.md §4.4；宿主侧就是**读几份源码拼成一份再编译**：

```rust
fn load_with_libs(engine: &Engine, lib_paths: &[&str], script_path: &str)
    -> vitul::Result<vitul::Script>
{
    let mut combined = String::new();
    for p in lib_paths {
        combined.push_str(&std::fs::read_to_string(p)?);
        combined.push('\n');
    }
    combined.push_str(&std::fs::read_to_string(script_path)?);
    engine.load_cached(&combined)
}
```

顺序无关；同名冲突编译报错（checker 区分宿主碰撞与脚本内部重复）；缓存按拼接后完整文本 hash，lib 改动自动失效所有依赖脚本；报错行号按拼接后文本算。

## 12.1 跨脚本调用（`call_script` 模式）

让脚本 A 调用脚本 B 里的指定函数：注册一个宿主函数 `call_script(脚本名, 函数名)`，闭包内用**共享的引擎实例**执行目标函数。完整参考实现：`tests/migration_so_demo.rs::cross_script_call_via_host_function`。

```rust
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// 宿主持有共享引擎（Rc<RefCell<Engine>>）；嵌套调用期间全是不可变借用
let engine = Rc::new(RefCell::new(Engine::new()));

// 目标脚本编译一次进缓存（对应 so_demo 的 AST_CACHE）
let cache: Rc<RefCell<HashMap<String, vitul::Script>>> = /* ... */;

// 递归检测的名字栈（对应 so_demo 的 SCRIPT_STACK）——必须有，见下
let stack: Rc<RefCell<Vec<String>>> = /* ... */;

register_fn!(engine.borrow_mut(), "call_script",
    (String, String) -> Result<(bool, i64), Error>,
    move |script_name, func| {
        if stack.borrow().iter().any(|s| s == &script_name) {
            return Err(Error::runtime(format!("跨脚本递归: {}", script_name)));
        }
        stack.borrow_mut().push(script_name.clone());
        let inner = (|| {
            let eng = engine.borrow();
            let script = cache.borrow().get(&script_name).cloned()
                .ok_or_else(|| Error::runtime(format!("脚本不存在: {}", script_name)))?;
            let v = eng.call(&script, &func, vec![Value::I64(6)])?;
            match v { Value::I64(n) => Ok((true, n)), _ => Ok((true, 0)) }
        })();
        stack.borrow_mut().pop();
        inner
    })?;
```

**四件事缺一就有坑**：

1. **共享引擎**：`Rc<RefCell<Engine>>`——嵌套 `engine.call` 全是不可变借用（`call` 只拿 `&self`），RefCell 合法；注册表共享 → 目标脚本也能调宿主函数
2. **递归检测（必须）**：每次嵌套 `call` 是**全新 VM/全新 gas**——引擎的 gas 拦不住 A→B→A 跨 VM 无限嵌套，只有宿主的名字栈能拦
3. **脚本缓存**：`HashMap<String, Script>` 免重复编译；`Script` 是 `Clone` 的 `Rc` 背衬结构，同线程内自由持有
4. **封送设计**：返回形状固定（如 `(bool, i64)`）意味着其他类型被**静默吞掉**（POC 断言：str 返回吞成 `(true, 0)` 无报错）——要么按签名注册变体（组合爆炸），要么约束「跨脚本调用只传字符串/整数」（任务编排定位）。**细粒度强类型的函数复用走 §12 拼接，别用这个**

已知代价：闭包捕获引擎 `Rc` 形成环 → 引擎延迟释放（长生命周期宿主可接受，可用 `Weak` 打破）。定位原则：**拼接管函数库（细粒度），call_script 管任务编排（粗粒度）**，两者共存不冲突。

## 13. C ABI（Android / JNI）

`src/ffi.rs` 导出两层：

- **一次性**：`ms_call_i64(source, func, arg, out)`——每次重编译，smoke-test 形态
- **句柄式**：`ms_script_create` / `ms_script_create_cached` / `ms_script_call`（`MsVal` 标量+字符串）/ `ms_script_reload` / `ms_script_free` / `ms_last_report`（多行诊断）

错误协议两步零跨边界分配；句柄绑定创建线程（跨线程 `MS_ERR_THREAD`）；全部增量导出，`ms_abi_version() == 1`。详细 C 头文件草图见 `ffi.rs` 模块文档；打包流程见 `scripts/android_build.sh`。

## 14. 源码地图

| 模块 | 职责 |
|---|---|
| `lexer/parser/ast` | 词法/语法（`MAX_NESTING=64` 守卫） |
| `ty/typecheck` | HM 风格推断 + 签名表 |
| `ir/codegen` | 类型特化字节码（71 opcode） |
| `vm` | 寄存器 VM（扁平寄存器文件、gas/堆/深度） |
| `validate` | 字节码结构校验（出生点跑一次） |
| `cache` | `.sbc` 编解码 + 指纹绑定 |
| `engine` | 公共 API |
| `host` | `register_fn!` 桥接层（ScriptType/FromValue/HostResult/Opaque） |
| `ffi` | C ABI |
| `eval` | 参考解释器（非安全边界） |
| `error/diag/span` | 错误与诊断渲染 |

架构与全部决策记录：`PROJECT_PLAN.md`。
