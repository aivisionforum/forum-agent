#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/dora_binary.sh"
FORUM_AGENT_DORA_BIN="$(resolve_native_dora)"
export FORUM_AGENT_DORA_BIN
MODE="${1:-dev}"
BUILD_TARGET_DIR="${FORUM_AGENT_CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/forum-shell-cargo-target}"

if [[ "$BUILD_TARGET_DIR" != /* ]]; then
  echo "FORUM_AGENT_CARGO_TARGET_DIR must be an absolute path: $BUILD_TARGET_DIR" >&2
  exit 1
fi

if [[ "$BUILD_TARGET_DIR" == *" "* ]]; then
  echo "AI Vision Forum's Cargo target directory cannot contain spaces: $BUILD_TARGET_DIR"
  echo "Set FORUM_AGENT_CARGO_TARGET_DIR to a path without spaces."
  exit 1
fi

mkdir -p "$BUILD_TARGET_DIR"
export CARGO_TARGET_DIR="$BUILD_TARGET_DIR"
export FORUM_AGENT_DORA_TARGET_DIR="$BUILD_TARGET_DIR"

case "$MODE" in
  dev)
    echo "Building local translation nodes in $BUILD_TARGET_DIR..."
    cargo build --locked \
      --manifest-path "$ROOT_DIR/Cargo.toml" \
      -p dora-qwen3-asr \
      -p dora-qwen35-translator \
      -p hen-local-init
    cd "$ROOT_DIR/forum-shell"
    exec ./ui/node_modules/.bin/tauri dev
    ;;
  build)
    exec bash "$ROOT_DIR/scripts/build_macos_app.sh"
    ;;
  *)
    echo "Usage: $(basename "$0") [dev|build]"
    exit 1
    ;;
esac
