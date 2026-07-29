# 论坛智能体（Forum Agent）需求说明 / Requirement Spec v0.1

> AI Vision Forum 深圳 2026 · Source: organizing committee draft (Google Doc v0.1), archived here for the repo.

## 愿景 / Vision

不只是"转写工具"，而是一位参加论坛的 AI 参会者：它听每一场讨论、做笔记、翻译、归纳、在恰当的时刻把洞察反馈给会场，会后产出报告。它本身就是论坛主题（人机协同 / Human Agency）的现场演示——AI 放大讨论质量，人保留判断与结论权。

Not a transcription tool but an AI participant attending the Forum: it listens to every session, takes notes, translates, synthesizes, feeds insights back to the room at the right moments, and drafts the report afterward.

## 能力需求 / Capabilities (by priority)

### P0 — 必须具备 / must have
- **C1** 实时双语字幕 live bilingual subtitles: EN⇄中文, code-switching support, < 3 s latency
- **C2** 双会场并行 two rooms in parallel: Day 2 split tracks, independent mixer-fed audio
- **C3** 全程录音与说话人分离转写 full recording + diarized, timestamped, anonymous transcripts (Chatham House)

### P1 — 智能体升级 / the agent upgrade
- **C4** 实时洞察面板 live insight panel: rolling summary, emerging consensus, tensions, open questions every 3–5 min
- **C5** 跨会场情报 cross-room feed during split tracks: one-line "what the other room is converging on"
- **C6** 环节即时纪要 instant end-of-session minutes (要点/结论/待办) for report-backs
- **C7** 会后综合报告初稿 draft synthesis report within 24 h of closing

### P2 — 探索项 / stretch
- **C8** 提问建议 suggested follow-up questions (facilitator's screen only)
- **C9** 参会者查询 participant QR-code query page (personal review, no interruption)
- **C10** 闭幕即席综述 closing-ceremony "AI's view of the two days"

## 硬性约束 / Hard constraints
- **本地运行 fully local/on-prem**: no venue-internet dependence, no audio to any cloud (Chatham House + compliance); local Whisper / 通义听悟 / 讯飞 local for ASR, local Qwen-class model for summarization
- **匿名化 anonymized everywhere**: no names or affiliations in any display or output; diarization labels only 发言人A/B / Speaker A/B
- **人在环上 human-in-the-loop**: all AI output is DRAFT until a facilitator/organizer confirms — itself a demonstration of Human Agency
- **开源 open source**: released as an OAIC/HAgency ecosystem artifact, reusable at future AIVF / GOSIM / AAAA events

## 落地 / Delivery
Two rehearsals in September 2026; hardware: Mac mini / local GPU host per room; audio from the mixing board (not room microphones).
