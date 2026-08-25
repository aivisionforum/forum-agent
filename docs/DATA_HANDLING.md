# Data handling / 数据处理

The Forum Agent runs at an invitation-only event under the Chatham House
Rule. Everything below follows from one fact: **the transcript contains
every name spoken aloud, in plaintext, and (if recording is enabled) sits
beside a WAV of the room** (stress/RESULTS.md). The pipeline anonymises
speaker *labels* (Speaker A/B/C) — it does not anonymise speech *content*.

## What is stored, and where / 存储内容与位置

All data stays on the operator's machine, under `data/`. Nothing is sent to
any network service: ASR, translation, diarization, insights, minutes and
reports all run on local models, and the web server binds to 127.0.0.1 only.

| File | Content | When |
|---|---|---|
| `data/<room>_transcript.jsonl` | verbatim text, speaker labels, timestamps | always, during a session |
| `data/<room>_translations.jsonl` | machine translations of the above | always |
| `data/<room>_insights*.json(l)` | AI summaries (drafts until approved) | always |
| `data/<room>_minutes*.md`, `data/report_draft.md` | AI minutes / report drafts | on demand |
| `data/<room>_recording.wav` | raw room audio | **on by default** as a backup; disable per session (console checkbox) |
| `data/sessions/<id>/` | all of the above, archived per session | on session stop |

## Recording / 录音

Raw audio recording is **on by default**: the WAV is the backup that lets a
session be re-processed if anything in the live pipeline fails (upload it
back through "Process recording into a session"). The room should be told it
is being recorded. For a session that must not be recorded, untick the
"save raw audio recording" checkbox before starting (or pass `--no-record`
on the CLI) — live subtitles, transcripts and insights work identically
without it.

## Names in transcripts / 转录中的人名

Chatham House anonymises attribution, but participants say names out loud.
After a session, run the **names check** (console, per archived session): a
local LLM scans the transcript and writes `redaction_report.md` into the
session folder, listing every personal name it found with the lines it
appears in. The operator reviews the report and edits the transcript/minutes
by hand before anything leaves the machine. The check is advisory — a draft
for a human, like every other AI output here. It never modifies files itself.

## Retention and deletion / 保留与删除

- There is no automatic retention: archives stay until the operator deletes
  them. Delete a session from the console (its whole `data/sessions/<id>/`
  folder is removed) or delete `data/` wholesale after the event report is
  final.
- Recommended: after the forum, keep only the approved minutes/report and
  delete transcripts and audio within 30 days.
- `data/` is git-ignored in its entirety. Never commit or upload anything
  under it — transcripts, recordings, logs and screenshots of real sessions
  are all private event material.
