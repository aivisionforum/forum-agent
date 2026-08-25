"""Startup resource check: warn upfront when the machine cannot run the
models reliably, instead of dying mid-forum (the watchdog makes a dead
model server recoverable, not painless — each restart drops translation
for ~30s)."""
import os
import subprocess

# Live path (Whisper + 8B translator) fits in ~7 GB of model weights but
# needs headroom; the 32B report model adds ~18 GB on top.
MIN_RAM_GB = 16
RECOMMENDED_RAM_GB = 64

warning: str | None = None  # surfaced on /api/status for the console


def total_ram_gb() -> int:
    out = subprocess.run(["sysctl", "-n", "hw.memsize"],
                         capture_output=True, text=True)
    return int(out.stdout.strip()) // (1024 ** 3)


def check() -> None:
    """Print the verdict; refuse to start below MIN_RAM_GB unless
    FORUM_AGENT_FORCE=1. Sets `warning` for undersized-but-allowed machines."""
    global warning
    ram = total_ram_gb()
    if ram >= RECOMMENDED_RAM_GB:
        print(f"[preflight] {ram} GB RAM — full pipeline supported")
        return
    if ram < MIN_RAM_GB:
        msg = (f"{ram} GB RAM is below the {MIN_RAM_GB} GB minimum: the "
               "model server will likely be killed by the OS under load")
        if os.environ.get("FORUM_AGENT_FORCE") != "1":
            raise SystemExit(
                f"[preflight] {msg}.\nSet FORUM_AGENT_FORCE=1 to start "
                "anyway — expect intermittent model-server restarts and "
                "unreliable translation/insights.")
        warning = msg + " (forced start)"
    else:
        warning = (f"{ram} GB RAM is below the {RECOMMENDED_RAM_GB} GB "
                   "recommended: live subtitles should work, but the 32B "
                   "report model may crash the model server; the watchdog "
                   "will restart it, dropping translation for ~30s each time")
    print(f"[preflight] warning: {warning}")
