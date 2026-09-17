# 整合桌面版的数据处理

本节适用于新的 AI Vision Forum.app；下方旧 `data/`/8710 描述仅适用于 Python 版本。

默认数据目录为 `~/Library/Application Support/AI Vision Forum/`：SQLite 及迁移在 `store/`；私有录音/恢复日志在 `sessions/<id>/audio/`（双轨进一步按 `tracks/<id>` 分开）；分析和说话人任务的本地工作文件在对应 jobs 目录。匿名 embedding/聚类信息也属于本地会议资料；匿名标签不等于说话内容已去掉人名。

ASR、翻译、会议分析和声纹均在本机运行。LAN 默认关闭；操作员启用并授权后，会向本机/已配对会场和限时只读客户端提供独立审核发布的正文与证据副本，远端可缓存这些已授权资料。不会通过这些接口发送原始录音、私有逐字稿、draft 或主持人问题。新桌面版没有接入旧版的 cloud polish 功能。

公开标题、正文和证据摘录须由操作员审查脱敏。撤回会传给在线对端；离线设备只能在恢复连接后获知，已有读者保存的资料不能远程收回。操作员会看到 stale 和最后同步时间；过期内容不能选入新的跨场任务。

本机 CA/服务证书保存在私有目录；CA 私钥只用于签发后删除。向手机转移的是公开 `ca.cer`，不要分享服务端私钥。二维码和配对邀请包含短期访问凭证，应只交给对应参会者/会场；可在共享面板撤销。重启保留证书但不会自动开启 LAN 或恢复凭证。

会后保留范围按活动政策执行。清理前应退出应用并备份需要保留的审核成果；不要在录音/恢复/分析时手动删除数据库或工作目录。测试和上传只使用授权合成素材或不含原文的指标，真实录音、字幕、向量、凭证与模型权重不提交仓库。

---

# 旧 Python 版本数据处理说明

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

## Post-event cloud polish / 会后云端润色

Everything above stays local. One explicit exception exists for the
production phase after the event: the operator can send a single draft
(one session's minutes, or the event report) to a cloud model for editorial polish — via OpenRouter, an Ollama cloud
model, or the operator's own Claude Code / Codex CLI subscriptions
(text then goes to Anthropic / OpenAI under that account). This is opt-in per run
with an on-screen warning, available only when the operator has configured
credentials in the environment (`OPENROUTER_API_KEY`, or a local Ollama
with `-cloud` models). By default what is sent is exactly the draft's text. An explicit extra
opt-in ("include full transcript") additionally sends the verbatim
transcript(s) so the cloud model can correct the draft from source —
that is every word spoken, so run the names check and edit first;
the prompt instructs the model to preserve Chatham House anonymity in
its output, but the input still leaves the machine. The polished copy is written beside the
draft, clearly labelled as cloud-produced, and the draft is never modified.

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
