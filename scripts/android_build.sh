#!/usr/bin/env bash
# Build Android artifacts for vitul, then smoke-test on a device if one is
# attached. Works from Git Bash on Windows, Linux and macOS.
#
#   ./scripts/android_build.sh [output-dir]     # default: jniLibs
#
# Prerequisites: rustup targets (aarch64-linux-android, armv7-linux-androideabi),
# Android NDK (ANDROID_NDK_HOME or ANDROID_HOME), cargo-ndk, and adb for the
# optional device run.
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-jniLibs}"
API="${ANDROID_API:-24}"

if ! command -v cargo-ndk >/dev/null 2>&1; then
  echo "cargo-ndk not found. Install with: cargo install cargo-ndk --locked" >&2
  exit 1
fi
if [[ -z "${ANDROID_NDK_HOME:-}" && -z "${ANDROID_HOME:-}" ]]; then
  echo "Set ANDROID_NDK_HOME (or ANDROID_HOME) to your Android NDK/SDK." >&2
  exit 1
fi

echo "==> Building JNI shared libraries (arm64-v8a, armeabi-v7a, API $API)"
cargo ndk -t arm64-v8a -t armeabi-v7a -P "$API" -o "$OUT" build --release --lib

echo "==> Building native smoke-test binaries"
for ABI in arm64-v8a armeabi-v7a; do
  cargo ndk -t "$ABI" -P "$API" build --release --example android_cli
done

mkdir -p "$OUT/cli"
cp target/aarch64-linux-android/release/examples/android_cli "$OUT/cli/android_cli-arm64-v8a"
cp target/armv7-linux-androideabi/release/examples/android_cli "$OUT/cli/android_cli-armeabi-v7a"
echo "==> Artifacts: $OUT/{arm64-v8a,armeabi-v7a}/libvitul.so, $OUT/cli/*"

if ! command -v adb >/dev/null 2>&1; then
  echo "==> adb not found: skipping device smoke test."
  exit 0
fi
DEVICE="$(adb devices | awk 'NR > 1 && $2 == "device" { print $1; exit }')"
if [[ -z "$DEVICE" ]]; then
  echo "==> No device/emulator attached: skipping device smoke test."
  echo "    Connect one and re-run, or run manually:"
  echo "    adb push $OUT/cli/android_cli-arm64-v8a /data/local/tmp/android_cli"
  echo "    adb shell chmod 755 /data/local/tmp/android_cli && adb shell /data/local/tmp/android_cli"
  exit 0
fi

ARCH="$(adb -s "$DEVICE" shell getprop ro.product.cpu.abi | tr -d '\r')"
case "$ARCH" in
  arm64-v8a) BIN="$OUT/cli/android_cli-arm64-v8a" ;;
  *)         BIN="$OUT/cli/android_cli-armeabi-v7a" ;;
esac
echo "==> Running smoke test on $DEVICE ($ARCH)"
# Git Bash on Windows rewrites `/data/...` into a Windows path before adb sees
# it, which makes the push fail with "remote secure_mkdirs() failed". The
# variable is a Git-for-Windows no-op on Linux/macOS, so prefixing unconditionally
# keeps the script portable.
MSYS_NO_PATHCONV=1 adb -s "$DEVICE" push "$BIN" /data/local/tmp/android_cli >/dev/null
adb -s "$DEVICE" shell chmod 755 /data/local/tmp/android_cli
adb -s "$DEVICE" shell /data/local/tmp/android_cli
