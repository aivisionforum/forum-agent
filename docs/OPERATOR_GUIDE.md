# Forum Agent — Operator Guide

For the person running the Forum Agent at the event. Assumes a working,
already-installed machine (installation lives in the README). Organized by
the day's timeline. Chinese version follows the English one below.
中文版在英文版之后。

## 1. Before the session (morning setup)

1. Double-click **Forum Agent.command** (Desktop or repo folder). A terminal
   window opens — keep it open all day; closing it shuts everything down.
   If the agent is already running, the double-click just opens the console.
2. Open the three pages:
   - **Console** (your screen): http://127.0.0.1:8710/control
   - **Subtitles wall** (projector 1): http://127.0.0.1:8710/subtitles
   - **Insights wall** (projector 2): http://127.0.0.1:8710/insights
3. Mic check: click **Start microphone** and speak — the level meter should
   move. If it doesn't, check macOS input settings, then Stop and re-start.
4. Announce to the room that audio is being recorded (recording is on by
   default as a backup).

## 2. Starting a session

- **Save raw audio recording** — leave ON (it is the backup that lets a
  failed session be re-processed). Untick only for a session that must not
  be recorded.
- **Auto-approve** — ON means new AI points go straight to the projector and
  you fix mistakes live (edit / hide). OFF is gatekeeper mode: nothing shows
  until you approve it. Use OFF if you can watch the console continuously.

## 3. During the session

- The first insight takes a few minutes — the insight wall shows a countdown
  and a WORKING banner. **A banner or countdown means it is thinking, not
  broken.** Insights refresh about every 3 minutes.
- Wrong or sensitive point on the wall: in the console's insight list, click
  **edit** to fix it or **hide** to pull it. Un-approving demotes it to draft.
- **Restart app** (Maintenance) stops the live session first, waits for
  work to drain, then restarts. Only use it if the app is truly stuck —
  a WORKING banner is not stuck.
- The **session so far** page (linked from the console) is a live list of
  approved points — useful for a moderator's wrap-up.

## 4. Stopping — minutes vs report

- Click **Stop**. Minutes are drafted automatically (~2 minutes; up to
  ~8 minutes the very first time while the large model loads).
- **Minutes** are per session: the *minutes* link appears in that session's
  row under Past sessions.
- The **report** is cross-session and manual: tick the sessions to include,
  generate it, and its link appears at the top of the console.
- Every AI output is a draft until a human reviews it.

## 5. After the event

1. Run **names check** on each session (per-session row). It writes a report
   listing personal names spoken aloud; edit the transcript/minutes by hand.
2. Optional **cloud polish** (collapsed section): sends the draft's text to
   a cloud provider — post-event only, after the names check. "Include full
   transcript" sends every word spoken; use it deliberately.
3. Delete what should not be kept: per-session **delete**, or remove the
   whole `data/` folder once the final report is done. Recommended: keep
   only approved minutes/report; delete transcripts and audio within 30 days.
4. Never upload anything under `data/` anywhere.

## 6. If something looks stuck

| Symptom | Meaning / fix |
|---|---|
| Insight wall empty with countdown or WORKING banner | Normal — it is processing. First insight takes a few minutes. |
| Wall page looks stale / old behavior | Reload the page (⌘R). Pages also reload themselves after a restart. |
| "address already in use" loop in a new terminal | The agent was already running. Close the new window; use the existing one. |
| Insight panel shows an engine error | It retries on the next 3-minute cycle. If it persists, Restart app. |
| Restart button "refused" | A session is stopping or minutes are generating — wait for the banner to clear and try again. |
| Nothing at http://127.0.0.1:8710 | The terminal window was closed. Double-click Forum Agent.command again. |

---

# 论坛智能体 — 操作指南（中文版）

写给活动现场的操作员。假定机器已安装就绪（安装步骤见 README）。按当天
时间线组织。

## 1. 会前准备（早晨）

1. 双击 **Forum Agent.command**（桌面或程序文件夹）。会弹出一个终端窗口——
   全天保持打开；关闭它即关闭整个系统。若程序已在运行，双击只会打开控制台。
2. 打开三个页面：
   - **控制台**（操作员屏幕）：http://127.0.0.1:8710/control
   - **字幕墙**（投影 1）：http://127.0.0.1:8710/subtitles
   - **洞察墙**（投影 2）：http://127.0.0.1:8710/insights
3. 试音：点击 **Start microphone** 并说话——音量条应有反应。没有反应时检查
   macOS 输入设置，然后 Stop 并重新开始。
4. 向会场宣布正在录音（录音默认开启，作为备份）。

## 2. 开始会话

- **保存原始录音**——保持开启（它是会话失败后重新处理的备份）。只有明确
  不允许录音的会话才取消勾选。
- **自动批准（auto-approve）**——开启时，AI 新要点直接上墙，操作员现场
  纠错（编辑/隐藏）；关闭即守门模式：人工批准后才显示。能持续盯着控制台
  时建议关闭。

## 3. 会议进行中

- 第一条洞察需要几分钟——洞察墙会显示倒计时和 WORKING 横幅。**有横幅或
  倒计时说明系统在思考，不是故障。** 洞察约每 3 分钟刷新一次。
- 墙上出现错误或敏感要点：在控制台洞察列表中点 **edit** 修改或 **hide**
  撤下。取消批准会将其降为草稿。
- **Restart app**（Maintenance 维护）会先停止当前会话、等待任务完成、再
  重启。只在程序真正卡死时使用——WORKING 横幅不是卡死。
- **本场会议至今（session so far）**页面（控制台有链接）实时列出已批准
  要点，可供主持人收尾使用。

## 4. 停止——纪要与报告的区别

- 点击 **Stop**。纪要自动生成（约 2 分钟；首次约 8 分钟，因为要加载大
  模型）。
- **纪要（minutes）**按会话生成：链接在 Past sessions 中该会话所在行。
- **报告（report）**跨会话、需手动生成：勾选要包含的会话后点生成，链接
  出现在控制台顶部。
- 所有 AI 输出在人工确认前都是草稿。

## 5. 会后

1. 对每个会话运行**人名检查（names check）**（会话行内）。它会生成一份
   列出口头提及人名的报告；请手工编辑转录和纪要。
2. 可选**云端润色**（折叠区）：将草稿文本发送到云端服务——仅限会后、且
   先做完人名检查。"包含完整转录"会发送所有发言原文，请慎用。
3. 删除不应保留的内容：按会话 **delete**，或在最终报告完成后整体删除
   `data/` 文件夹。建议：只保留确认后的纪要/报告，30 天内删除转录与录音。
4. `data/` 下的任何内容都不得上传到任何地方。

## 6. 疑似卡住时

| 现象 | 含义 / 处理 |
|---|---|
| 洞察墙空白但有倒计时或 WORKING 横幅 | 正常——正在处理。第一条洞察需要几分钟。 |
| 墙面页面显示陈旧 | 刷新页面（⌘R）。重启后页面也会自动刷新。 |
| 新终端窗口反复报 "address already in use" | 程序已在运行。关闭新窗口，使用原有窗口。 |
| 洞察面板显示引擎错误 | 下一个 3 分钟周期会自动重试；持续出错时使用 Restart app。 |
| 重启按钮被拒绝 | 会话正在停止或纪要正在生成——等横幅消失后重试。 |
| http://127.0.0.1:8710 打不开 | 终端窗口被关闭了。重新双击 Forum Agent.command。 |
