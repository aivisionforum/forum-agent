"""M1 acceptance check, run AFTER a full replay:

    python -m forum_agent.replay data/fixture_meeting.wav --room room1
    python tests/acceptance_m1.py

Verifies against PROMPT.md M1 criteria:
 1. transcript JSONL complete: covers the fixture (>=85% of reference
    utterances matched by time overlap), monotonic non-overlapping times,
    every record has all required keys and non-empty text;
 2. subtitle lag < 3s (from replay_stats.json recorded during replay);
 3. bilingual: both zh and en/mixed segments present, translations attached
    for a clear majority of segments (checked from the translations log).
"""
import json
import sys
from pathlib import Path

REQUIRED_KEYS = {"t_start", "t_end", "speaker_id", "lang", "text"}
FAILURES = []


def check(name: str, ok: bool, detail: str) -> None:
    print(f"{'PASS' if ok else 'FAIL'}  {name}: {detail}")
    if not ok:
        FAILURES.append(name)


def main() -> None:
    records = [json.loads(line) for line in
               Path("data/room1_transcript.jsonl").read_text().splitlines()]
    ref = json.loads(Path("data/fixture_reference.json").read_text())
    stats = json.loads(Path("data/replay_stats.json").read_text())

    check("records-exist", len(records) > 0, f"{len(records)} JSONL records")
    check("schema", all(REQUIRED_KEYS <= set(r) and r["text"].strip()
                        for r in records), "all keys present, text non-empty")
    check("monotonic", all(a["t_end"] <= b["t_start"] + 0.01 for a, b in
                           zip(records, records[1:])), "no overlapping segments")

    covered = sum(1 for u in ref if any(
        min(u["t_end"], r["t_end"]) - max(u["t_start"], r["t_start"]) >
        0.5 * (u["t_end"] - u["t_start"]) for r in records))
    check("coverage", covered >= 0.85 * len(ref),
          f"{covered}/{len(ref)} reference utterances covered")

    check("lag", stats["max_lag_s"] < 3.0,
          f"max {stats['max_lag_s']}s, mean {stats['mean_lag_s']}s (budget 3s)")

    langs = {r["lang"] for r in records}
    check("bilingual", bool(langs & {"zh", "mixed"}) and "en" in langs,
          f"languages seen: {sorted(langs)}")

    tpath = Path("data/room1_translations.jsonl")
    n_trans = len(tpath.read_text().splitlines()) if tpath.exists() else 0
    check("translations", n_trans >= 0.8 * len(records),
          f"{n_trans}/{len(records)} segments translated")

    speakers = {r["speaker_id"] for r in records}
    check("diarization", 2 <= len(speakers) <= 6, f"speakers: {sorted(speakers)}")

    sys.exit(1 if FAILURES else 0)


if __name__ == "__main__":
    main()
