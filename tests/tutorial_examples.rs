//! 《miniscript 入门教程》（docs/TUTORIAL.md）的可执行示例集。
//!
//! 教程里的每一段宿主代码都对应这里的一个 `#[test]`；教程声称的每个返回值
//! 都被断言钉死。跑法：`cargo test --test tutorial_examples`。

use miniscript::{register_fn, Engine, Error, Opaque, Value};
use std::cell::RefCell;
use std::rc::Rc;

type Logs = Rc<RefCell<Vec<String>>>;

fn engine_with_log(logs: &Logs) -> Engine {
    let mut engine = Engine::new();
    let sink = Rc::clone(logs);
    register_fn!(engine, "log", (String), move |s| sink.borrow_mut().push(s)).unwrap();
    engine
}

/// 编译并调用 `main()`，返回脚本值。
fn run_main(engine: &Engine, src: &str) -> Value {
    let script = engine
        .load_str(src)
        .unwrap_or_else(|e| panic!("compile failed: {}", e));
    engine
        .call(&script, "main", vec![])
        .unwrap_or_else(|e| panic!("call failed: {}", e))
}

// ===========================================================================
// §1 五分钟上手
// ===========================================================================

#[test]
fn quick_start() {
    let mut engine = Engine::new();
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&logs);
    register_fn!(engine, "log", (String), move |s| {
        sink.borrow_mut().push(s)
    })
    .unwrap();

    // 脚本顶层只能放函数定义；流程逻辑包进 main() 由宿主调用
    let script = engine
        .load_str(
            "func double(x) { x * 2 }\n\
             func main() {\n    log(\"开始计算\")\n    double(21)\n}",
        )
        .unwrap();
    let result = engine.call(&script, "double", vec![Value::I64(21)]).unwrap();
    assert_eq!(result, Value::I64(42));

    engine.call(&script, "main", vec![]).unwrap();
    assert_eq!(logs.borrow().as_slice(), ["开始计算"]);
}

// ===========================================================================
// §2.1 函数与返回值
// ===========================================================================

#[test]
fn functions_and_returns() {
    let engine = Engine::new();

    // 末位表达式 = 返回值
    let script = engine.load_str("func add(a, b) { a + b }").unwrap();
    assert_eq!(
        engine.call(&script, "add", vec![Value::I64(2), Value::I64(3)]).unwrap(),
        Value::I64(5)
    );

    // 显式返回类型 + return 提前退出
    let script = engine
        .load_str("func div_safe(a, b) -> f64 {\n    if b == 0.0 {\n        return 0.0\n    }\n    a / b\n}")
        .unwrap();
    assert_eq!(
        engine.call(&script, "div_safe", vec![Value::I64(17), Value::I64(5)]).unwrap(),
        Value::F64(3.4)
    );
    assert_eq!(
        engine.call(&script, "div_safe", vec![Value::I64(10), Value::I64(0)]).unwrap(),
        Value::F64(0.0)
    );

    // 无返回值的函数返回 ()
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let engine = engine_with_log(&logs);
    let script = engine.load_str("func shout(msg: str) {\n    log(msg)\n    log(msg + \"!\")\n}").unwrap();
    assert_eq!(
        engine.call(&script, "shout", vec![Value::from("你好")]).unwrap(),
        Value::Unit
    );
    assert_eq!(logs.borrow().as_slice(), ["你好", "你好!"]);
}

// ===========================================================================
// §2.2 变量
// ===========================================================================

#[test]
fn variables() {
    let engine = Engine::new();
    assert_eq!(
        run_main(&engine, "func main() {\n    let x = 10\n    x = x + 5\n    let y = x * 2\n    y\n}"),
        Value::I64(30)
    );

    // 类型不能中途改变
    let err = engine
        .load_str("func main(x) {\n    x = 1\n    x = \"s\"\n    x\n}")
        .unwrap_err();
    assert!(err.message.contains("assignment to `x`"), "{}", err.message);
    assert!(err.message.contains("expected i64, found str"), "{}", err.message);
}

// ===========================================================================
// §2.3 控制流
// ===========================================================================

#[test]
fn control_flow_if() {
    let engine = Engine::new();
    let script = engine
        .load_str(
            "func classify(n) {\n    if n < 0 {\n        \"负数\"\n    } else if n == 0 {\n        \"零\"\n    } else {\n        \"正数\"\n    }\n}",
        )
        .unwrap();
    assert_eq!(engine.call(&script, "classify", vec![Value::I64(-5)]).unwrap(), Value::from("负数"));
    assert_eq!(engine.call(&script, "classify", vec![Value::I64(0)]).unwrap(), Value::from("零"));
    assert_eq!(engine.call(&script, "classify", vec![Value::I64(7)]).unwrap(), Value::from("正数"));

    // if 表达式直接赋值（else 必须有）
    let script = engine
        .load_str("func max_of(a, b) {\n    let bigger = if a > b { a } else { b }\n    bigger\n}")
        .unwrap();
    assert_eq!(engine.call(&script, "max_of", vec![Value::I64(3), Value::I64(9)]).unwrap(), Value::I64(9));
}

#[test]
fn control_flow_loops() {
    let engine = Engine::new();

    // for 是左闭右开：0..10 迭代 0..=9，奇数恰好 5 个
    let script = engine
        .load_str(
            "func count_odd_under(n) {\n    let c = 0\n    for i in 0..n {\n        if i % 2 == 0 {\n            continue\n        }\n        c = c + 1\n    }\n    c\n}",
        )
        .unwrap();
    assert_eq!(engine.call(&script, "count_odd_under", vec![Value::I64(10)]).unwrap(), Value::I64(5));

    // while + break
    let script = engine
        .load_str(
            "func first_square_over(n) {\n    let i = 0\n    while true {\n        if i * i > n {\n            break\n        }\n        i = i + 1\n    }\n    i\n}",
        )
        .unwrap();
    assert_eq!(engine.call(&script, "first_square_over", vec![Value::I64(30)]).unwrap(), Value::I64(6));

    // 递归
    let script = engine
        .load_str("func fib(n) { if n < 2 { n } else { fib(n-1) + fib(n-2) } }")
        .unwrap();
    assert_eq!(engine.call(&script, "fib", vec![Value::I64(12)]).unwrap(), Value::I64(144));
}

// ===========================================================================
// §2.4 类型系统
// ===========================================================================

#[test]
fn types_and_conversions() {
    let engine = Engine::new();

    let script = engine.load_str("func a() { to_i64(2.9) }").unwrap();
    assert_eq!(engine.call(&script, "a", vec![]).unwrap(), Value::I64(2));

    let script = engine.load_str("func b() { to_f64(3) }").unwrap();
    assert_eq!(engine.call(&script, "b", vec![]).unwrap(), Value::F64(3.0));

    let script = engine.load_str("func c() { to_str(42) }").unwrap();
    assert_eq!(engine.call(&script, "c", vec![]).unwrap(), Value::from("42"));

    // 脚本内不做隐式数值转换
    let err = engine.load_str("func d() { 1 + 2.5 }").unwrap_err();
    assert!(err.message.contains("type mismatch"), "{}", err.message);
}

// ===========================================================================
// §2.5 元组与多返回值
// ===========================================================================

#[test]
fn tuples_and_unit() {
    let engine = Engine::new();

    // divmod 与调用它的函数必须在同一份脚本里（脚本是独立编译单元）
    let script = engine
        .load_str(
            "func divmod(a, b) {\n    (a / b, a % b)\n}\n\
             func use_divmod() {\n    let r = divmod(17, 5)\n    let q = r.0\n    let rem = r.1\n    q * 10 + rem\n}",
        )
        .unwrap();
    assert_eq!(
        engine.call(&script, "divmod", vec![Value::I64(17), Value::I64(5)]).unwrap(),
        Value::Tuple(vec![Value::I64(3), Value::I64(2)])
    );
    assert_eq!(engine.call(&script, "use_divmod", vec![]).unwrap(), Value::I64(32));

    // () 是 unit
    let script = engine.load_str("func noop() { }").unwrap();
    assert_eq!(engine.call(&script, "noop", vec![]).unwrap(), Value::Unit);

    let script = engine.load_str("func f() { let u: () = ()\n u }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::Unit);
}

// ===========================================================================
// §2.6 字符串
// ===========================================================================

#[test]
fn strings() {
    let engine = Engine::new();

    // 参数标注 str/f64 —— to_str(未推断变量) 会被钉成 i64，所以 score 要标注
    let script = engine
        .load_str("func report(name: str, score: f64) {\n    \"玩家 \" + name + \" 得分 \" + to_str(score)\n}")
        .unwrap();
    assert_eq!(
        engine.call(&script, "report", vec![Value::from("小明"), Value::F64(3.5)]).unwrap(),
        Value::from("玩家 小明 得分 3.5")
    );

    // len 按字符数；contains / substr 按字符
    let script = engine.load_str("func l() { len(\"中文\") }").unwrap();
    assert_eq!(engine.call(&script, "l", vec![]).unwrap(), Value::I64(2));

    let script = engine
        .load_str("func check_password(pw: str) {\n    if len(pw) < 8 {\n        \"太短\"\n    } else if contains(pw, \"123\") {\n        \"太弱\"\n    } else {\n        \"通过\"\n    }\n}")
        .unwrap();
    assert_eq!(engine.call(&script, "check_password", vec![Value::from("abc")]).unwrap(), Value::from("太短"));
    assert_eq!(engine.call(&script, "check_password", vec![Value::from("abcdefgh123")]).unwrap(), Value::from("太弱"));
    assert_eq!(engine.call(&script, "check_password", vec![Value::from("abcdefgh")]).unwrap(), Value::from("通过"));

    let script = engine.load_str("func i(s: str) { substr(s, 0, 2) }").unwrap();
    assert_eq!(engine.call(&script, "i", vec![Value::from("中文abc")]).unwrap(), Value::from("中文"));
}

// ===========================================================================
// §2.8 内置函数白名单
// ===========================================================================

#[test]
fn builtins_are_numeric_generic() {
    let engine = Engine::new();

    // 整数证据 → i64 家族
    let script = engine.load_str("func m1() { min(1, 2) }").unwrap();
    assert_eq!(engine.call(&script, "m1", vec![]).unwrap(), Value::I64(1));

    // 无证据 → f64 家族；宿主传 i64 自动提升
    let script = engine.load_str("func pick(a, b) { min(a, b) }").unwrap();
    assert_eq!(
        engine.call(&script, "pick", vec![Value::I64(2), Value::I64(9)]).unwrap(),
        Value::F64(2.0)
    );

    // 一次调用里混型 → 编译报错
    let err = engine.load_str("func m2() { min(1, 2.5) }").unwrap_err();
    assert!(err.message.contains("min"), "{}", err.message);
    assert!(err.message.contains("type mismatch"), "{}", err.message);

    // sqrt 仅 f64
    let script = engine.load_str("func s(x: f64) { sqrt(x) }").unwrap();
    assert_eq!(engine.call(&script, "s", vec![Value::F64(9.0)]).unwrap(), Value::F64(3.0));
}

// ===========================================================================
// §3.1 register_fn! 的各种形态
// ===========================================================================

#[test]
fn register_fn_forms() {
    let mut engine = Engine::new();

    register_fn!(engine, "add", (i64, i64) -> i64, |a, b| a + b).unwrap();
    register_fn!(engine, "add_short", (i64, i64), |a, b| a + b).unwrap();
    register_fn!(engine, "answer", (), || 42).unwrap();
    register_fn!(engine, "fail_fast", (i64) -> Result<i64, Error>, |x| {
        if x < 0 {
            Err(Error::runtime("负数不接受"))
        } else {
            Ok(x)
        }
    })
    .unwrap();

    let script = engine.load_str("func go(a, b) { add(a, b) + add_short(a, b) }").unwrap();
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(1), Value::I64(2)]).unwrap(),
        Value::I64(6)
    );

    let script = engine.load_str("func ask() { answer() }").unwrap();
    assert_eq!(engine.call(&script, "ask", vec![]).unwrap(), Value::I64(42));

    // Result 返回的宿主函数：Err 传到脚本侧表现为运行时错误
    let script = engine.load_str("func try_it(x) { fail_fast(x) }").unwrap();
    let err = engine
        .call(&script, "try_it", vec![Value::I64(-5)])
        .unwrap_err();
    assert!(err.message.contains("host function: 负数不接受"), "{}", err.message);
    assert_eq!(
        engine.call(&script, "try_it", vec![Value::I64(5)]).unwrap(),
        Value::I64(5)
    );

    // 同名重复注册直接报错
    let dup = register_fn!(engine, "add", (i64, i64) -> i64, |a, b| a + b);
    assert!(dup.is_err());
}

// ===========================================================================
// §3.2 调用与传参
// ===========================================================================

#[test]
fn value_constructions() {
    let a = Value::I64(21);
    let b: Value = 21.into();
    assert_eq!(a, b);
    assert_eq!(Value::from("文本"), Value::Str("文本".into()));
    assert_eq!(Value::from(true), Value::Bool(true));
    assert_eq!(Value::F64(2.5).ty_string(), "f64");
}

// 给 Value 一个只为本测试服务的类型名辅助（避免依赖 display 细节）
trait TyString {
    fn ty_string(&self) -> String;
}
impl TyString for Value {
    fn ty_string(&self) -> String {
        match self {
            Value::I64(_) => "i64".into(),
            Value::F64(_) => "f64".into(),
            Value::Bool(_) => "bool".into(),
            Value::Str(_) => "str".into(),
            Value::Handle(_) => "handle".into(),
            Value::Tuple(_) => "tuple".into(),
            Value::Unit => "()".into(),
        }
    }
}

// ===========================================================================
// §3.3 类型边界（默认 i64 + 标注逃生舱）
// ===========================================================================

#[test]
fn type_boundary_defaults_and_annotations() {
    let engine = Engine::new();

    // 有字符串线索 → 自动推断为 str
    let ok = engine.load_str("func greet(name) { \"hi \" + name }").unwrap();
    assert_eq!(
        engine.call(&ok, "greet", vec![Value::from("小明")]).unwrap(),
        Value::from("hi 小明")
    );

    // 无线索 → 默认 i64 → 传字符串被边界校验拒绝（而非静默算错）
    let bad = engine.load_str("func echo(x) { x }").unwrap();
    let err = engine.call(&bad, "echo", vec![Value::from("文本")]).unwrap_err();
    assert!(err.message.contains("argument type mismatch"), "{}", err.message);
    assert!(err.message.contains("Annotate the parameter"), "{}", err.message);

    // 标注之后就好了
    let good = engine.load_str("func echo(x: str) { x }").unwrap();
    assert_eq!(engine.call(&good, "echo", vec![Value::from("文本")]).unwrap(), Value::from("文本"));

    // f64 参数收 i64 自动提升
    let script = engine
        .load_str("func avg(a: f64, b: f64) { (a + b) / 2.0 }")
        .unwrap();
    assert_eq!(
        engine.call(&script, "avg", vec![Value::I64(3), Value::I64(4)]).unwrap(),
        Value::F64(3.5)
    );
}

// ===========================================================================
// §3.4 错误处理与诊断
// ===========================================================================

#[test]
fn errors_and_reports() {
    let engine = Engine::new();
    let script = engine.load_str("func go(x) { 100 / x }").unwrap();

    // 单行 Display
    let err = engine.call(&script, "go", vec![Value::I64(0)]).unwrap_err();
    assert!(err.to_string().contains("division by zero"), "{}", err);

    // 多行报告：--> 位置 + 源码行 + caret + 调用栈
    let report = script.report(&err);
    assert!(report.contains("-->"), "{}", report);
    assert!(report.contains("call stack"), "{}", report);
    assert!(report.contains("100 / x"), "{}", report);
    assert!(report.contains('^'), "{}", report);

    // 编译错误也能渲染（report 需要：源码 + 文件名）。`x +` 吃掉换行后
    // 在第三行的 `}` 处暴露，所以位置是 3:1。
    let src = "func go(x) {\n  x +\n}";
    let cerr = engine.load_str(src).unwrap_err();
    let creport = cerr.report(src, "broken.ms");
    assert!(creport.contains("--> broken.ms:3:1"), "{}", creport);
}

// ===========================================================================
// §3.5 Opaque<T>
// ===========================================================================

struct Player {
    name: String,
    hp: i64,
}

#[test]
fn opaque_handles() {
    let mut engine = Engine::new();
    register_fn!(engine, "heal", (Opaque<Player>, i64) -> String, |p, amount| {
        format!("{} 回了 {} 血，现 HP {}", p.name, amount, p.hp + amount)
    })
    .unwrap();

    let script = engine.load_str("func buff(p, n) { heal(p, n) }").unwrap();
    let p = Value::from(Opaque::new(Player { name: "战士".into(), hp: 100 }));
    let result = engine.call(&script, "buff", vec![p, Value::I64(30)]).unwrap();
    assert_eq!(result, Value::from("战士 回了 30 血，现 HP 130"));
}

// ===========================================================================
// §3.6 宿主返回元组（2–8）
// ===========================================================================

#[test]
fn host_tuple_returns() {
    let mut engine = Engine::new();
    register_fn!(engine, "hsl_to_rgb", (i64) -> (i64, i64, i64), |h| (h * 2, h * 3, h * 4)).unwrap();
    register_fn!(engine, "take3", ((i64, i64, i64)) -> i64, |t| t.0 + t.1 + t.2).unwrap();

    let script = engine
        .load_str("func use_color() {\n    let c = hsl_to_rgb(10)\n    c.0 + c.1 + c.2\n}")
        .unwrap();
    assert_eq!(engine.call(&script, "use_color", vec![]).unwrap(), Value::I64(90));

    // 元组字面量作实参跨边界（((i64,i64,i64)) 是一个元组类型的参数）
    let script = engine.load_str("func pass_through(t) { take3(t) }").unwrap();
    assert_eq!(
        engine
            .call(
                &script,
                "pass_through",
                vec![Value::Tuple(vec![Value::I64(1), Value::I64(2), Value::I64(3)])]
            )
            .unwrap(),
        Value::I64(6)
    );
}

// ===========================================================================
// §3.7 热重载
// ===========================================================================

#[test]
fn hot_reload() {
    let dir = std::env::temp_dir().join(format!("ms_tutorial_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("greet.ms");
    std::fs::write(&path, "func greet(name: str) { \"hello \" + name }").unwrap();

    let engine = Engine::new();
    let mut script = engine.load_file(&path).unwrap();
    assert_eq!(
        engine.call(&script, "greet", vec![Value::from("world")]).unwrap(),
        Value::from("hello world")
    );

    // 改文件 → 原地换字节码，宿主不重启
    std::fs::write(&path, "func greet(name: str) { \"你好 \" + name }").unwrap();
    engine.reload(&mut script, &path).unwrap();
    assert_eq!(
        engine.call(&script, "greet", vec![Value::from("世界")]).unwrap(),
        Value::from("你好 世界")
    );

    // reload 失败时旧脚本原样保留
    std::fs::write(&path, "func greet( {").unwrap();
    assert!(engine.reload(&mut script, &path).is_err());
    assert_eq!(
        engine.call(&script, "greet", vec![Value::from("世界")]).unwrap(),
        Value::from("你好 世界")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// §3.8 安全三件套
// ===========================================================================

#[test]
fn safety_limits() {
    // gas：死循环被掐
    let engine = Engine::new().with_limits(1_000, 64);
    let script = engine.load_str("func loop_forever() { while true { 1 } }").unwrap();
    let err = engine.call(&script, "loop_forever", vec![]).unwrap_err();
    assert!(err.message.contains("gas"), "{}", err.message);

    // 内存配额：无限翻倍字符串被掐
    let engine = Engine::new().with_heap_limit(1_000);
    let script = engine
        .load_str("func grow() { let s = \"x\"\n for i in 0..100 { s = s + s }\n len(s) }")
        .unwrap();
    let err = engine.call(&script, "grow", vec![]).unwrap_err();
    assert!(err.message.contains("memory limit"), "{}", err.message);

    // panic 边界：宿主回调 panic 变成 Err，不炸宿主进程。
    // （宿主函数本体一定 panic，用 raw API 注册——类型化宏要求可推导的返回类型）
    let mut engine = Engine::new();
    engine
        .register_fn("boom", vec![], miniscript::ty::Ty::Unit, |_| panic!("boom!"))
        .unwrap();
    let script = engine.load_str("func go() { boom() }").unwrap();
    let err = engine.call(&script, "go", vec![]).unwrap_err();
    assert!(err.message.contains("panic"), "{}", err.message);
}

// ===========================================================================
// §4.1 字节码缓存
// ===========================================================================

#[test]
fn bytecode_cache() {
    let engine = Engine::new();

    // 内存往返
    let script = engine.load_str("func add(a, b) { a + b }").unwrap();
    let bytes = engine.script_to_bytes(&script);
    let restored = engine.script_from_bytes(&bytes).unwrap();
    assert_eq!(
        engine.call(&restored, "add", vec![Value::I64(2), Value::I64(3)]).unwrap(),
        Value::I64(5)
    );

    // 磁盘缓存：第二次 compile_cached 命中（源码 hash 相同）
    let dir = std::env::temp_dir().join(format!("ms_tutorial_cache_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("demo.sbc");
    let src = "func add(a, b) { a + b }";
    let s1 = engine.compile_cached(src, "demo", &cache).unwrap();
    assert!(cache.exists(), "cache written best-effort");
    let s2 = engine.compile_cached(src, "demo", &cache).unwrap();
    assert_eq!(
        engine.call(&s2, "add", vec![Value::I64(2), Value::I64(3)]).unwrap(),
        Value::I64(5)
    );
    let _ = s1;
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// §4.2 线程模型
// ===========================================================================

#[test]
fn one_engine_per_thread() {
    // 注意：Value 不是 Send（Rc 承载），跨线程只传标量——
    // 在线程内部把 Value 转成 i64/f64/bool 再交出去。
    let handles: Vec<_> = (0..4)
        .map(|tid| {
            std::thread::spawn(move || {
                let mut engine = Engine::new();
                let bonus = tid as i64 * 100;
                register_fn!(engine, "add_bonus", (i64) -> i64, move |x| x + bonus).unwrap();
                let script = engine.load_str("func go(x) { add_bonus(x) }").unwrap();
                let v = engine.call(&script, "go", vec![Value::I64(1)]).unwrap();
                match v {
                    Value::I64(n) => n,
                    _ => panic!("expected i64"),
                }
            })
        })
        .collect();
    for (tid, h) in handles.into_iter().enumerate() {
        assert_eq!(h.join().unwrap(), 1 + tid as i64 * 100);
    }
}

#[test]
fn tuples_deep_dive() {
    let engine = Engine::new();

    // 单元素元组要带尾逗号；不带逗号的括号只是分组
    let script = engine.load_str("func f() { let s = (1,)\n s.0 }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(1));

    // 嵌套元组连点取值
    let script = engine.load_str("func f() { let p = ((1, 2), (3, 4))\n p.1.0 }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(3));

    // 索引必须是字面量（取第几个在编译期确定）
    let err = engine.load_str("func f(t, i) { t.i }").unwrap_err();
    assert!(err.message.contains("expected tuple index"), "{}", err.message);

    // 已知长度的越界是编译错误
    let err = engine.load_str("func f() { let t = (1, 2)\n t.5 }").unwrap_err();
    assert!(err.message.contains("tuple index 5 out of range"), "{}", err.message);

    // 元组相等被禁止（内部是堆索引，比相等会静默错值）
    let err = engine.load_str("func f() { (1, 2) == (1, 2) }").unwrap_err();
    assert!(err.message.contains("equality"), "{}", err.message);

    // 标志位模式：用 (ok, value) 表达"可能失败"
    let script = engine
        .load_str(
            "func safe_div(a: f64, b: f64) {\n    if b == 0.0 {\n        (false, 0.0)\n    } else {\n        (true, a / b)\n    }\n}\n\
             func use2() {\n    let ok = safe_div(10.0, 2.0)\n    if ok.0 {\n        ok.1\n    } else {\n        0.0\n    }\n}",
        )
        .unwrap();
    assert_eq!(engine.call(&script, "use2", vec![]).unwrap(), Value::F64(5.0));

    // "至少 N 元"推断：宿主传更长的元组，多出的元素无害
    let script = engine.load_str("func first2(t) { t.0 * 10 + t.1 }").unwrap();
    assert_eq!(
        engine
            .call(
                &script,
                "first2",
                vec![Value::Tuple(vec![
                    Value::I64(1),
                    Value::I64(2),
                    Value::I64(99)
                ])]
            )
            .unwrap(),
        Value::I64(12)
    );
}

// ===========================================================================
// §2.4 补充：整数边界（FAQ Q13）
// ===========================================================================

#[test]
fn integer_boundaries() {
    let engine = Engine::new();

    // 42 只是教程表格里的例子——i64 装得下的都合法
    let script = engine.load_str("func f() { 9223372036854775807 }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(i64::MAX));

    let script = engine.load_str("func f() { 1234567890123 }").unwrap();
    assert_eq!(engine.call(&script, "f", vec![]).unwrap(), Value::I64(1234567890123));

    // 字面量超范围 → 编译报错
    let err = engine.load_str("func f() { 9223372036854775808 }").unwrap_err();
    assert!(err.message.contains("integer literal overflows i64"), "{}", err.message);

    // 最小值写不成字面量：先解析数字（溢出）再取负
    let err = engine.load_str("func f() { -9223372036854775808 }").unwrap_err();
    assert!(err.message.contains("integer literal overflows i64"), "{}", err.message);

    // 运算溢出 → 运行时报错，不回绕
    let script = engine.load_str("func f(x) { x + 1 }").unwrap();
    let err = engine.call(&script, "f", vec![Value::I64(i64::MAX)]).unwrap_err();
    assert!(err.message.contains("i64 add overflow"), "{}", err.message);

    let script = engine.load_str("func f() { 9223372036854775807 * 2 }").unwrap();
    let err = engine.call(&script, "f", vec![]).unwrap_err();
    assert!(err.message.contains("i64 mul overflow"), "{}", err.message);
}

// ===========================================================================
// §5 FAQ
// ===========================================================================

#[test]
fn faq_common_pitfalls() {
    let engine = Engine::new();

    // Q1 没有分号
    let err = engine.load_str("let x = 1;").unwrap_err();
    assert!(err.message.contains("';'"), "{}", err.message);

    // Q6 内置名是保留字
    let err = engine.load_str("func len(s) { s }").unwrap_err();
    assert!(err.message.contains("reserved built-in"), "{}", err.message);

    // Q8 嵌套上限 64
    let deep = format!("func f(x) {{ {}x{} }}", "(".repeat(100), ")".repeat(100));
    let err = engine.load_str(&deep).unwrap_err();
    assert!(err.message.contains("nesting too deep"), "{}", err.message);

    // Q9 一次调用最多 8 个实参：函数可以定义 9 个参数，但调用时
    // typecheck 先放行（9 == 9），codegen 的操作数上限再拦
    let err = engine
        .load_str("func f(a, b, c, d, e, g, h, i, j) { a }\nfunc go() { f(1, 2, 3, 4, 5, 6, 7, 8, 9) }")
        .unwrap_err();
    assert!(err.message.contains("the maximum is 8"), "{}", err.message);

    // 元组字面量同样上限 8
    let err = engine
        .load_str("func go() { (1, 2, 3, 4, 5, 6, 7, 8, 9) }")
        .unwrap_err();
    assert!(err.message.contains("the maximum is 8"), "{}", err.message);

    // Q5 if 表达式必须有 else
    let err = engine.load_str("func f(c) { let x = if c { 1 }\n x }").unwrap_err();
    assert!(err.to_string().contains("Else"), "{}", err);

    // Q11 "抛异常"的两种对应物：
    //   a) 宿主函数返回 Result（脚本失败终止）
    //   b) 宿主函数返回 (ok, err) 探针元组（脚本自己分支）
    let mut engine = Engine::new();
    register_fn!(engine, "probe", (i64) -> (bool, String), |x| {
        if x < 0 {
            (false, format!("{} 为负", x))
        } else {
            (true, String::new())
        }
    })
    .unwrap();
    let script = engine
        .load_str(
            "func go(x) {\n    let p = probe(x)\n    if p.0 {\n        \"ok\"\n    } else {\n        \"失败: \" + p.1\n    }\n}",
        )
        .unwrap();
    assert_eq!(engine.call(&script, "go", vec![Value::I64(3)]).unwrap(), Value::from("ok"));
    assert_eq!(
        engine.call(&script, "go", vec![Value::I64(-2)]).unwrap(),
        Value::from("失败: -2 为负")
    );
}
