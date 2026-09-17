#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script only supports macOS."
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/dora_binary.sh"
FORUM_AGENT_DORA_BIN="$(resolve_native_dora)"
export FORUM_AGENT_DORA_BIN
WORKSPACE_VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' "$ROOT_DIR/Cargo.toml" | head -n 1)"
if [[ -z "$WORKSPACE_VERSION" ]]; then
  echo "Failed to read workspace package version from $ROOT_DIR/Cargo.toml"
  exit 1
fi

APP_NAME="AI Vision Forum"
BUNDLE_ID="org.aivisionforum.agent"
PROFILE="release"
OUT_DIR="$ROOT_DIR/dist"
VERSION="$WORKSPACE_VERSION"
MARKETING_VERSION="${VERSION%%[-+]*}"
BUILD_VERSION="${FORUM_AGENT_BUILD_VERSION:-$MARKETING_VERSION}"
BUILD_TARGET_DIR="${FORUM_AGENT_CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/forum-shell-cargo-target}"
TAURI_PRODUCT_NAME="AI Vision Forum"
SKIP_UPDATER_ARTIFACTS=1

ensure_translator_is_not_running() {
  if pgrep -x forum-shell >/dev/null 2>&1; then
    echo "AI Vision Forum is currently running." >&2
    echo "Quit every AI Vision Forum window before rebuilding the app bundle." >&2
    echo "Replacing a live macOS bundle can leave WKWebView attached to stale files and show a blank window." >&2
    exit 1
  fi
}

usage() {
  cat <<EOF
Usage:
  $(basename "$0") [options]

Options:
  --app-name <name>      App name shown in Finder (default: "$APP_NAME")
  --bundle-id <id>       CFBundleIdentifier (default: "$BUNDLE_ID")
  --profile <profile>    Cargo profile: release or dev (default: "$PROFILE")
  --out-dir <dir>        Output directory for .app (default: "$OUT_DIR")
  --version <version>    App version; must match Cargo.toml (default: "$VERSION")
  --skip-updater-artifacts
                         Build a local test app without updater archive/signature
  -h, --help             Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-name) APP_NAME="$2"; shift 2 ;;
    --bundle-id) BUNDLE_ID="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --skip-updater-artifacts) SKIP_UPDATER_ARTIFACTS=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1"; usage; exit 1 ;;
  esac
done

if [[ "$VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "App version $VERSION does not match the workspace version $WORKSPACE_VERSION"
  echo "Update workspace Cargo.toml, product.json, Tauri and UI package versions together."
  exit 1
fi
if [[ "$BUNDLE_ID" != "org.aivisionforum.agent" || "$APP_NAME" != "AI Vision Forum" ]]; then
  echo "Product identity comes from profiles/ai-vision-forum/product.json; change it there and update the shell config first."
  exit 1
fi
if [[ "$PROFILE" != "release" && "$PROFILE" != "dev" ]]; then
  echo "Unsupported profile: $PROFILE (expected release or dev)"
  exit 1
fi
PROFILE_DIR="$PROFILE"
if [[ "$PROFILE" == "dev" ]]; then
  PROFILE_DIR="debug"
fi
if [[ "$BUILD_TARGET_DIR" == *" "* ]]; then
  echo "Cargo target directory cannot contain spaces: $BUILD_TARGET_DIR"
  exit 1
fi
if [[ "$BUILD_TARGET_DIR" != /* ]]; then
  echo "FORUM_AGENT_CARGO_TARGET_DIR must be an absolute path: $BUILD_TARGET_DIR" >&2
  exit 1
fi
if [[ ! "$MARKETING_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "macOS marketing version must contain three numeric components: $MARKETING_VERSION"
  exit 1
fi
if [[ ! "$BUILD_VERSION" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  echo "macOS build version must contain one to three numeric components: $BUILD_VERSION"
  exit 1
fi

# Never mutate an application bundle while macOS still has its executable and
# WebContent processes open. This check is repeated immediately before the
# replacement because the build itself can take several minutes.
ensure_translator_is_not_running

mkdir -p "$BUILD_TARGET_DIR" "$OUT_DIR"
export CARGO_TARGET_DIR="$BUILD_TARGET_DIR"
export FORUM_AGENT_DORA_TARGET_DIR="$BUILD_TARGET_DIR"

resolve_mlx_prebuilt_path() {
  local build_dir="$BUILD_TARGET_DIR/$PROFILE_DIR/build"
  if [[ -d "$build_dir" ]]; then
    find "$build_dir" -type d -path '*mlx-sys-*/out/mlx-prebuilt' 2>/dev/null | tail -n 1 || true
  fi
}

run_cargo_build() {
  local mlx_prebuilt_path=""
  mlx_prebuilt_path="$(resolve_mlx_prebuilt_path)"
  if [[ -n "$mlx_prebuilt_path" ]]; then
    MLX_PREBUILT_PATH="$mlx_prebuilt_path" cargo build --locked "$@"
  else
    cargo build --locked "$@"
  fi
}

CARGO_PROFILE_ARGS=(--release)
if [[ "$PROFILE" == "dev" ]]; then
  CARGO_PROFILE_ARGS=(--profile dev)
fi

echo "Building required local translation executables..."
run_cargo_build --manifest-path "$ROOT_DIR/Cargo.toml" "${CARGO_PROFILE_ARGS[@]}" \
  -p dora-qwen3-asr \
  -p dora-qwen35-translator \
  -p hen-local-init

TARGET_TRIPLE="aarch64-apple-darwin"
SIDECAR_DIR="$ROOT_DIR/forum-shell/.tauri-sidecars"
rm -rf "$SIDECAR_DIR"
mkdir -p "$SIDECAR_DIR"
stage_sidecar() {
  local source="$1"
  local name="$2"
  if [[ ! -f "$source" ]]; then
    echo "Required sidecar not found: $source"
    exit 1
  fi
  cp "$source" "$SIDECAR_DIR/${name}-${TARGET_TRIPLE}"
  chmod +x "$SIDECAR_DIR/${name}-${TARGET_TRIPLE}"
}
stage_sidecar "$FORUM_AGENT_DORA_BIN" "dora"
stage_sidecar "$BUILD_TARGET_DIR/$PROFILE_DIR/dora-qwen3-asr" "dora-qwen3-asr"
stage_sidecar "$BUILD_TARGET_DIR/$PROFILE_DIR/dora-qwen35-translator" "dora-qwen35-translator"
stage_sidecar "$BUILD_TARGET_DIR/$PROFILE_DIR/hen-local-init" "hen-local-init"
stage_sidecar "$BUILD_TARGET_DIR/$PROFILE_DIR/mlx.metallib" "mlx.metallib"

# Use Tauri's official bundler for the application shell. The previous script
# assembled Info.plist and the launcher by hand, which produced WebViews that
# could open as empty white windows in the distributed DMG.
# Build a standalone interpreter and hash-pinned wheels. The build interpreter
# is bootstrap tooling only and is never copied into the app.
BUILD_PYTHON="${FORUM_AGENT_BUILD_PYTHON:-python3}"
WORKER_STAGE="$SIDECAR_DIR/meeting-worker"
WORKER_PACKAGE_ARGS=(--with-asr --output "$WORKER_STAGE" --cache "${FORUM_AGENT_WORKER_CACHE:-$BUILD_TARGET_DIR/worker-artifacts}")
if [[ "${FORUM_AGENT_BUILD_OFFLINE:-0}" == "1" ]]; then
  WORKER_PACKAGE_ARGS+=(--offline)
fi
"$BUILD_PYTHON" "$ROOT_DIR/scripts/package_meeting_worker.py" "${WORKER_PACKAGE_ARGS[@]}"
echo "Building the Tauri application bundle..."
(
  cd "$ROOT_DIR/forum-shell"
  TAURI_BUILD_ARGS=(build --bundles app --config tauri.bundle.conf.json)
  if [[ "$SKIP_UPDATER_ARTIFACTS" == "1" ]]; then
    TAURI_BUILD_ARGS+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
  fi
  if [[ "$PROFILE" == "dev" ]]; then
    TAURI_BUILD_ARGS+=(--debug)
  fi
  ./ui/node_modules/.bin/tauri "${TAURI_BUILD_ARGS[@]}"
)

TAURI_APP="$BUILD_TARGET_DIR/$PROFILE_DIR/bundle/macos/$TAURI_PRODUCT_NAME.app"
if [[ ! -d "$TAURI_APP" ]]; then
  echo "Tauri app bundle not found: $TAURI_APP"
  exit 1
fi

APP_DIR="$OUT_DIR/$APP_NAME.app"
ensure_translator_is_not_running
rm -rf "$APP_DIR"
cp -R "$TAURI_APP" "$APP_DIR"

cp -R "$WORKER_STAGE" "$APP_DIR/Contents/Resources/meeting-worker"
mkdir -p "$APP_DIR/Contents/Resources/diagnostics"
cp "$ROOT_DIR/scripts/check_meeting_worker_bundle.py" "$APP_DIR/Contents/Resources/diagnostics/"
cp "$ROOT_DIR/../scripts/f01_lan_probe.py" "$APP_DIR/Contents/Resources/diagnostics/"
# Run from the final resource location before signing, with developer paths
# excluded by the checker's subprocess environment.
"$APP_DIR/Contents/Resources/meeting-worker/python/bin/python3.12" -I -B \
  "$APP_DIR/Contents/Resources/diagnostics/check_meeting_worker_bundle.py" \
  --resource-dir "$APP_DIR/Contents/Resources/meeting-worker" \
  --report "$OUT_DIR/meeting-worker-local-check-$(date +%s).json"

PLIST_PATH="$APP_DIR/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $MARKETING_VERSION" "$PLIST_PATH"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_VERSION" "$PLIST_PATH"
codesign --force --deep --sign - "$APP_DIR"

echo "Updater artifacts are disabled by the Forum development profile."
rm -rf "$SIDECAR_DIR"

echo "App bundle created with the official Tauri shell:"
echo "  $APP_DIR"
echo "Standalone Python/MLX included; clean-Mac acceptance and production analysis jobs are pending."
echo "Models are not bundled. Prepare them before offline use."
