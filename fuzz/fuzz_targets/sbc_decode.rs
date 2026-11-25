//! .sbc 缓存解码与执行 fuzz：任意字节序列喂给解码器，合法产物继续进入 VM。
//! 「损坏的缓存返回 Err、合法字节码不 panic / 越界」是核心安全保证。

#![no_main]

#[path = "support.rs"]
mod support;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let engine = brook::Engine::new()
        .with_limits(10_000, 64)
        .with_heap_limit(64 * 1024);
    if let Ok(script) = engine.script_from_bytes(data) {
        support::exercise(&engine, &script);
    }
});
