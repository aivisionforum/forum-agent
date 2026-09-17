# 第四部分：实现与本机验证

2026-09-16，`codex/vision-forum-integration`。本记录区分工程交付与现场验收；用户已授权开始第四部分。没有启动用户的会议数据库或真实音频采集。

最终本机测试、双会场真实 8B 链路与开发包摘要见 [不含会议原文的验证汇总](evidence/f09-f12-local-20260916.json)。安装包及模型准备步骤见 [发行说明](PACKAGING_RELEASE_ZH.md#2026-09-16-本机最终开发候选)。本记录随第四部分实现提交，具体提交和推送结果以 Git 历史为准；正式产品验收状态仍为未评估。

## F09：双轨与匿名说话人

麦克风走 CPAL，系统音频走 ScreenCaptureKit，分别持有设备、录音日志与 VAD。设备时间戳映射到同一个 16 kHz 时钟；连续重采样避免按回调取整的累计偏差。双轨在同一个停止时间裁剪，两路设备释放后才发送联合 capture seal。丢失的回调/尾部记为不可恢复 gap，不能拿合成静音掩盖。

一个 ASR 实例按音轨轮流出队；当前段的指定 revision 持久确认后才能发下一段。原始连续 PCM 保留在各轨日志，提供给 ASR 的参考回声处理后段另存，恢复使用精确的段文件。旧单轨存储仍可恢复；新双轨目录为 `sessions/<session>/audio/tracks/<track>/`。

最终回归发现立即重开日志时，fork/dup 继承的文件描述符可能延长旧 `flock` 所有权。生产日志锁现与 core 一样由实际持有进程显式释放；子进程拷贝的析构只关闭自己的描述符，不释放仍活跃的父进程锁。真实 dup/fork 回归验证了重开和互斥，未靠睡眠掩盖时序。

当前回声处理是保守的线性参考抵消：只有麦克风与系统参考高度相关时减去估计回声，无法确定时保留声音。不按文字相似性删除句子。这尚未证明扬声器多径、双人插话、蓝牙切换或真实 90 分钟表现；不能称为已通过完整声学 AEC 验收。

ECAPA 独立 worker 固定架构，离线加载授权的本地权重，返回 192 维向量。core 只管理场次内匿名 ID，来源 revision 变化或人工修订会阻止迟到自动结果覆盖。过短、低能量、边界不确定时保持 unknown。overlap 支持人工标记；ECAPA 本身不是自动重叠检测器。聚类阈值是开发默认值，未用真人多人样本校准。

已执行：

- 音频桥 58 项测试通过，覆盖双轨联合封存、独立段身份、原始/处理后 PCM、缺失回调、轮流调度、恢复与停止裁剪。90 分钟项采用加速合成时间戳，验证时钟算法，不是运行 90 分钟的硬件排练。
- `dual_session_probe` 走真实桌面 runtime → Dora → Whisper 自动 ASR → Qwen3.5 翻译 → core；两轮每轮两轨，14.747 秒 / 11.341 秒完成恢复，两轮均 `incomplete=false`、`translation_pending=0`，没有打开音频设备。
- 第二轮把同一句真实重复表达放到两轨，两个独立来源均保留。短探针只验证数据关联和恢复，不能代表回声抑制或会议语义质量。
- 独立安装的 ECAPA runtime 对 3 秒合成波形实际输出 192 维向量，3.719 秒，推理中 ping 0.292 毫秒，缓存未改；22 项 worker 测试通过；真实 Manager → 安装版 ECAPA → core 在整组退出补强后以 3.308 秒完成，退出确认且缓存未改。详见 `services/speaker-worker/packaging/LOCAL_VALIDATION.md`。

本机原始证据（不提交）：`artifacts/local/f09-dual-desktop-runtime/4ea4f3e2-5652-4e1b-af3e-4884c26142b7/report.json`、`artifacts/local/f09-ecapa-installed-probe-2/report.json`。这些合成测试不构成 C3 正式验收。

## F10：双会场与公开内容

core schema v6 保存独立的说话人修订、peer 公共投影与选定版本的来源关系。配对保持 owner/event/session 边界；增量按 cursor 幂等处理，缺口退回快照，撤回水位防止旧内容复活。断线将 peer 标为 stale；重连后才能再次选入报告。对已公开版本的撤回或修订递归使依赖报告失效。

LAN 默认关闭。操作员生成本机 CA/服务端证书，显式启用 HTTPS；证书可保留，访问凭证不在重启后自动恢复。peer 一次性邀请同时验证 CA、主机名、有效期和证书指纹。参会者凭证仅授权一个场次的公开资料，不能读数据库、原文或录音，也没有录音控制路由。二维码使用 URL fragment 传递限时凭证。

手机页面可浏览和检索公开正文/审核后的证据。跨场报告和闭幕稿从操作员勾选的确切公开版本生成，来源版本变化会清除旧选择。主持人问题仍是私有资料。加入活动仅影响本机随后创建的场次，不修改历史归属。

闭幕朗读读取独立审核发布的 `PublicArtifact.text`，不读取模型草稿。采集进行中拒绝朗读；开始采集前等待朗读输出释放。公开版本失效/撤回后停止朗读。播放器代次隔离、合成子进程回收与停止确认已有无设备测试，实际扬声器取消和声学回灌仍需现场验证。

已执行 82 项 core + 4 项 contracts 测试，含新版 peer/说话人/选定来源边界；TLS socket 测试及两个独立临时数据库的 host manager 同步测试通过。手机证书安装、会场 Wi-Fi 客户端隔离和两台物理机器尚未测试。

收尾已修正 worker 遗留单场限制：报告和闭幕稿统一接受 1–100 个明确场次的公开快照，验证每条输入的场次子集与全场覆盖；原文任务仍限定单场。活动面板同时提供精确版本的报告/闭幕入口；朗读只列实际支持的中英文，需事先安装对应 Apple 音色。

真实 Qwen3 8B 探针使用两个独立临时数据库/owner：分别生成纪要并审核公开，将对端公开投影导入主机，再由生产 AnalysisManager 生成两场报告和闭幕稿。第二轮 107.488 秒完成；两任务均 `succeeded`、覆盖完整，报告 `valid`，闭幕稿因两条无引用提问为 `needs_review`。已实际验证该闭幕稿不能批准或发布；远端撤回后两份成果均失效，旧公开版本无法重新提交，所有自有 manager/core 正常退出。模型语义质量尚未认证，此时间也不作为长会性能指标。

证据为 `artifacts/local/f10-event-peer-host-probe/run-2.json`。首轮同样成功生成两类结果，但探针过严地要求每个结果都 `valid`，在正确的 `needs_review` 状态提前结束；失败记录为同目录 `run-1.json`。第二轮按产品规则验证私有草稿与发布阻拦，没有放宽生产审核规则。此探针直接使用 core 的 peer 投影接口，真实 TLS 另由上面的网关/manager 测试覆盖，不能称为物理双机联调。

## F11：可安装开发包

主构建包含 ASR/翻译、会议分析和独立声纹 worker。ASR 与会议分析复用已锁定的 Python/MLX 环境、以独立进程运行；声纹使用独立 Python/PyTorch 环境，避免版本冲突。权重不随包分发。

Forum 可靠采集路径不使用遗留 `libAudioCapture.dylib`；该二进制实际最低 macOS 15，已从新的 macOS 14 目标包资源移除，导入来源仍保留。构建校验真实 Mach-O 最低系统与本地动态库引用，不只看 Info.plist。

构建、开发/正式签名、公证和模型准备见 [发行说明](PACKAGING_RELEASE_ZH.md)。本机开发签名不等于 Developer ID、公证或干净 Mac 验收；更新源继续关闭，不继承 Hen Translator 的 feed 或密钥。

最终 `.app` 与第五轮回归的 276 份源码摘要逐项相符，两个随包 Python 环境、宿主完整性及 `codesign --verify --deep --strict` 均通过。使用最终包内 ECAPA 再次离线处理 3 秒合成波形，输出 192 维归一化向量，3.614 秒，推理中 ping 0.327 毫秒，已有模型缓存未改。证据为 `artifacts/local/f11-final-build-source-check.json`、`artifacts/local/f11-part4-final-app-ecapa-probe/report.json`；不将这些合成输入结果当作说话人准确率。

## F12：回归与待执行排练

`scripts/run_forum_regression.py` 执行无模型契约/数据库/runtime/网关/worker/桌面/UI 回归，记录每条命令、退出码、耗时、日志 SHA256、源码 SHA256 和提交。运行中源码改变则整次记录不能标记稳定通过。CI 执行 portable 子集；本机实际执行结果另行记录，不能将未运行的远程 CI 称为通过。

最终本机回归记录为 `artifacts/local/f09-f12-regression-20260916-5/report.json`：11 个阶段全部通过，`regression_passed=true`、`source_changed_during_run=false`。合计 357 项测试通过，2 项显式忽略（人工电源状态测试与默认关闭的真实 ECAPA 探针；后者已另行执行）。Svelte 检查零错误/警告，三个前端入口构建成功，生成的契约与源码一致。明细为 contracts 4、core 82、gateway 9、runtime 15、音频桥 58、桌面 64、会议 worker 63、声纹 worker 22、安装/发行工具 16、UI 24。

此前记录均保留：第一轮因 UI 命令使用不存在的 `npm test`、且安装工具源码仍有修改而失败；第二轮旧候选通过。随后补齐真实多场 worker 与报告 UI，第三轮发现队列测试把前一命令完成误当成队列已空，修成按 `QueueFull` 有界重试；第四轮抓到上面的 producer 日志锁继承问题。没有跳过失败项或降低生产规则；第五轮在最终源码冻结后全部通过。GitHub Actions 已配置；本次本机取证不包含远程 CI 结果，CI 状态以对应提交的运行记录为准。

执行：

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target python3.12 scripts/run_forum_regression.py \
  --output /absolute/new/forum-regression
```

正式步骤与 C1–C10 记录表见 [双机排练手册](FORUM_REHEARSAL_ZH.md)。仍需真实两台 Mac 并行 90 分钟的受控回放与真实会议两次排练、iOS/Android HTTPS、声学与质量盲评、长会纪要时限及签名/安装验收。D01–D07 未决口径保留；当前不能标记稳定 Forum release，也不开始 Minutes 派生。
