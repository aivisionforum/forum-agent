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
    # Source checkouts do not contain the packaged Python sidecars. Reuse the
    # repository environment and adapters; never fetch model weights at startup.
    REPO_DIR="$(cd "$ROOT_DIR/.." && pwd)"
    DEV_PYTHON="${FORUM_AGENT_DEV_PYTHON:-$REPO_DIR/.venv/bin/python}"
    : "${FORUM_ASR_PYTHON:=$DEV_PYTHON}"
    : "${FORUM_TRANSLATOR_PYTHON:=$DEV_PYTHON}"
    : "${FORUM_MEETING_PYTHON:=$DEV_PYTHON}"
    : "${FORUM_ASR_SCRIPT:=$REPO_DIR/services/asr-worker/asr_worker.py}"
    : "${FORUM_TRANSLATOR_SCRIPT:=$REPO_DIR/services/translation-worker/translation_worker.py}"
    : "${FORUM_MEETING_WORKER_SOURCE:=$REPO_DIR/services/meeting-worker/src}"
    export FORUM_ASR_PYTHON FORUM_ASR_SCRIPT FORUM_TRANSLATOR_PYTHON FORUM_TRANSLATOR_SCRIPT
    export FORUM_MEETING_PYTHON FORUM_MEETING_WORKER_SOURCE
    for runtime in "$FORUM_ASR_PYTHON" "$FORUM_TRANSLATOR_PYTHON" "$FORUM_MEETING_PYTHON"; do
      if [[ "$runtime" != /* || ! -x "$runtime" ]]; then
        echo "Development Python is missing: $runtime. Prepare the repository .venv or set FORUM_AGENT_DEV_PYTHON." >&2
        exit 1
      fi
    done
    "$FORUM_ASR_PYTHON" -I -c 'import mlx_qwen3_asr' || {
      echo "ASR runtime is incomplete; install dependencies from services/asr-worker/runtime-macos-arm64.lock.json. Model downloads will not fix this." >&2
      exit 1
    }
    "$FORUM_TRANSLATOR_PYTHON" -I -c 'import mlx_lm' || exit 1
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
