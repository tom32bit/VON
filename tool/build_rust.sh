#!/usr/bin/env bash
#
# Cross-compiles the Rust core for Android and drops the .so files where Gradle
# expects them.
#
# Run this before `flutter build apk` / `flutter run`. CI calls the same script,
# so a local build and a release build produce identical artefacts.
#
#   ./tool/build_rust.sh              # release, all ABIs
#   ./tool/build_rust.sh --debug      # debug, all ABIs
#   ./tool/build_rust.sh --abi arm64  # one ABI (much faster while iterating)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
JNI_DIR="$ROOT/android/app/src/main/jniLibs"

PROFILE="release"
ABIS=("arm64-v8a" "armeabi-v7a" "x86_64")
FEATURE_ARGS=()
VARIANT="full"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug)   PROFILE="debug"; shift ;;
    --release) PROFILE="release"; shift ;;
    --cloud-only)
      # Drops llama.cpp, which is by far the slowest and most fragile thing to
      # cross-compile. Everything else works — the app just can't run a model
      # on-device, and the Model Hub hides that section (see build_features()).
      FEATURE_ARGS=(--no-default-features --features wasm-exec,python-exec)
      VARIANT="cloud-only"
      shift
      ;;
    --abi)
      case "$2" in
        arm64)  ABIS=("arm64-v8a") ;;
        arm)    ABIS=("armeabi-v7a") ;;
        x86_64) ABIS=("x86_64") ;;
        *) echo "unknown abi '$2' (expected arm64, arm, or x86_64)" >&2; exit 1 ;;
      esac
      shift 2
      ;;
    *) echo "unknown flag '$1'" >&2; exit 1 ;;
  esac
done

# ---- preflight ------------------------------------------------------------

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

if ! command -v cargo-ndk >/dev/null 2>&1; then
  echo "installing cargo-ndk…"
  cargo install cargo-ndk --locked
fi

if [[ -z "${ANDROID_NDK_HOME:-}" && -z "${ANDROID_NDK_ROOT:-}" ]]; then
  echo "error: set ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) to your NDK path." >&2
  echo "       Install it with: sdkmanager 'ndk;27.0.12077973'" >&2
  exit 1
fi

# rustup targets are per-ABI; adding one that is already present is a no-op.
declare -A TARGET_FOR=(
  ["arm64-v8a"]="aarch64-linux-android"
  ["armeabi-v7a"]="armv7-linux-androideabi"
  ["x86_64"]="x86_64-linux-android"
)
for abi in "${ABIS[@]}"; do
  rustup target add "${TARGET_FOR[$abi]}" >/dev/null 2>&1 || true
done

# ---- build ----------------------------------------------------------------

mkdir -p "$JNI_DIR"

NDK_ARGS=()
for abi in "${ABIS[@]}"; do
  NDK_ARGS+=("-t" "$abi")
done

echo "building von core ($PROFILE, $VARIANT) for: ${ABIS[*]}"
cd "$RUST_DIR"

# Stale .so files from a previous variant would otherwise be packaged alongside
# the new ones, and Gradle would happily ship whichever it found.
rm -f "$JNI_DIR"/*/libvon.so

PROFILE_ARGS=()
if [[ "$PROFILE" == "release" ]]; then
  PROFILE_ARGS=(--release)
fi

# `--platform 26` must match minSdk in android/app/build.gradle. A lower value
# links against symbols that do not exist on the devices we ship to; a higher
# one silently raises the effective minimum.
cargo ndk "${NDK_ARGS[@]}" --platform 26 -o "$JNI_DIR" build \
  "${PROFILE_ARGS[@]}" "${FEATURE_ARGS[@]}"

# Record the variant so the packaging step can name the APK accurately.
echo "$VARIANT" > "$JNI_DIR/.variant"

echo
echo "native libraries:"
find "$JNI_DIR" -name "*.so" -exec ls -lh {} \; | awk '{print "  " $9 " (" $5 ")"}'
echo
echo "done. Now run: flutter run   (or flutter build apk --release)"
