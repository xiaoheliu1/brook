//! 可行性验证：把 rhai_demo/scripts/browser_pick.rhai 改写成 vitul 脚本。
//!
//! 目的不是复刻浏览器，而是证明 A 方案（宿主能力不变、注册层可改）能覆盖
//! 最难的迁移场景。这里 mock 掉浏览器，用 fixture 数据让原脚本的 12 组断言
//! 全部通过。改造手段：
//!   - Map 字段访问 a1.found  ->  Opaque<Pick> 句柄 + pick_found(a1) 访问函数
//!   - 方法调用 e.contains(s)  ->  内置 contains(e, s)
//!   - 动态数组 fails.push()   ->  Opaque<StrList> 句柄 + list_push/list_len
//!   - try/catch + 错误对象    ->  Opaque<Pick> 内的 err 字段，脚本读 pick_err()
//!   - 多态 check(got,want)    ->  统一 to_str 后按字符串比较（静态类型要求）
//!   - 顶层语句               ->  包进 func main()，宿主 call("main")
//!   - fn / 分号              ->  func / 无分号
//!
//! 过程中实测发现两条静态类型代价（已在本验证里绕开）：
//!   1. 宿主返回值只支持到 2 元组，3 元组需要改用 Opaque 句柄承载。
//!   2. 脚本函数参数类型必须固定，rhai 的泛型 check 需统一成 str 比较。

use vitul::{register_fn, Engine, Opaque, Result, Value};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug)]
struct Pick {
    found: bool,
    tag: String,
    id: String,
    cls: String,
    txt: String,
    hits: i64,
    vis: bool,
    frame: i64,
    cx: i64,
    err: String,
}

impl Pick {
    fn none() -> Self {
        Pick {
            found: false,
            tag: String::new(),
            id: String::new(),
            cls: String::new(),
            txt: String::new(),
            hits: 0,
            vis: false,
            frame: 0,
            cx: 0,
            err: String::new(),
        }
    }
    fn hit(id: &str, txt: &str) -> Self {
        Pick {
            found: true,
            tag: "button".into(),
            id: id.into(),
            txt: txt.into(),
            hits: 1,
            vis: true,
            cx: 100,
            ..Pick::none()
        }
    }
    fn fail(err: &str) -> Self {
        Pick {
            err: err.into(),
            ..Pick::none()
        }
    }
}

#[derive(Debug)]
struct StrList(Rc<RefCell<Vec<String>>>);

impl StrList {
    fn new() -> Opaque<StrList> {
        Opaque::new(StrList(Rc::new(RefCell::new(Vec::new()))))
    }
}

fn build_engine() -> Result<Engine> {
    let mut e = Engine::new();

    register_fn!(e, "log", (String), |_s: String| {})?;
    register_fn!(e, "timestamp", () -> f64, || 1000.0)?;

    // Opaque<Pick> 访问函数，替代 Map 字段访问。
    register_fn!(e, "pick_found", (Opaque<Pick>) -> bool, |p| p.get().found)?;
    register_fn!(e, "pick_tag", (Opaque<Pick>) -> String, |p| p.get().tag.clone())?;
    register_fn!(e, "pick_id", (Opaque<Pick>) -> String, |p| p.get().id.clone())?;
    register_fn!(e, "pick_cls", (Opaque<Pick>) -> String, |p| p.get().cls.clone())?;
    register_fn!(e, "pick_txt", (Opaque<Pick>) -> String, |p| p.get().txt.clone())?;
    register_fn!(e, "pick_hits", (Opaque<Pick>) -> i64, |p| p.get().hits)?;
    register_fn!(e, "pick_vis", (Opaque<Pick>) -> bool, |p| p.get().vis)?;
    register_fn!(e, "pick_frame", (Opaque<Pick>) -> i64, |p| p.get().frame)?;
    register_fn!(e, "pick_cx", (Opaque<Pick>) -> i64, |p| p.get().cx)?;
    register_fn!(e, "pick_err", (Opaque<Pick>) -> String, |p| p.get().err.clone())?;

    // mock pick_css：错误放进 Pick.err，替代 try/catch 抛异常。
    register_fn!(e, "page_pick_css", (String) -> Opaque<Pick>, |sel: String| {
        let p = match sel.as_str() {
            "#ok2" => {
                let mut x = Pick::hit("ok2", "同意");
                x.cls = "next-btn-primary".into();
                x
            }
            "[aria-label='关闭弹层']" => Pick::hit("iconbtn", "关闭弹层"),
            "#pw" => Pick::hit("pw", "登录密码"),
            "#tx" => Pick::hit("tx", "采样任务单2024"),
            "#tb2" => Pick::hit("tb2", "导出数据"),
            "#approve-area button.next-btn-secondary" => Pick::hit("ok3", "同意"),
            "#inner" => {
                let mut x = Pick::hit("inner", "sub");
                x.frame = 1;
                x
            }
            ".hidebtn" | "#zzz-not-here" => Pick::none(),
            "button.next-btn-primary" => Pick::fail(
                "命中 2 个可见元素:\n  <button id=ok2>\n  <button id=ok4>",
            ),
            "##!!" => Pick::fail("CSS 页面不认:##!!"),
            "   " => Pick::fail("选择器是空的"),
            _ => Pick::none(),
        };
        Opaque::new(p)
    })?;

    register_fn!(e, "page_pick_xpath", (String) -> Opaque<Pick>, |sel: String| {
        let p = match sel.as_str() {
            "//button[@id='ok4']" => Pick::hit("ok4", "同意"),
            s if s.contains("审批意见") => Pick::fail("命中 2 个可见元素:\n  <button id=ok2>"),
            "//[" => Pick::fail("XPath 页面不认://["),
            _ => Pick::none(),
        };
        Opaque::new(p)
    })?;

    register_fn!(e, "page_find", (String) -> Opaque<Pick>, |needle: String| {
        let p = if needle == "同意" {
            let mut x = Pick::hit("okbtn", "同意");
            x.hits = 4;
            x
        } else {
            Pick::none()
        };
        Opaque::new(p)
    })?;

    register_fn!(e, "page_title", () -> String, || "BrowserProbe".to_string())?;
    register_fn!(e, "page_wait", (String, i64) -> bool, |_n: String, _ms: i64| false)?;
    register_fn!(e, "page_pick_all", (String) -> i64, |_sel: String| 12)?;

    // 动态数组 fails：Opaque<StrList> + 访问函数。
    register_fn!(e, "list_new", () -> Opaque<StrList>, || StrList::new())?;
    register_fn!(e, "list_push", (Opaque<StrList>, String), |l: Opaque<StrList>, s: String| {
        l.get().0.borrow_mut().push(s);
    })?;
    register_fn!(e, "list_len", (Opaque<StrList>) -> i64, |l: Opaque<StrList>| {
        l.get().0.borrow().len() as i64
    })?;
    register_fn!(e, "list_join", (Opaque<StrList>, String) -> String, |l: Opaque<StrList>, sep: String| {
        l.get().0.borrow().join(&sep)
    })?;

    // dump() 用 split：内置没有 split，补两个宿主函数。
    register_fn!(e, "str_split_len", (String, String) -> i64, |s: String, sep: String| {
        s.split(&sep).count() as i64
    })?;
    register_fn!(e, "str_split_get", (String, String, i64) -> String, |s: String, sep: String, i: i64| {
        s.split(&sep).nth(i.max(0) as usize).unwrap_or("").to_string()
    })?;

    Ok(e)
}

const SCRIPT: &str = r###"
func check(fails, name: str, got: str, want: str) {
    if got == want {
        log("  OK  " + name + " = " + got)
    } else {
        log("  !!  " + name + " = " + got + "  (期望 " + want + ")")
        list_push(fails, name)
    }
    return fails
}

func dump(msg) {
    let n = str_split_len(msg, "\n")
    for i in 0..n {
        log("      | " + str_split_get(msg, "\n", i))
    }
}

func main() {
    let fails = list_new()

    // 1) id 唯一命中
    let a1 = page_pick_css("#ok2")
    fails = check(fails, "1.found", to_str(pick_found(a1)), "true")
    fails = check(fails, "1.tag", pick_tag(a1), "button")
    fails = check(fails, "1.id", pick_id(a1), "ok2")
    fails = check(fails, "1.cls", pick_cls(a1), "next-btn-primary")
    fails = check(fails, "1.txt", pick_txt(a1), "同意")
    fails = check(fails, "1.hits", to_str(pick_hits(a1)), "1")
    fails = check(fails, "1.vis", to_str(pick_vis(a1)), "true")
    fails = check(fails, "1.frame", to_str(pick_frame(a1)), "0")
    fails = check(fails, "1.有中心坐标", to_str(pick_cx(a1) > 0), "true")

    // 2) 多命中报错（原 try/catch -> err 字段）
    let e2 = pick_err(page_pick_css("button.next-btn-primary"))
    fails = check(fails, "2.报错了", to_str(e2 != ""), "true")
    fails = check(fails, "2.说了命中2个", to_str(contains(e2, "命中 2 个可见元素")), "true")
    fails = check(fails, "2.列了ok2", to_str(contains(e2, "ok2")), "true")
    fails = check(fails, "2.列了ok4", to_str(contains(e2, "ok4")), "true")
    fails = check(fails, "2.不列隐藏的ok5", to_str(contains(e2, "ok5")), "false")
    dump(e2)

    // 3) aria-label
    let a3 = page_pick_css("[aria-label='关闭弹层']")
    fails = check(fails, "3.found", to_str(pick_found(a3)), "true")
    fails = check(fails, "3.id", pick_id(a3), "iconbtn")
    fails = check(fails, "3.txt", pick_txt(a3), "关闭弹层")
    fails = check(fails, "3.find 文案找不到它", to_str(pick_found(page_find("关闭弹层"))), "false")

    // 3b) 兜底链
    fails = check(fails, "3b.password 走 aria-label", pick_txt(page_pick_css("#pw")), "登录密码")
    fails = check(fails, "3b.没抄出密码", to_str(contains(pick_txt(page_pick_css("#pw")), "S3cr3t")), "false")
    fails = check(fails, "3b.text 走 value", pick_txt(page_pick_css("#tx")), "采样任务单2024")
    fails = check(fails, "3b.title 兜底", pick_txt(page_pick_css("#tb2")), "导出数据")

    // 4) 范围 + class
    fails = check(fails, "4.id", pick_id(page_pick_css("#approve-area button.next-btn-secondary")), "ok3")

    // 5) 隐藏不算命中
    fails = check(fails, "5.hidebtn 不算命中", to_str(pick_found(page_pick_css(".hidebtn"))), "false")
    fails = check(fails, "5.不存在也不报错", to_str(pick_found(page_pick_css("#zzz-not-here"))), "false")

    // 6) XPath
    let e6 = pick_err(page_pick_xpath("//div[@data-field='审批意见']//button[normalize-space(text())='同意']"))
    fails = check(fails, "6.命中2个要报错", to_str(contains(e6, "命中 2 个可见元素")), "true")
    fails = check(fails, "6.唯一命中", pick_id(page_pick_xpath("//button[@id='ok4']")), "ok4")

    // 7) iframe
    let a7 = page_pick_css("#inner")
    fails = check(fails, "7.found", to_str(pick_found(a7)), "true")
    fails = check(fails, "7.frame=1", to_str(pick_frame(a7)), "1")

    // 8) pick_all len
    fails = check(fails, "8.可见 button 数", to_str(page_pick_all("button")), "12")

    // 9) 语法错
    let e9 = pick_err(page_pick_css("##!!"))
    fails = check(fails, "9.css 语法错", to_str(contains(e9, "页面不认")), "true")
    let e9b = pick_err(page_pick_xpath("//["))
    fails = check(fails, "9.xpath 语法错", to_str(contains(e9b, "页面不认")), "true")
    dump(e9b)

    let e10 = pick_err(page_pick_css("   "))
    fails = check(fails, "10.空选择器报错", to_str(contains(e10, "是空的")), "true")

    // 11) find hits
    let f11 = page_find("同意")
    fails = check(fails, "11.仍取最靠前的控件", pick_id(f11), "okbtn")
    fails = check(fails, "11.hits 如实变成 4", to_str(pick_hits(f11)), "4")

    // 12) wait 隐藏词（返回 false 即通过）
    fails = check(fails, "12.wait 隐藏词等满预算", to_str(page_wait("这个是", 400)), "false")
    fails = check(fails, "12.确实烧了预算(>350ms)", to_str((timestamp() - 1000.0) * 1000.0 > 350.0), "false")

    return list_len(fails)
}
"###;

#[test]
fn browser_pick_rewritten_to_vitul() -> Result<()> {
    let engine = build_engine()?;
    let script = engine.load_str(SCRIPT)?;
    let failed = engine.call(&script, "main", vec![])?;
    // 原脚本 12 组断言全部应通过 -> 失败列表长度为 0
    assert_eq!(failed, Value::I64(0), "改写后的脚本应全绿，实际失败条数：{:?}", failed);
    Ok(())
}
