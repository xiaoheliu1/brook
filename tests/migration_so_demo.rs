//! android_so_demo 迁移 POC —— 用 brook 忠实改写两个真实脚本，验证
//! 「标量面直接接住 + N 元组化 Map 返回 + Opaque 记录集 + OK/ERR 契约」。
//!
//! 原脚本（rhai）:
//!   * `.push/com.example.t_wifi.rhai` —— 顶层语句流 + 字符串拼接 + log。
//!     rhai 的整文件即程序在 brook 里包成 `main()`（ADR-013 模式），
//!     失败通道从 throw 变成宿主 `Err` → `ERR\t...` 报告串。
//!   * `scripts/bind_verify.rhai` —— img_screen_size/img_crop 的 Map 字段
//!     访问 → 元组；img_find_all/img_find_bind 的 Array<Map> → Opaque +
//!     count/name/x/y 索引 getter；try/catch(win<0) → 预期失败探针
//!     `(ok, err)` 二元组；`to_string()` → `to_str()`。
//!
//! API 形状改写记录（宿主 v2 相对 rhai 版）：
//!   * `img_screen_size() -> Map`            → `(w, h)`
//!   * `img_crop(...) -> Map{x,y,w,h}`       → `(x, y, w, h)`
//!   * `img_find_all(name, max) -> Array`    → `Opaque` + `find_count`
//!   * `img_find_bind(main, [[c,dx,dy]], win) -> Array<Map>` →
//!     `img_find_bind(main, c, dx, dy, win) -> Result<Opaque, _>`——
//!     原 API 收"伴随模板三元组数组"，但全部 3 个真实调用点都只传 1 个伴随，
//!     数组形状是过度泛化，标量化后语义不变。
//!   * try/catch 的替代：正常路径不需要 catch（rhai 的 try/catch 253 处里
//!     绝大多数是"预期内失败继续走"），预期失败用 `(ok, err)` 探针变体。

use brook::{register_fn, Engine, Error, Opaque, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// ---- 模拟 so_demo 的宿主状态与 Kotlin 回调 ----

#[derive(Clone)]
struct BindHit {
    name: String,
    x: i64,
    y: i64,
}

#[derive(Clone, Default)]
struct BindList {
    hits: Vec<BindHit>,
}

struct Task {
    title: RefCell<String>,
    steps: Cell<i64>,
    errors: RefCell<Vec<String>>,
}

type Logs = Rc<RefCell<Vec<String>>>;

fn log_sink(logs: &Logs) -> impl Fn(String) + 'static {
    let logs = Rc::clone(logs);
    move |s| logs.borrow_mut().push(s)
}

/// 模拟 so_demo 的 `run_script` 报告协议（lib.rs:1880-1884）：成败只看
/// `OK\t` / `ERR\t` 前缀，脚本末值被丢弃。
fn run_report(engine: &Engine, script: &brook::Script) -> String {
    match engine.call(script, "main", vec![]) {
        Ok(_) => "OK\t脚本执行完成".to_string(),
        Err(e) => format!("ERR\t{}", e),
    }
}

fn build_engine(logs: &Logs, root_gate: Rc<Cell<bool>>) -> Engine {
    let mut engine = Engine::new();

    // ---- 标量面（签名与 so_demo 一致）----
    let sink = log_sink(logs);
    register_fn!(engine, "log", (String), move |s| sink(s)).unwrap();
    register_fn!(engine, "set_wifi", (bool) -> String, move |_on| {
        if root_gate.get() { "ok".to_string() } else { "无 root".to_string() }
    })
    .unwrap();
    register_fn!(engine, "request_capture", (), || true).unwrap();

    // ---- N 元组返回（rhai Map → 元组）----
    register_fn!(engine, "img_screen_size", () -> (i64, i64), || (1088, 2400)).unwrap();
    register_fn!(engine, "img_crop", (String, i64, i64, i64, i64) -> (i64, i64, i64, i64),
        |name, x, y, w, h| { let _ = name; (x, y, w, h) })
    .unwrap();
    register_fn!(engine, "img_tol", (i64) -> i64, |t| t).unwrap();

    // ---- 变长记录集 → Opaque + count/indexed getter ----
    register_fn!(engine, "img_find_all", (String, i64) -> Opaque<BindList>, |name, max| {
        // 模拟:当前屏上有 2 处与模板一致(裁自同一帧)。
        let _ = (name, max);
        Opaque::new(BindList {
            hits: vec![
                BindHit { name: "templates/_bindA.png".into(), x: 100, y: 575 },
                BindHit { name: "templates/_bindA.png".into(), x: 100, y: 575 },
            ],
        })
    })
    .unwrap();

    // 抛错版（对应 rhai 的 Result → 可 throw）： happy path 用。
    register_fn!(engine, "img_find_bind",
        (String, String, i64, i64, i64) -> Result<Opaque<BindList>, Error>,
        |main, companion, dx, dy, win| {
            if win < 0 {
                return Err(Error::runtime(format!("invalid window: {}", win)));
            }
            let mut hits = Vec::new();
            if dx.abs() < 2000 && dy.abs() < 2000 {
                hits.push(BindHit { name: main, x: 100, y: 575 });
                hits.push(BindHit { name: companion, x: 100 + dx, y: 575 + dy });
            }
            Ok(Opaque::new(BindList { hits }))
        })
    .unwrap();
    // 预期失败探针（对应 rhai 的 try/catch）：不抛错，(ok, err) 二元组。
    register_fn!(engine, "bind_win_probe",
        (String, String, i64, i64, i64) -> (bool, String),
        |main, companion, dx, dy, win| {
            let _ = (&main, &companion, &dx, &dy);
            if win < 0 {
                (false, format!("invalid window: {}", win))
            } else {
                (true, String::new())
            }
        })
    .unwrap();
    register_fn!(engine, "bind_count", (Opaque<BindList>) -> i64, |l| l.hits.len() as i64).unwrap();
    register_fn!(engine, "bind_name", (Opaque<BindList>, i64) -> String, |l, i| l.hits[i as usize].name.clone()).unwrap();
    register_fn!(engine, "bind_x", (Opaque<BindList>, i64) -> i64, |l, i| l.hits[i as usize].x).unwrap();
    register_fn!(engine, "bind_y", (Opaque<BindList>, i64) -> i64, |l, i| l.hits[i as usize].y).unwrap();

    // ---- Task 句柄（rhai 的 &mut Task 方法调用 → Opaque + 内部可变性）----
    register_fn!(engine, "new_task", (String) -> Opaque<Task>, |t| {
        Opaque::new(Task { title: RefCell::new(t), steps: Cell::new(0), errors: RefCell::new(vec![]) })
    })
    .unwrap();
    register_fn!(engine, "task_step", (Opaque<Task>, String) -> i64, |t, msg| {
        let _ = msg;
        t.steps.set(t.steps.get() + 1);
        t.steps.get()
    })
    .unwrap();
    register_fn!(engine, "task_fail", (Opaque<Task>, String), |t, msg| {
        t.errors.borrow_mut().push(msg)
    })
    .unwrap();
    register_fn!(engine, "task_set_title", (Opaque<Task>, String), |t, s| *t.title.borrow_mut() = s).unwrap();
    register_fn!(engine, "task_title", (Opaque<Task>) -> String, |t| t.title.borrow().clone()).unwrap();
    register_fn!(engine, "task_steps", (Opaque<Task>) -> i64, |t| t.steps.get()).unwrap();
    register_fn!(engine, "task_summary", (Opaque<Task>) -> String, |t| format!(
        "{}: {} 步 {} 错",
        t.title.borrow(),
        t.steps.get(),
        t.errors.borrow().len()
    ))
    .unwrap();

    // ---- 失败通道（对应 rhai 的 throw）----
    register_fn!(engine, "hard_fail", () -> Result<i64, Error>, || {
        Err(Error::runtime("boom"))
    })
    .unwrap();

    engine
}

fn compile(engine: &Engine, src: &str) -> brook::Script {
    engine
        .load_str(src)
        .unwrap_or_else(|e| panic!("script should compile: {}", e))
}

fn last_log(logs: &Logs) -> String {
    logs.borrow().last().cloned().unwrap_or_default()
}

// ---- 端口 1：t_wifi —— 标量面 + OK/ERR 契约 ----

const T_WIFI: &str = r#"
func main() {
    let on = set_wifi(true)
    log("set_wifi(true) -> " + on)
}
"#;

#[test]
fn t_wifi_scalar_surface_with_ok_err_contract() {
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));

    // root 总闸开 → 真 ok
    let gate = Rc::new(Cell::new(true));
    let engine = build_engine(&logs, Rc::clone(&gate));
    let script = compile(&engine, T_WIFI);
    let report = run_report(&engine, &script);
    assert_eq!(report, "OK\t脚本执行完成");
    assert_eq!(last_log(&logs), "set_wifi(true) -> ok");

    // 同一脚本重复执行（保留编译产物的宿主形态）
    logs.borrow_mut().clear();
    assert_eq!(run_report(&engine, &script), "OK\t脚本执行完成");
    assert_eq!(last_log(&logs), "set_wifi(true) -> ok");

    // 无 root：同一份字节码，行为随宿主状态变化
    let gate_off = Rc::new(Cell::new(false));
    let logs2: Logs = Rc::new(RefCell::new(Vec::new()));
    let engine2 = build_engine(&logs2, gate_off);
    let script2 = compile(&engine2, T_WIFI);
    assert_eq!(run_report(&engine2, &script2), "OK\t脚本执行完成");
    assert_eq!(last_log(&logs2), "set_wifi(true) -> 无 root");

    // 编译失败 → ERR 前缀（rhai 的编译错走同一通道）
    let err = engine
        .load_str("func main() { let x = }\n}")
        .unwrap_err();
    let report = format!("ERR\t{}", err);
    assert!(report.starts_with("ERR\t"), "{}", report);

    // 脚本失败通道（rhai throw 的对应物：宿主 Err）
    let fail_script = compile(&engine, "func main() { hard_fail() }");
    let report = run_report(&engine, &fail_script);
    assert!(report.starts_with("ERR\t"), "{}", report);
    assert!(report.contains("host function: boom"), "{}", report);
}

// ---- 端口 2：bind_verify —— Map→元组、Array<Map>→Opaque、try/catch→探针 ----

const BIND_VERIFY: &str = r#"
func main() {
    if !request_capture() {
        log("BIND 无截屏权限")
        return
    }
    let sz = img_screen_size()
    let W = sz.0
    let H = sz.1
    log("BIND 帧 " + to_str(W) + "x" + to_str(H))

    img_tol(0)

    let pa = img_crop("templates/_bindA.png", 100, 575, 270, 68)
    let pb = img_crop("templates/_bindB.png", 100, 1663, 270, 68)

    let cax = pa.0 + pa.2 / 2
    let cay = pa.1 + pa.3 / 2
    let cbx = pb.0 + pb.2 / 2
    let cby = pb.1 + pb.3 / 2
    let dx = cbx - cax
    let dy = cby - cay
    log("BIND A中心=" + to_str(cax) + "," + to_str(cay) +
        " B中心=" + to_str(cbx) + "," + to_str(cby) +
        " 偏移=" + to_str(dx) + "," + to_str(dy))

    let allA = img_find_all("templates/_bindA.png", 8)
    log("BIND A 命中数(<=8)=" + to_str(bind_count(allA)))

    let r1 = img_find_bind("templates/_bindA.png", "templates/_bindB.png", dx, dy, 8)
    log("BIND 正例 个数=" + to_str(bind_count(r1)) + " (期望 2)")
    if bind_count(r1) >= 2 {
        log("  [0] " + bind_name(r1, 0) + " @" + to_str(bind_x(r1, 0)) + "," + to_str(bind_y(r1, 0)) +
            " (应≈" + to_str(cax) + "," + to_str(cay) + ")")
        log("  [1] " + bind_name(r1, 1) + " @" + to_str(bind_x(r1, 1)) + "," + to_str(bind_y(r1, 1)) +
            " (应≈" + to_str(cbx) + "," + to_str(cby) + ")")
    }

    let rs = img_find_bind("templates/_bindA.png", "templates/_bindA.png", 0, 0, 4)
    log("BIND 自绑(0,0) 个数=" + to_str(bind_count(rs)) + " (期望 2)")

    let r2 = img_find_bind("templates/_bindA.png", "templates/_bindB.png", 4000, 4000, 8)
    log("BIND 反例(远偏移) 个数=" + to_str(bind_count(r2)) + " (期望 0)")

    let probe = bind_win_probe("templates/_bindA.png", "templates/_bindB.png", dx, dy, -1)
    if probe.0 {
        log("BIND win<0 未报错(不该!)")
    } else {
        log("BIND win<0 如期报错: " + probe.1)
    }
}
"#;

#[test]
fn bind_verify_ported_with_tuples_and_opaque_collections() {
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let engine = build_engine(&logs, Rc::new(Cell::new(true)));
    let script = compile(&engine, BIND_VERIFY);
    assert_eq!(run_report(&engine, &script), "OK\t脚本执行完成");

    let got = logs.borrow().clone();
    let expect = vec![
        "BIND 帧 1088x2400".to_string(),
        // pa=(100,575,270,68) → 中心 (235,609)；pb=(100,1663,270,68) → (235,1697)
        "BIND A中心=235,609 B中心=235,1697 偏移=0,1088".to_string(),
        "BIND A 命中数(<=8)=2".to_string(),
        "BIND 正例 个数=2 (期望 2)".to_string(),
        "  [0] templates/_bindA.png @100,575 (应≈235,609)".to_string(),
        "  [1] templates/_bindB.png @100,1663 (应≈235,1697)".to_string(),
        "BIND 自绑(0,0) 个数=2 (期望 2)".to_string(),
        "BIND 反例(远偏移) 个数=0 (期望 0)".to_string(),
        "BIND win<0 如期报错: invalid window: -1".to_string(),
    ];
    assert_eq!(got, expect, "ported bind_verify must produce identical log flow");
}

// ---- 端口 3：Task 句柄（rhai 的 &mut Task 方法 → Opaque + 内部可变性）----

const TASK_SCRIPT: &str = r#"
func main() {
    let t = new_task("冒烟")
    let _ = task_step(t, "截图")
    task_fail(t, "找不到按钮")
    task_set_title(t, "冒烟-重试")
    log(task_title(t) + "/" + to_str(task_steps(t)) + "/" + task_summary(t))
}
"#;

#[test]
fn task_handle_with_interior_mutability() {
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let engine = build_engine(&logs, Rc::new(Cell::new(true)));
    let script = compile(&engine, TASK_SCRIPT);
    assert_eq!(run_report(&engine, &script), "OK\t脚本执行完成");
    assert_eq!(last_log(&logs), "冒烟-重试/1/冒烟-重试: 1 步 1 错");
}

// ---- 端口 4：rhai_demo 的两大硬点 —— Page 对象与插件 Map 传参 ----
//
// 背景（docs/migration/rhai_demo_inventory.md）：
//   * browser.rs 的 Page（39 个方法，&mut Page 首参）→ Opaque + 普通函数
//   * system.rs 的 run_plugin(Map) -> Map → 信封扁平化：argv 动态数组在
//     全部真实调用点本质是"exe + 脚本 + 空白分隔参数串"，payload 本来就
//     以 JSON 字符串形态流动（json_input / stdout），Map 只是信封。
//
// 另验证 rhai 的 `let p = ()` 哨兵模式（先空后 Page）在 brook 里
// 用「作用域重构」替代——把用 p 的逻辑搬进获取 p 的分支。

struct Page {
    url: RefCell<String>,
    closed: Cell<bool>,
}

const BROWSER_POC: &str = r#"
func main() {
    let p = browser_open("task.html")
    let f = page_find(p, "同意")
    if f.0 {
        log("命中 " + f.2 + " hits=" + to_str(f.1))
        page_close(p)
        log("closed=" + to_str(page_is_closed(p)))
    } else {
        log("没找到")
    }
}
"#;

#[test]
fn browser_page_maps_to_opaque() {
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Engine::new();
    let sink = Rc::clone(&logs);
    register_fn!(engine, "log", (String), move |s| sink.borrow_mut().push(s)).unwrap();

    // rhai: register_type_with_name::<Page>("Page") → Opaque<Page>
    register_fn!(engine, "browser_open", (String) -> Result<Opaque<Page>, Error>, |url| {
        Ok(Opaque::new(Page { url: RefCell::new(url), closed: Cell::new(false) }))
    })
    .unwrap();
    // rhai: p.find(text) → Map{found,hits,id}；真实消费字段是 .found/.hits/.id
    // → (bool, i64, String) 三元组（mock：任务页里有"同意"按钮）
    register_fn!(engine, "page_find", (Opaque<Page>, String) -> (bool, i64, String), |p, text| {
        let url = p.url.borrow();
        if url.contains("task") && text == "同意" {
            (true, 4, "okbtn".into())
        } else {
            (false, 0, String::new())
        }
    })
    .unwrap();
    register_fn!(engine, "page_close", (Opaque<Page>), |p| p.closed.set(true)).unwrap();
    register_fn!(engine, "page_is_closed", (Opaque<Page>) -> bool, |p| p.closed.get()).unwrap();

    let script = engine.load_str(BROWSER_POC).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::Unit);
    assert_eq!(
        logs.borrow().as_slice(),
        ["命中 okbtn hits=4", "closed=true"]
    );

    // 类型护栏：把别的句柄递给 page_find 会被边界拒绝
    let wrong = engine
        .load_str("func go(t) { page_find(t, \"x\") }")
        .unwrap();
    let err = engine.call(
        &wrong,
        "go",
        vec![Value::from(Opaque::new(BindList::default()))],
    );
    assert!(err.is_err(), "BindList is not a Page");
}

// rhai: run_plugin(spec Map) → 信封扁平化 + JSON 字符串透传
const PLUGIN_POC: &str = r#"
func main() {
    // 原 rhai：动态数组 argv + 条件 push → 字符串拼接（shell 风格）
    let args = "-f jsonl --timing"
    args = args + " -l zh"

    // 原 rhai：json_str(#{image: "x.png", min_conf: 0.9}) → 字段少手动拼
    let payload = "{\"image\":\"x.png\",\"min_conf\":0.9}"

    let m = run_plugin("python", "plugins/nbocr.py", args, payload)
    if m.0 == 0 {
        log("code=0 stdout=" + m.1)
    } else {
        log("stderr=" + m.2)
    }
}
"#;

#[test]
fn run_plugin_map_flattens_to_params_and_json_passthrough() {
    let logs: Logs = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Engine::new();
    let sink = Rc::clone(&logs);
    register_fn!(engine, "log", (String), move |s| sink.borrow_mut().push(s)).unwrap();

    register_fn!(engine, "run_plugin",
        (String, String, String, String) -> Result<(i64, String, String), Error>,
        |exe, script, args, json_input| {
            // mock：插件崩溃通道走 Err（rhai 侧 try/catch 的对应物）
            if args.contains("--boom") {
                return Err(Error::runtime("plugin crashed"));
            }
            let _ = (exe, script);
            Ok((0, format!("echoed {}", json_input), String::new()))
        })
    .unwrap();

    let script = engine.load_str(PLUGIN_POC).unwrap();
    assert_eq!(engine.call(&script, "main", vec![]).unwrap(), Value::Unit);
    assert_eq!(
        logs.borrow().as_slice(),
        ["code=0 stdout=echoed {\"image\":\"x.png\",\"min_conf\":0.9}"]
    );

    // 插件崩溃 → Err → engine.call 失败（宿主组装 ERR\t 报告）
    let crash = engine.load_str("func main() { run_plugin(\"p\", \"s\", \"--boom\", \"{}\") }").unwrap();
    let err = engine.call(&crash, "main", vec![]).unwrap_err();
    assert!(err.message.contains("plugin crashed"), "{}", err.message);
}

// ---- 端口 5：跨脚本调用 —— 宿主开放 call_script，脚本调另一个脚本的函数 ----
//
// so_demo 已有对应物 run_script(name)（整脚本执行）；这里是更精细的
// 「调目标脚本中的指定函数」。引擎零改造：宿主用 Rc<RefCell<Engine>>
// 共享同一引擎实例（嵌套调用期间全是不可变借用，RefCell 不冲突）。
// 宿主闭包必须自管三件事：递归检测（每次嵌套是全新 gas，引擎拦不住
// A→B→A 无限嵌套）、脚本缓存（免重复编译）、嵌套深度上限。

#[test]
fn cross_script_call_via_host_function() {
    use std::collections::HashMap;

    let engine = Rc::new(RefCell::new(Engine::new()));

    // 脚本 B：独立编译单元，提供一个 compute 函数
    let script_b = engine
        .borrow()
        .load_str("func compute(n) { n * 7 }")
        .unwrap();
    // 宿主的脚本缓存（对应 so_demo 的 AST_CACHE）
    let cache: Rc<RefCell<HashMap<String, brook::Script>>> =
        Rc::new(RefCell::new(HashMap::from([("b.bk".to_string(), script_b)])));
    // 递归检测的名字栈（对应 so_demo 的 SCRIPT_STACK）
    let stack: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // 在共享引擎上注册 call_script——闭包捕获引擎的 Rc
    // （会形成 Rc 环导致引擎延迟释放；长生命周期宿主可接受，测试进程退出即回收）
    let eng2 = Rc::clone(&engine);
    let cache2 = Rc::clone(&cache);
    let stack2 = Rc::clone(&stack);
    register_fn!(
        engine.borrow_mut(),
        "call_script",
        (String, String) -> Result<(bool, i64), Error>,
        move |script_name, func| {
            if stack2.borrow().iter().any(|s| s == &script_name) {
                return Err(Error::runtime(format!("跨脚本递归: {}", script_name)));
            }
            stack2.borrow_mut().push(script_name.clone());
            let inner = (|| {
                let eng = eng2.borrow();
                let script = cache2
                    .borrow()
                    .get(&script_name)
                    .cloned()
                    .ok_or_else(|| Error::runtime(format!("脚本不存在: {}", script_name)))?;
                let v = eng.call(&script, &func, vec![Value::I64(6)])?;
                match v {
                    Value::I64(n) => Ok((true, n)),
                    _ => Ok((true, 0)),
                }
            })();
            stack2.borrow_mut().pop();
            inner
        }
    )
    .unwrap();

    // 脚本 A 调用 B 的 compute —— 对脚本作者来说就是调一个宿主函数
    let script_a = engine
        .borrow()
        .load_str("func main() {\n    let r = call_script(\"b.bk\", \"compute\")\n    if r.0 { r.1 } else { -1 }\n}")
        .unwrap();
    assert_eq!(
        engine.borrow().call(&script_a, "main", vec![]).unwrap(),
        Value::I64(42)
    );

    // 递归拦截：目标脚本又调 call_script(自己) → 名字栈检测 → 明确报错
    let script_self = engine
        .borrow()
        .load_str("func self_call(n) { call_script(\"self.bk\", \"self_call\") }")
        .unwrap();
    cache
        .borrow_mut()
        .insert("self.bk".into(), script_self);
    let caller = engine
        .borrow()
        .load_str("func main() { call_script(\"self.bk\", \"self_call\") }")
        .unwrap();
    let err = engine.borrow().call(&caller, "main", vec![]).unwrap_err();
    assert!(err.message.contains("跨脚本递归"), "{}", err.message);

    // ⚠️ 陷阱实证：call_script 的返回形状固定为 (bool, i64)——目标函数
    // 返回字符串时被静默吞成 (true, 0)。要做对就得为每种签名注册
    // 变体（call_script_s / call_script_is / … 组合爆炸）。这就是为什么
    // call_script 适合任务编排（粗粒度、字符串进出），而函数库（细粒度、
    // 强类型、任意签名）应该用宿主拼接——拼接下签名编译期检查、类型自由。
    let script_str = engine.borrow().load_str("func name(n) { \"文本\" }").unwrap();
    cache.borrow_mut().insert("str.bk".into(), script_str);
    let caller = engine
        .borrow()
        .load_str("func main() { call_script(\"str.bk\", \"name\") }")
        .unwrap();
    let r = engine.borrow().call(&caller, "main", vec![]).unwrap();
    assert_eq!(
        r,
        Value::Tuple(vec![Value::Bool(true), Value::I64(0)]),
        "字符串返回被 (bool, i64) 封送静默吞成 0——无报错、无提示"
    );
}

// ---- N 元组桥接层直测（2–8 元组作为宿主返回值与实参）----

#[test]
fn host_tuples_of_arity_2_to_8_roundtrip() {
    let mut engine = Engine::new();
    register_fn!(engine, "t3", () -> (i64, String, bool), || (1, "a".into(), true)).unwrap();
    register_fn!(engine, "t5", () -> (i64, i64, i64, i64, i64), || (1, 2, 3, 4, 5)).unwrap();
    register_fn!(engine, "t8", () -> (i64, i64, i64, i64, i64, i64, i64, i64),
        || (1, 2, 3, 4, 5, 6, 7, 8)).unwrap();
    register_fn!(engine, "sum3", ((i64, i64, i64)) -> i64, |t| t.0 + t.1 + t.2).unwrap();

    // N 元组作为宿主返回值 + 脚本侧 .N 索引 + 元组既可作宿主参数（((i64,i64,i64))
    // 是一个元组类型的参数）也可用脚本元组字面量跨边界。
    let script = engine
        .load_str(
            "func go() {\n\
             let a = t3()\n\
             let s = sum3((t5().0, t5().1, t5().2))\n\
             let e = t8()\n\
             a.0 + s + e.7\n\
             }",
        )
        .unwrap();
    assert_eq!(
        engine.call(&script, "go", vec![]).unwrap(),
        Value::I64(1 + 1 + 2 + 3 + 8)
    );

    // 元组实参跨边界（FromValue 方向）
    let script = engine
        .load_str("func go(t) { t.0 + t.1 + t.2 }")
        .unwrap();
    let r = engine
        .call(
            &script,
            "go",
            vec![Value::Tuple(vec![
                Value::I64(1),
                Value::I64(2),
                Value::I64(3),
            ])],
        )
        .unwrap();
    assert_eq!(r, Value::I64(6));
    // 3 元组签名收到 2 元组：明确报错而非静默错值
    let r = engine.call(
        &script,
        "go",
        vec![Value::Tuple(vec![Value::I64(1), Value::I64(2)])],
    );
    assert!(r.is_err(), "arity-mismatched tuple must be rejected");

    // 类型不匹配的元素也明确报错
    let r = engine.call(
        &script,
        "go",
        vec![Value::Tuple(vec![
            Value::I64(1),
            Value::Str("x".into()),
            Value::I64(3),
        ])],
    );
    assert!(r.is_err(), "wrong element type must be rejected");
}
