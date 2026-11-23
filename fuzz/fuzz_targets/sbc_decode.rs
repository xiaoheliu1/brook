//! .sbc 缓存解码 fuzz：任意字节序列喂给解码器，永不 panic / 越界。
//! 对应 tests/fuzz_smoke.rs 的字节变异分支——「损坏的缓存返回 Err 而非崩溃」
//! 是核心安全保证，libFuzzer 会持续寻找能突破 wire validator 的输入。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let engine = brook::Engine::new();
    let _ = engine.script_from_bytes(data);
});
