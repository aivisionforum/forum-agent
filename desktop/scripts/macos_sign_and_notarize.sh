#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script only supports macOS." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_PATH=""
DMG_PATH=""
PROFILE=""

usage() {
  cat <<EOF
Usage:
  $(basename "$0") --app </path/to/AI Vision Forum.app> --profile dev
  $(basename "$0") --app </path/to/AI Vision Forum.app> --profile release
  $(basename "$0") --dmg </path/to/AI-Vision-Forum.dmg> --profile release

Release signing requires APPLE_SIGNING_IDENTITY (Developer ID Application).
DMG notarization also requires an existing APPLE_NOTARY_KEYCHAIN_PROFILE.
This script never creates, exports or changes certificates/keychain credentials.
Dev uses an ad-hoc signature and is never notarized or called a signed release.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP_PATH="$2"; shift 2 ;;
    --dmg) DMG_PATH="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if [[ "$PROFILE" != "dev" && "$PROFILE" != "release" ]]; then
  echo "Select an explicit --profile dev or --profile release." >&2; exit 1
fi
if [[ -n "$APP_PATH" && -n "$DMG_PATH" ]] || [[ -z "$APP_PATH" && -z "$DMG_PATH" ]]; then
  echo "Choose exactly one --app or --dmg." >&2; exit 1
fi
if [[ "$PROFILE" == "release" && -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "Release requires an existing Developer ID Application identity in APPLE_SIGNING_IDENTITY; no ad-hoc fallback." >&2
  exit 1
fi

if [[ -n "$APP_PATH" ]]; then
  if [[ ! -d "$APP_PATH" || -L "$APP_PATH" ]]; then
    echo "App must be an existing real .app directory." >&2; exit 1
  fi
  PYTHON="$APP_PATH/Contents/Resources/meeting-worker/python/bin/python3.12"
  "$PYTHON" -I -B "$SCRIPT_DIR/check_forum_app.py" --app "$APP_PATH" --profile "$PROFILE" --allow-unsigned >/dev/null
  SIGN_ARGS=(--force --timestamp=none --sign -)
  if [[ "$PROFILE" == "release" ]]; then
    SIGN_ARGS=(--force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY")
  fi
  # Resolve the full native inventory before signing. A failed scan must stop
  # this operation, rather than disappear as an unchecked process substitution.
  SIGN_INVENTORY="$(mktemp "${TMPDIR:-/tmp}/forum-sign-inventory.XXXXXX")"
  trap 'rm -f "$SIGN_INVENTORY"' EXIT
  "$PYTHON" -I -B "$SCRIPT_DIR/list_forum_native_code.py" --app "$APP_PATH" > "$SIGN_INVENTORY"
  while IFS= read -r -d '' code_file; do
    /usr/bin/codesign "${SIGN_ARGS[@]}" "$code_file"
  done < "$SIGN_INVENTORY"
  /usr/bin/codesign "${SIGN_ARGS[@]}" "$APP_PATH"
  "$PYTHON" -I -B "$SCRIPT_DIR/check_forum_app.py" --app "$APP_PATH" --profile "$PROFILE" >/dev/null
  echo "App signature verified for explicit profile: $PROFILE"
  echo "Notarization was not requested by app signing."
  exit 0
fi

if [[ "$PROFILE" != "release" ]]; then
  echo "Dev DMGs are local test packages and cannot enter the notarization release flow." >&2; exit 1
fi
if [[ ! -f "$DMG_PATH" || -L "$DMG_PATH" ]]; then
  echo "DMG must be an existing regular file." >&2; exit 1
fi
if [[ -z "${APPLE_NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  echo "Set APPLE_NOTARY_KEYCHAIN_PROFILE to an existing notarytool credential profile; this script does not store credentials." >&2
  exit 1
fi
DMG_MANIFEST="$DMG_PATH.manifest.json"
if [[ ! -f "$DMG_MANIFEST" ]] || [[ "$(/usr/bin/plutil -extract profile raw -o - "$DMG_MANIFEST")" != "release" ]]; then
  echo "Notarization requires the matching release DMG manifest produced by build_macos_dmg.sh." >&2; exit 1
fi
RECORDED_SHA="$(/usr/bin/plutil -extract sha256 raw -o - "$DMG_MANIFEST")"
ACTUAL_SHA="$(/usr/bin/shasum -a 256 "$DMG_PATH" | /usr/bin/awk '{print $1}')"
if [[ "$RECORDED_SHA" != "$ACTUAL_SHA" ]]; then
  echo "DMG differs from its recorded release candidate; refusing notarization." >&2; exit 1
fi
/usr/bin/codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$DMG_PATH"
/usr/bin/codesign --verify --verbose=2 "$DMG_PATH"
SIGNATURE_DETAIL="$(/usr/bin/codesign --display --verbose=4 "$DMG_PATH" 2>&1)"
if [[ "$SIGNATURE_DETAIL" != *"Authority=Developer ID Application:"* ]]; then
  echo "DMG signature is not Developer ID Application." >&2; exit 1
fi
SIGNED_SHA="$(/usr/bin/shasum -a 256 "$DMG_PATH" | /usr/bin/awk '{print $1}')"
/usr/bin/plutil -replace sha256 -string "$SIGNED_SHA" "$DMG_MANIFEST"
/usr/bin/plutil -replace size_bytes -integer "$(/usr/bin/stat -f %z "$DMG_PATH")" "$DMG_MANIFEST"
/usr/bin/plutil -replace signature -string "developer_id_application" "$DMG_MANIFEST"
/usr/bin/plutil -replace notarization -string "submission_pending" "$DMG_MANIFEST"
RESULT_PATH="$DMG_PATH.notary-result.json"
if ! /usr/bin/xcrun notarytool submit "$DMG_PATH" \
    --keychain-profile "$APPLE_NOTARY_KEYCHAIN_PROFILE" --wait --timeout 45m \
    --output-format json > "$RESULT_PATH"; then
  echo "Apple notarization did not complete successfully; inspect $RESULT_PATH. No acceptance claimed." >&2
  exit 1
fi
STATUS="$(/usr/bin/plutil -extract status raw -o - "$RESULT_PATH")"
if [[ "$STATUS" != "Accepted" ]]; then
  echo "Apple notarization status: $STATUS. Package is not accepted." >&2; exit 1
fi
/usr/bin/xcrun stapler staple "$DMG_PATH"
/usr/bin/xcrun stapler validate "$DMG_PATH"
/usr/sbin/spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG_PATH"
FINAL_SHA="$(/usr/bin/shasum -a 256 "$DMG_PATH" | /usr/bin/awk '{print $1}')"
/usr/bin/plutil -replace sha256 -string "$FINAL_SHA" "$DMG_MANIFEST"
/usr/bin/plutil -replace size_bytes -integer "$(/usr/bin/stat -f %z "$DMG_PATH")" "$DMG_MANIFEST"
/usr/bin/plutil -replace notarization -string "accepted_and_stapled" "$DMG_MANIFEST"
echo "Developer ID DMG accepted by Apple, stapled and assessed: $DMG_PATH"
