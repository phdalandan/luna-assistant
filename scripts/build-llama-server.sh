#!/usr/bin/env bash
# Builds the pinned llama.cpp server as a self-contained Tauri sidecar.
# Usage: scripts/build-llama-server.sh [target-triple]
set -euo pipefail

LLAMA_CPP_TAG="b11517"
LLAMA_CPP_COMMIT="8a1a9b5126126e5228b95fa909d4b08fac65e8b3"

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
work="${LLAMA_BUILD_DIR:-$root/src-tauri/target/llama.cpp}"
source="$work/src-$LLAMA_CPP_TAG"
build="$work/build-$LLAMA_CPP_TAG-$target"

if [ ! -d "$source/.git" ]; then
  rm -rf "$source"
  git clone --quiet --depth 1 --branch "$LLAMA_CPP_TAG" https://github.com/ggml-org/llama.cpp "$source"
fi
actual="$(git -C "$source" rev-parse HEAD)"
if [ "$actual" != "$LLAMA_CPP_COMMIT" ]; then
  echo "llama.cpp $LLAMA_CPP_TAG resolved to $actual, expected $LLAMA_CPP_COMMIT" >&2
  exit 1
fi

options=(
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=OFF
  -DLLAMA_BUILD_TESTS=OFF
  -DLLAMA_BUILD_EXAMPLES=OFF
  -DLLAMA_BUILD_APP=OFF
  -DLLAMA_BUILD_UI=OFF
  -DLLAMA_USE_PREBUILT_UI=OFF
  -DLLAMA_OPENSSL=OFF
  -DGGML_OPENMP=OFF
  -DGGML_NATIVE=OFF
)
case "$target" in
  aarch64-apple-darwin)
    options+=(-DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_OSX_DEPLOYMENT_TARGET=11.0)
    ;;
  x86_64-pc-windows-msvc)
    # CPU backend for AVX2 processors, with the C runtime linked statically.
    options+=(-DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded)
    ;;
  x86_64-unknown-linux-gnu)
    options+=(-DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON)
    ;;
  *)
    echo "Unsupported target: $target" >&2
    exit 1
    ;;
esac

cmake -S "$source" -B "$build" "${options[@]}"
cmake --build "$build" --config Release --target llama-server --parallel

extension=""
[[ "$target" == *windows* ]] && extension=".exe"
binary="$(find "$build/bin" -name "llama-server$extension" -type f | head -n 1)"
mkdir -p "$root/src-tauri/binaries"
cp "$binary" "$root/src-tauri/binaries/llama-server-$target$extension"
echo "Built llama-server $LLAMA_CPP_TAG for $target"
