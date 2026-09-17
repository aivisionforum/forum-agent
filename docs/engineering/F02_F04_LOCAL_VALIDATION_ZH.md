# 第二部分：可靠原文、双语字幕与恢复

状态：F02–F04 工程实现及 M0 本机文件联调完成，正式质量与现场验收尚未通过。

范围为 F02–F04。第一部分的第二台 Mac、手机和人工共存验收，按用户指示延期；本部分不包含 F05 会议分析任务、纪要生成、公共手机页面或双轨说话人处理。

## 已实现的代码路径

1. 桌面 `forum-shell/src/meeting.rs` 创建 session/track、0700 会议目录、0600 元数据和本场私有 Unix socket。`forum-core` 有界 actor 是 SQLite 的唯一写入方；版本 4 迁移前备份，拒绝较新数据库和重复进程写入。Producer 在本机 outbox 写入、fsync，再发给 core；确认超时保留原 message ID 重试。
2. `reliable_capture.rs` 给 PCM 设置采样时钟和稳定段 ID。处理后的 16 kHz PCM、闭合段、SHA-256、gap、方向边界和 capture seal 写入录音 journal；闭合段登记 ACK 之后才发给 ASR。音频设备回调使用有界缓冲，不直接等待数据库或模型；溢出形成明确缺口。
3. 固定语种走现有 Rust Qwen3 ASR；`auto` 走独立 Python/MLX Whisper large-v3-turbo 进程。模型加载完成才发布 ready，只有 ASR ready 才允许开始采音。ASR final 先进入自己的 outbox 和 core，再由翻译器消费持久 coverage，避免译文故障同时丢失原文。
4. 翻译器使用 Qwen3.5-2B-MLX-4bit。ASR 与翻译器在 ready 前对实际配置、tokenizer、Jinja 模板和 safetensors 计算只读文件指纹；环境中的 manifest ID 只作为预期比对，不能覆盖实际身份。正式下载 registry 留待后续。每条请求保存原文 revision、UTF-8 spans、输入重建规则、目标语种、方向 epoch、revision/attempt；旧版本结果不能覆盖新版本。中文和英文模式为两个目标；同语种内容有显式 passthrough，混说的汉字/拉丁字母检查防止 dominant language 将混合文本误当纯单语直接展示。
5. 现场交换中英语向在音频段边界生效：先关闭并登记旧段，再持久提交方向边界，最后激活新配置。历史原文仍按原方向处理。界面在 ACK 前显示等待且禁用重复交换；自动识别和双目标不使用交换按钮。
6. 采音的 delivery 队列最多保存八个已登记段，同一时间只向 Dora 发送一个待确认段；现场循环持续收音并非阻塞检查 ACK，回放逐段等待确切 revision 的终态。显式设置 Dora 输入队列大小为 4，队列不代替 durable ACK。Stop 先发采音停止标志、实际释放设备，再登记最后一段和 capture seal。Dora 数据通道保留到处理完毕；source seal 要求已登记段逐项有终态和 ASR producer seal。30 秒 drain 期限后的未完成工作保留在录音/outbox/coverage，界面如实显示待恢复，不伪装全部完成。
7. 恢复入口只读取所选会议保存的设置和录音，不打开麦克风/系统音频。重放保存的原事件；缺失或 failed 原文使用新 revision。一次恢复有固定待发送 revision 集合和 dispatch-complete 栅栏，防止重放途中先封存；已封存但含失败段的会议仍要等待新 revision 和 producer ACK。旧进程只在 journal 身份、旧 outbox 重放与 OS 确认其 PID 已消失后对账，不依据 PID 猜测或杀共享进程。
8. 显式恢复为已耗尽翻译请求给予额外三次额度；额度按用户此次恢复 action UUID 持久化，同一 action 的进程重启不会无限续额。未完成请求采用 keyset 分页，不会被前 100 条旧失败任务挡住后续任务。

## 桌面界面和数据

新增自动识别、中文和英文双目标、录音开关、“本机会议记录”和“恢复未完成内容”。记录按固定 snapshot cursor 每页 100 段显示原文、译文与失败/等待状态；实时浮窗只投影最近 200 段，完整数据留在 SQLite。Markdown 导出从完整存储读取，包括 empty/failed/missing 和 gap。播报按 translation ID/revision/attempt 去重，迟到结果不依靠“最后 N 行”判断。

TS 类型与 JSON Schema 从 Rust 同源生成，Python 用同一 schema 版本校验。独立运行包携带 schema/hash。已检查浏览器预览排版；预览明确标识合成数据，不启动真实音频或模型。

## 本机验证记录

自动化与文件模型测试的报告位于被 Git 忽略的 `artifacts/local/`。测试使用既有合成音频，没有打开用户麦克风/系统采音，没有下载新的模型权重。

- Core/contracts：41 + 3 项通过；包含事务失败、跨 session 引用、重复/冲突事件、队列满和 ACK_UNKNOWN、迁移/所有权、封存与旧 run 对账、UTF-8/attempt/epoch、快照历史与方向边界恢复。
- Translator：42 项通过；新 outbox 的 12 项队列兼容回归通过。真实 2B 模型取消后可以继续下一次翻译。
- Bridge：50 项通过；runtime：5 项通过；ASR seal：2 项通过。
- Shell：45 项通过、1 项忽略，包含历史封存失败段恢复、新 revision/producer ACK 栅栏和播报身份去重。
- Python：31 项 meeting-worker/打包/schema 测试及 6 项 ASR adapter 测试通过。
- UI：Svelte 类型检查 0 错误、0 警告；11 项现有 UI 测试通过。
- 无 Translator 的真实生产 ASR 探针：`artifacts/local/f03-durable-asr/a904f28e-d4f8-4c89-aecb-caf22b77d6a9/report.json`。四个终态（30 ms Empty，中文、英文、混说 Success），首帧 ready 屏障、首次 final ACK 丢失重放、outbox 清空和自有进程退出均验证。
- 独立 Whisper 包：`artifacts/local/f03-whisper-package-probe/report.json`。从临时目录、隔离 Python 路径运行，四样本和协议退出通过；缓存文件未变。混说个例“它”识别为“他”，保留偏差记录。
- 最终完整桌面 Runtime 回放：`artifacts/local/f04-desktop-runtime/96727416-76ef-4a91-a112-cf8a04554698/report.json`。第一场七份样本产生六个 success 和一个 empty，六段非空原文均有中英 coverage、无待处理项；同一 Runtime 再完成一场短句恢复，两场 Stop 都为 incomplete=false、translation_pending=0。这是实际 Rust host/core、Dora、Whisper、Qwen2B 与录音恢复路径，不是前端模拟。
- 最终开发包：`desktop/dist/AI Vision Forum.app`。Tauri 构建、独立 Python/MLX 打包、隔离运行 checker、Metal 小矩阵、协议退出和签名步骤完成；模型权重未进入安装包。
- 历史失败保持可追溯：首轮因采音 node 过早 Drop 终止下游；第二轮因 Dora 默认单条队列覆盖中间音频。最终报告来自保留连接、逐段 durable ACK 及有界 delivery 修复后的重新执行。
- 文件联调的完成标志不表示语义全对。最终审核仍发现混说样本的 API→IP，以及中文原文“它”→“他”；本轮英文为“It has not been reviewed yet.”，历史男性主语错误没有复现。另外，英文 on Friday 被译为“本周五”，存在轻微时间具体化。数字、否定句与短句的个例检查通过。详细逐 span 审核在 `artifacts/local/f04-acceptance-review.json`，不记作“七样本质量全通过”。

## 重现方式

在 `desktop/` 中使用稳定工具链和同一 target：

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --locked --offline -p forum-core -p forum-contracts -p forum-runtime -p forum-shell
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable run --locked --offline -p forum-core --example generate_contracts -- --check
```

前端检查从仓库根目录运行：

```sh
npm --prefix desktop/forum-shell/ui run check
npm --prefix desktop/forum-shell/ui run test:p0
npm --prefix desktop/forum-shell/ui run build
```

完整文件回放探针源码为 `desktop/forum-shell/examples/reliable_session_probe.rs`。先构建真实 ASR/Translator 和该 example；显式指定原生 Dora、私有 runtime 目录、两个节点目录和已有 Whisper 独立 runtime/model。从仓库根目录执行 example，读取已有七份合成 WAV；报告输出到 `artifacts/local/f04-desktop-runtime/`，临时数据库和录音位于该报告记录的 `/tmp/forum-runtime-probe-*`。不要将此程序当成麦克风测试。

`desktop/scripts/build_macos_app.sh --profile dev` 默认携带 `--with-asr` 独立 Python 包；正式模型仍由本机完整文件提供。包的 native checker 在隔离环境检查 Mach-O 依赖、模块导入、Metal 与协议。SciPy 上游 wheel 的三条无用 Homebrew RPATH 被精确移除并重新签名，RECORD 和构建清单记录变换前后 SHA。

## 尚不能据此宣称的结果

- 混说术语、中文指代及时间具体化偏差仍待改进；不使用规则将测试样本答案写回 ASR/翻译，也不覆盖已有原始识别证据。
- D01 的 `<3s` 统计口径、D04 的数值质量/盲评阈值尚未确定；少量合成样本不构成 C1、P95 或 90 分钟现场稳定性验收。
- 第二台干净 M5 Max、iPhone/Android、真实麦克风/系统音频权限及设备拔出，仍需要实际设备测试。CPU buffer/gap 故障测试不冒充拔线实测。
- Journal 保存的是处理后 16 kHz PCM，不是硬件原始声道。当前短批次持久写入和完整 manifest 重写需做长会磁盘压力验证；不得据短文件回放断言小时级性能。
- 关闭录音仍保存原文/译文，但尚未转成文本的 PCM 无法在进程退出后补回；已损坏或丢失的录音不会被伪造为空白成功。
- Whisper 的 detected language 是段级主语言，不是逐词 LID；真实多人/口音/重叠发言质量仍待评估。
- 当前包是本机 ad-hoc 开发包。自动模型的权重下载/离线分发、正式签名与公证、最终双机排练仍属于后续交付门槛。

完成本部分汇报后，等待用户确认再进入第三部分 F05–F08。
