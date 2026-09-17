# 整合桌面版操作指南（F09–F12 开发版本）

本节适用于 **AI Vision Forum.app**。下面保留的 Python/8710 指南是旧版流程。当前是本机工程测试版本，跨设备、90 分钟、正式签名与质量验收见 [进度](engineering/PROGRESS_ZH.md)；开发包安装及模型准备见 [发行说明](engineering/PACKAGING_RELEASE_ZH.md)。

## 会前

1. 安装 Forum app，准备本地模型。固定语言使用 Qwen ASR；自动模式使用 Whisper，翻译使用本地 Qwen3.5，会议分析用 Qwen3 8B；匿名标签可另启 ECAPA。未准备的组件会显示不可用，不会在推理时偷偷下载模型。
2. 选择源语言、目标语言和输入。混合线上会议选“麦克风 + 系统音频”，授予麦克风和系统音频权限；双轨使用系统当前默认麦克风。切换设备或双轨语向前先停止采集。检查录音选项；正式录音/匿名逐字稿验收要求全程开启录音。
3. 两会场各用一台 Mac。新装机的共享面板要选中已有场次才显示：会前先在两机各建立并停止一个明确标为“联调”的测试场次。第一台在共享面板填写当前 LAN IP、生成证书并启用 HTTPS，发出该测试场次的邀请；第二台粘贴邀请、核对第一台指纹后配对，再在完全停止采集时点“将后续场次加入此活动”。这只影响第二台之后新建的场次，不改变测试场次或历史会议归属。
4. 两机随后各自创建正式场次。公开会场名/标题由操作员单独填写，检查不含需隐去的身份信息。邀请只授权一个场次：正式场次需各自启用共享并重新发邀请，不能沿用测试场次凭证；两机互看需要双向配对。生成跨场内容时不要勾选联调资料。
5. 参会者扫码前，按面板说明向手机安装/信任对应 `ca.cer`；同一网络须允许设备互访。证书操作见 [网关手册](../desktop/crates/forum-gateway/README.md)。不跳过证书警告。

## 会中

开始采集后，原文、译文与音轨/片段关联持续保存在本机。双轨轮流交给一个 ASR；队列过长或设备失败时显示问题并保留可恢复音频。双轨回声处理尚需真实扬声器场景校准。

在原文页展开“匿名说话人标签”，可输入已准备的 ECAPA 模型绝对目录并启用当前场次分析，留空使用应用管理目录。标签可能稍后补齐；unknown 不是识别到一位叫 Unknown 的人。可对选中段标记未知、重叠或本场匿名 ID，填写修订依据。标签不代表实名，也不自动隐去说话内容里的人名。

洞察/纪要/报告均先产生私有草稿。核对引用、编辑、审核，再填写用于公开显示的标题、正文与证据摘录。只读大屏和二维码入口只看公开投影；模型原文不能直接上墙。主持人问题始终用于私有工作区。

跨场面板显示最后同步时间；断线为 stale，本地采集仍可继续。对方隐藏或修订来源后，依赖报告会失效，需要重新选择、生成并审核。共享面板的“已发出的访问权限”可撤销旧二维码、邀请和 peer 访问。

## 停止、纪要与闭幕

1. 点击停止，等待“音频设备已释放”和持久保存结果。仍有缺失原文/译文时按会议库的恢复提示处理；录音关闭的段不能凭空恢复声纹或音频。
2. 停止后纪要进入任务队列，查看实际完成与覆盖状态；有错误或部分草稿时不能当作完整纪要。现场长会的 180 秒候选时限尚未验收。
3. 在“活动与会场共享”展开活动公开资料，勾选在线、已发布的确切版本（包含当前本机场次），点击“生成跨会场报告”或“生成闭幕综述”。新版本不会悄悄替换旧选择；生成结果仍需人工审核并发布。下方“生成会议成果”的报告入口用于按本机场次选择，跨机资料请使用活动面板。
4. 已发布闭幕稿可先选择中文或英语，再朗读公开正文；采集进行中按钮不可用。会前在 macOS 辅助功能中安装对应 Apple 音色；默认中文使用 `Yue (Premium)`（`zh_CN`），英语使用 `Voice 1`（`en_US`）。朗读沿用“实时控制 → 译文播报”保存的音色编号和输出设备；中文仅有 1、2 号，请在停止采集时选择中文目标语言并确认可用音色。未安装的音色会明确拒绝朗读。停止朗读、撤回公开稿或开始新采集会取消输出；新采集等待朗读设备释放。实际音响回灌还需排练验证。
5. 关闭窗口不等于退出。退出时应用等待自有音频、模型和网络资源收尾；如果提示仍有资源未退出，保留错误信息，不把“请求停止”当作完成。

模型/网络异常先保存错误与场次 ID；不要删除录音或数据库来“修复”状态。开发包不要覆盖正在运行的 app，升级应安排在会议全部停止后。真实两机/手机的逐步排练见 [排练手册](engineering/FORUM_REHEARSAL_ZH.md)。

---

# 旧 Python 版本操作指南（保留参考）

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
