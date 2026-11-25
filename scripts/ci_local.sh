#!/usr/bin/env bash
# 本机 CI（与 .github/workflows/ci.yml 对齐的可跑版本）。
#   bash scripts/ci_local.sh          # 全跑
#   SKIP_CROSS=1 bash scripts/ci_local.sh   # 跳过交叉编译（没装 NDK/target 时）
#
# 设计：每步独立执行、失败继续，最后出汇总表——与 android_so_demo 的
# ci_local.ps1 同口径，一次看到全貌而不是第一处失败就中断。
set -uo pipefail
cd "$(dirname "$0")/.."

results=()
run() {
  local name="$1"; shift
  echo; echo ">>> $name"
  local t0=$(date +%s)
  if "$@"; then
    results+=("PASS|$name|$(( $(date +%s) - t0 ))")
  else
    results+=("FAIL|$name|$(( $(date +%s) - t0 ))")
  fi
}

run "cargo fmt --check"   cargo fmt --all -- --check
run "cargo clippy"        cargo clippy --locked --all-targets -- -D warnings
run "cargo test（全量）"  cargo test --locked --all-targets
run "doctest"             cargo test --locked --doc
run "strict rustdoc"      env RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
run "release 构建"        cargo build --locked --release --lib

if [[ "${SKIP_CROSS:-0}" != "1" ]]; then
  if command -v cargo-ndk >/dev/null 2>&1 && [[ -n "${ANDROID_NDK_HOME:-${ANDROID_HOME:-}}" ]]; then
    run "Android 双 ABI 交叉编译" bash -c '
      cargo ndk -t arm64-v8a -t armeabi-v7a -P 24 -o /tmp/brook_jni build --release --lib &&
      for abi in arm64-v8a armeabi-v7a; do
        test -f "/tmp/brook_jni/$abi/libbrook.so" || exit 1
        readelf -h "/tmp/brook_jni/$abi/libbrook.so" | grep -q Machine: || exit 1
      done'
  else
    echo ">>> Android 双 ABI 交叉编译：SKIP（需 cargo-ndk + ANDROID_NDK_HOME）"
    results+=("SKIP|Android 双 ABI|$(( 0 ))")
  fi
else
  echo ">>> Android 双 ABI 交叉编译：SKIP（SKIP_CROSS=1）"
  results+=("SKIP|Android 双 ABI|0")
fi

echo; echo "================ 本机 CI 汇总 ================"
fails=0
for r in "${results[@]}"; do
  IFS='|' read -r mark name sec <<< "$r"
  printf "  [%s] %-28s %ss\n" "$mark" "$name" "$sec"
  [[ "$mark" == "FAIL" ]] && fails=$((fails+1))
done
if [[ $fails -gt 0 ]]; then
  echo "结论：失败 $fails 项"; exit 1
fi
echo "结论：全部通过（${#results[@]} 项）"
