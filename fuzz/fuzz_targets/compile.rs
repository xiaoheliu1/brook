//! 编译器前端 fuzz：任意字节序列当源码喂给前端，永不 panic。
//! 对应 tests/fuzz_smoke.rs 的随机 token 源码分支，但由 libFuzzer 做
//! 覆盖率引导的变异（比固定种子的 5000 轮冒烟更强）。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 前端只接受 UTF-8 源码；非 UTF-8 输入直接跳过（不是引擎的职责）。
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let engine = brook::Engine::new();
    let _ = engine.load_str(src);
});
