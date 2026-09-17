#!/usr/bin/env bash
# Select the native sidecar, never a Python/Conda launcher that happens to be on PATH.
resolve_native_dora() {
  local candidate description version
  local candidates=()
  if [[ -n "${FORUM_AGENT_DORA_BIN:-}" ]]; then
    candidates=("$FORUM_AGENT_DORA_BIN")
  else
    if [[ -n "${FORUM_AGENT_APP_RESOURCES:-}" ]]; then
      candidates+=("$FORUM_AGENT_APP_RESOURCES/../MacOS/dora")
    fi
    candidates+=("$HOME/.cargo/bin/dora")
    candidate="$(command -v dora || true)"
    [[ -z "$candidate" ]] || candidates+=("$candidate")
  fi
  for candidate in "${candidates[@]}"; do
    [[ "$candidate" == /* && -x "$candidate" ]] || continue
    description="$(file -Lb "$candidate")"
    if [[ "$description" == *Mach-O* && "$description" == *arm64* ]]; then
      version="$("$candidate" --version 2>/dev/null)" || continue
      [[ "${version%%$'\n'*}" == 'dora-cli 0.4.1' ]] || continue
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo 'Native Dora CLI missing. Set FORUM_AGENT_DORA_BIN to an arm64 dora-cli 0.4.1 binary.' >&2
  return 1
}
