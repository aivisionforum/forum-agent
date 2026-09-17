#!/usr/bin/env bash
# Read-only development checks for the imported translation shell.
# Python meeting-worker packaging and model checksum acceptance are separate F01 gates.
set -euo pipefail

MODE="${1:-}"
APP_RESOURCES="${FORUM_AGENT_APP_RESOURCES:-}"
APP_BIN_PATH=""
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/dora_binary.sh"

FORUM_MODEL_ROOT="$HOME/Library/Application Support/AI Vision Forum/models"
QWEN_ASR_MODEL_DIR="${QWEN3_ASR_MODEL_PATH:-$FORUM_MODEL_ROOT/qwen3-asr-1.7b}"
QWEN35_TRANSLATOR_MODEL_DIR="${QWEN35_TRANSLATOR_MODEL_PATH:-$FORUM_MODEL_ROOT/Qwen3.5-2B-MLX-4bit}"
QWEN_ASR_REPO="${QWEN3_ASR_REPO:-mlx-community/Qwen3-ASR-1.7B-8bit}"
QWEN35_TRANSLATOR_REPO="${QWEN35_TRANSLATOR_REPO:-mlx-community/Qwen3.5-2B-MLX-4bit}"

if [[ -z "$APP_RESOURCES" ]]; then
  APP_RESOURCES="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

TRANSLATION_DATAFLOW_PATH=""
if [[ -f "$APP_RESOURCES/dataflow/translation_qwen35.yml" ]]; then
  TRANSLATION_DATAFLOW_PATH="$APP_RESOURCES/dataflow/translation_qwen35.yml"
elif [[ -f "$APP_RESOURCES/forum-shell/dataflow/translation_qwen35.yml" ]]; then
  TRANSLATION_DATAFLOW_PATH="$APP_RESOURCES/forum-shell/dataflow/translation_qwen35.yml"
fi

BUILD_TARGET_DIR="${FORUM_AGENT_DORA_TARGET_DIR:-${CARGO_TARGET_DIR:-$APP_RESOURCES/target}}"
resolve_runtime_binary() {
  local name="$1" candidate
  for candidate in "$APP_RESOURCES/../MacOS/$name" \
      "$BUILD_TARGET_DIR/debug/$name" "$BUILD_TARGET_DIR/release/$name" \
      "$BUILD_TARGET_DIR/$name"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}
APP_BIN_PATH="$(resolve_runtime_binary forum-shell || true)"

errors=()
warnings=()

check_file() {
  local path="$1"
  local label="$2"
  if [[ ! -f "$path" ]]; then
    errors+=("$label missing: $path")
  fi
}

qwen35_translation_model_ready() {
  local model_dir="$1"
  [[ -f "$model_dir/config.json" ]] &&
  [[ -f "$model_dir/tokenizer.json" ]] &&
  [[ -f "$model_dir/tokenizer_config.json" ]] &&
  ([[ -f "$model_dir/model.safetensors" ]] || [[ -f "$model_dir/model.safetensors.index.json" ]])
}

asr_model_ready() {
  local model_dir="$1"
  [[ -f "$model_dir/config.json" ]] &&
  [[ -f "$model_dir/tokenizer.json" ]] &&
  ([[ -f "$model_dir/model.safetensors" ]] || [[ -f "$model_dir/model.safetensors.index.json" ]])
}

if ! FORUM_AGENT_DORA_BIN="$(FORUM_AGENT_APP_RESOURCES="$APP_RESOURCES" resolve_native_dora)"; then
  errors+=("Native arm64 Dora CLI 0.4.1 is missing (Python/Conda wrappers are not supported)")
fi
check_file "$APP_BIN_PATH" "App runtime binary"
check_file "$TRANSLATION_DATAFLOW_PATH" "Translation dataflow file"
for name in dora-qwen3-asr dora-qwen35-translator hen-local-init; do
  if ! resolve_runtime_binary "$name" >/dev/null; then
    errors+=("Required runtime binary is missing: $name")
  fi
done

if [[ -n "$TRANSLATION_DATAFLOW_PATH" && -f "$TRANSLATION_DATAFLOW_PATH" ]]; then
  if grep -q 'TRANSLATION_MERGE_ENABLED' "$TRANSLATION_DATAFLOW_PATH"; then
    errors+=("translation_qwen35.yml still references removed TRANSLATION_MERGE_ENABLED placeholder: $TRANSLATION_DATAFLOW_PATH")
  fi
  if ! grep -q 'path: __ASR_BIN_PATH__' "$TRANSLATION_DATAFLOW_PATH"; then
    errors+=("translation_qwen35.yml missing __ASR_BIN_PATH__ placeholder: $TRANSLATION_DATAFLOW_PATH")
  fi
  if ! grep -q 'path: __TRANSLATOR_BIN_PATH__' "$TRANSLATION_DATAFLOW_PATH"; then
    errors+=("translation_qwen35.yml missing __TRANSLATOR_BIN_PATH__ placeholder: $TRANSLATION_DATAFLOW_PATH")
  fi
  if ! grep -q 'question_ended: moxin-mic-input/question_ended' "$TRANSLATION_DATAFLOW_PATH"; then
    warnings+=("translation_qwen35.yml no longer wires question_ended into ASR: $TRANSLATION_DATAFLOW_PATH")
  fi
fi

# Check Qwen3 ASR model (required)
if [[ -z "${QWEN3_ASR_MODEL_PATH:-}" ]] && ! asr_model_ready "$QWEN_ASR_MODEL_DIR" && \
    asr_model_ready "$HOME/.OminiX/models/qwen3-asr-1.7b"; then
  QWEN_ASR_MODEL_DIR="$HOME/.OminiX/models/qwen3-asr-1.7b"
fi
if ! asr_model_ready "$QWEN_ASR_MODEL_DIR"; then
  errors+=("Qwen3-ASR model not found: $QWEN_ASR_MODEL_DIR — run hen-local-init or launch the app")
fi

# Check Qwen3.5 translator model (required)
if [[ -z "${QWEN35_TRANSLATOR_MODEL_PATH:-}" ]] && ! qwen35_translation_model_ready "$QWEN35_TRANSLATOR_MODEL_DIR" && \
    qwen35_translation_model_ready "$HOME/.OminiX/models/Qwen3.5-2B-MLX-4bit"; then
  QWEN35_TRANSLATOR_MODEL_DIR="$HOME/.OminiX/models/Qwen3.5-2B-MLX-4bit"
fi
if ! qwen35_translation_model_ready "$QWEN35_TRANSLATOR_MODEL_DIR"; then
  errors+=("Qwen3.5 translator model incomplete: $QWEN35_TRANSLATOR_MODEL_DIR — run hen-local-init or launch the app")
fi

if [[ "$MODE" != "--quick" ]]; then
  echo "=== AI Vision Forum Preflight ==="
  echo "Resources:  $APP_RESOURCES"
  echo "Dataflow:   $TRANSLATION_DATAFLOW_PATH"
  echo "ASR model:  $QWEN_ASR_MODEL_DIR"
  echo "Qwen3.5 translator model: $QWEN35_TRANSLATOR_MODEL_DIR"
  echo ""
fi

if ((${#warnings[@]} > 0)) && [[ "$MODE" != "--quiet" ]]; then
  echo "Warnings:"
  for w in "${warnings[@]}"; do
    echo "  - $w"
  done
  echo ""
fi

if ((${#errors[@]} > 0)); then
  if [[ "$MODE" != "--quiet" ]]; then
    echo "Preflight failed:"
    for e in "${errors[@]}"; do
      echo "  - $e"
    done
  fi
  exit 1
fi

if [[ "$MODE" != "--quiet" ]]; then
  echo "Translation shell preflight passed (file presence only; not model checksum or Forum product acceptance)."
fi
