#!/usr/bin/env bash
# The imported translation models use the same Rust downloader as the desktop.
# Model paths/providers can be overridden through macos_bootstrap.sh variables.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$SCRIPT_DIR/macos_bootstrap.sh"
