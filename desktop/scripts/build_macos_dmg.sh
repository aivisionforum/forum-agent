#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script only supports macOS."
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="AI Vision Forum"
APP_PATH="$ROOT_DIR/dist/${APP_NAME}.app"
OUT_DIR="$ROOT_DIR/dist"
VERSION=""
VOL_NAME=""
DMG_NAME=""
PROFILE="dev"

read_workspace_version() {
  sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' "$ROOT_DIR/Cargo.toml" | head -n 1
}

read_app_version() {
  local plist="$APP_PATH/Contents/Info.plist"
  if [[ -f "$plist" ]] && command -v /usr/libexec/PlistBuddy >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist" 2>/dev/null || true
  fi
}

usage() {
  cat <<EOF
Usage:
  $(basename "$0") [options]

Options:
  --app-path <path>   Path to .app bundle (default: $APP_PATH)
  --out-dir <dir>     Output directory (default: $OUT_DIR)
  --version <version> Version used for default DMG naming (default: app bundle version)
  --vol-name <name>   DMG volume name (default: "$APP_NAME <version> Installer")
  --dmg-name <name>   DMG file name (default: AI-Vision-Forum-v<version>.dmg)
  --profile <name>    Explicit dev or release app verification (default: dev)
  -h, --help          Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-path)
      APP_PATH="$2"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --vol-name)
      VOL_NAME="$2"
      shift 2
      ;;
    --dmg-name)
      DMG_NAME="$2"
      shift 2
      ;;
    --profile) PROFILE="$2"; shift 2 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      usage
      exit 1
      ;;
  esac
done

if [[ ! -d "$APP_PATH" ]]; then
  echo "App bundle not found: $APP_PATH"
  echo "Build it first with scripts/build_macos_app.sh"
  exit 1
fi
if [[ "$PROFILE" != "dev" && "$PROFILE" != "release" ]]; then
  echo "Choose --profile dev or release." >&2; exit 1
fi
APP_PYTHON="$APP_PATH/Contents/Resources/meeting-worker/python/bin/python3.12"
"$APP_PYTHON" -I -B "$ROOT_DIR/scripts/check_forum_app.py" --app "$APP_PATH" --profile "$PROFILE" >/dev/null

WORKSPACE_VERSION="$(read_workspace_version)"
WORKSPACE_MARKETING_VERSION="${WORKSPACE_VERSION%%[-+]*}"
APP_VERSION="$(read_app_version)"
if [[ -z "$WORKSPACE_VERSION" ]]; then
  echo "Failed to determine the workspace version from Cargo.toml"
  exit 1
fi
if [[ -z "$APP_VERSION" ]]; then
  echo "Failed to determine the app bundle version"
  exit 1
fi
if [[ "$APP_VERSION" != "$WORKSPACE_MARKETING_VERSION" ]]; then
  echo "App bundle version $APP_VERSION does not match the workspace marketing version $WORKSPACE_MARKETING_VERSION"
  echo "Synchronize workspace, product, Tauri and UI versions, then rebuild the app."
  exit 1
fi
if [[ -n "$VERSION" && "$VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "DMG version $VERSION does not match the workspace version $WORKSPACE_VERSION"
  echo "Synchronize workspace, product, Tauri and UI versions before changing the DMG version."
  exit 1
fi
VERSION="$WORKSPACE_VERSION"
if [[ -z "$VOL_NAME" ]]; then
  VOL_NAME="$APP_NAME $VERSION $PROFILE Installer"
fi
if [[ -z "$DMG_NAME" ]]; then
  DMG_NAME="AI-Vision-Forum-v${VERSION}-${PROFILE}.dmg"
fi
if [[ "$DMG_NAME" != "$(basename "$DMG_NAME")" || "$DMG_NAME" != *.dmg ]]; then
  echo "DMG name must be one .dmg filename without directory components." >&2; exit 1
fi

mkdir -p "$OUT_DIR"
DMG_PATH="$OUT_DIR/$DMG_NAME"
STAGING_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGING_DIR"' EXIT
if [[ -e "$DMG_PATH" || -L "$DMG_PATH" || -e "$DMG_PATH.manifest.json" ]]; then
  echo "DMG output already exists; choose a new name rather than overwrite an existing candidate." >&2; exit 1
fi

cp -R "$APP_PATH" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"
if [[ "$PROFILE" == "dev" ]]; then
  cat > "$STAGING_DIR/DEVELOPMENT-TEST-PACKAGE.txt" <<'NOTICE'
AI Vision Forum development test package

This app uses an ad-hoc local signature. It is not a Developer ID signed or
Apple-notarized release. Clean-Mac installation and meeting-quality acceptance
remain pending. Model weights are not included; prepare authorized local models
before offline use. Do not infer distribution readiness from this DMG.
NOTICE
fi

hdiutil create \
  -volname "$VOL_NAME" \
  -srcfolder "$STAGING_DIR" \
  -format UDZO \
  "$DMG_PATH"
hdiutil verify "$DMG_PATH" >/dev/null
"$APP_PYTHON" -I -B - "$DMG_PATH" "$APP_PATH" "$PROFILE" "$VERSION" <<'PY'
import hashlib, json, pathlib, sys
dmg = pathlib.Path(sys.argv[1]); digest = hashlib.sha256()
with dmg.open('rb') as source:
    for data in iter(lambda: source.read(1024 * 1024), b''):
        digest.update(data)
manifest = {'schema_version': 1, 'owner': 'ai-vision-forum-dmg-v1', 'profile': sys.argv[3],
    'version': sys.argv[4], 'filename': dmg.name, 'size_bytes': dmg.stat().st_size, 'sha256': digest.hexdigest(),
    'signature': 'unsigned_container', 'notarization': 'not_submitted',
    'app_signature': 'ad_hoc_development' if sys.argv[3] == 'dev' else 'developer_id_application',
    'model_weights_included': False, 'formal_clean_mac_acceptance': False}
with pathlib.Path(str(dmg) + '.manifest.json').open('x') as output:
    json.dump(manifest, output, indent=2); output.write('\n')
PY

rm -rf "$STAGING_DIR"

echo "DMG created:"
echo "  $DMG_PATH"
echo "Profile: $PROFILE; notarization not performed. Container checksum: $DMG_PATH.manifest.json"
