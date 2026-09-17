#!/usr/bin/env bash
# Archive an already assembled F01 test kit, preserving macOS code signatures
# stored in extended attributes (notably the Metal resource sidecar).
set -euo pipefail
if [[ $# -ne 1 || "$1" != /* || ! -d "$1/AI Vision Forum.app" ]]; then
  echo 'Usage: archive_f01_test_kit.sh /absolute/path/to/assembled-test-kit' >&2
  exit 1
fi
KIT_DIR="${1%/}"
ARCHIVE="$KIT_DIR.zip"
if [[ -e "$ARCHIVE" ]]; then
  echo "Archive already exists; choose a new kit path: $ARCHIVE" >&2
  exit 1
fi
/usr/bin/codesign --verify --deep --strict "$KIT_DIR/AI Vision Forum.app"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$KIT_DIR" "$ARCHIVE"
VERIFY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/forum-f01-unpack.XXXXXX")"
trap 'rm -rf "$VERIFY_DIR"' EXIT
/usr/bin/ditto -x -k "$ARCHIVE" "$VERIFY_DIR"
/usr/bin/codesign --verify --deep --strict "$VERIFY_DIR/$(basename "$KIT_DIR")/AI Vision Forum.app"
/usr/bin/shasum -a 256 "$ARCHIVE" > "$ARCHIVE.sha256"
echo "Verified archive: $ARCHIVE"
