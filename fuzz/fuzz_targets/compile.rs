//! 编译器前端与执行 fuzz：任意 UTF-8 源码喂给前端，合法产物继续进入 VM。
//! 对应 tests/fuzz_smoke.rs 的随机 token 源码分支，但由 libFuzzer 做
//! 覆盖率引导的变异（比固定种子的 5000 轮冒烟更强）。

#![no_main]

#[path = "support.rs"]
mod support;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Prefix 0/1 turns the remaining UTF-8 bytes into a function body, giving
    // token-level mutations a shorter path to executable programs.
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let src = match data.first().copied() {
        Some(0) => format!("func f() {{ {} }}", &source[1..]),
        Some(1) => format!("func f(n) {{ {} }}", &source[1..]),
        _ => source.to_string(),
    };
    let engine = brook::Engine::new()
        .with_limits(10_000, 64)
        .with_heap_limit(64 * 1024);
    if let Ok(script) = engine.load_str(&src) {
        support::exercise(&engine, &script);
    }
});
