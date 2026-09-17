# Vision Forum 实施进度与验证记录

更新：2026-09-16。分支：`codex/vision-forum-integration`。

本文件记录已经执行的代码工作；[主方案](VISION_FORUM_ENGINEERING_PLAN_ZH.md)、[协议](DATA_AND_PROTOCOL_ZH.md)和[任务表](IMPLEMENTATION_TASKS_ZH.md)仍定义最终目标。没有验证证据的项目不标为通过。

## 按四个部分推进

| 顺序 | 范围 | 交付与出口 | 当前进度 |
|---|---|---|---|
| 第一步：建立整合基础 | F00–F01 | 固定产品与源码基线；桌面、模型、Python 进程和打包可行性验证 | 本机开发完成；跨设备及人工共存验收延期 |
| 第二步：做可靠的实时双语字幕 | F02–F04 | 原文、译文、录音与会议关联；停止不丢最后一句；重启可恢复 | F02–F04 工程和 M0 本机联调完成；质量及实机验收待完成 |
| 第三步：做单场论坛产品 | F05–F08 | 分析 worker、模型调度、统一操作台、审核洞察、会议库、纪要与报告 | 工程实现及本机 8B 完整链路完成；长会性能、语义质量和真实设备验收待完成 |
| 第四步：完成 Forum 版 | F09–F12 | 双轨与说话人、双会场、公开查询、正式安装、现场排练 | 待实施 |

2026-09-16 用户更新执行顺序：“我暂时测试不了第一部分。开始第二部分开发。”据此延期 F01 的第二台 Mac、手机和原 Translator 人工共存验收，开始 F02→F03→F04；延期不等于验收通过。保留逐部分确认约定：第二部分完成后汇报，等待用户确认再开始第三部分。用户可用设备为 M5 Max、iPhone 和 Android，实际测试日期待定。

随后用户明确授权：“Git push, then start part 3”。已将第二部分 `73f7bb1` 与 `3969fcb` 推送到 origin，再完成 F05–F08 工程实现。用户随后要求 git push，第三部分按 core、worker/runtime、桌面集成三个提交组织交付；第四部分尚未开始；仍保留完成一部分后汇报、确认后再推进下一部分的约定。

## 第三步已落地的内容：F05–F08

新分析 worker 使用本机 Qwen3 8B，core 固定输入快照并验证引用/版本/覆盖。持久任务、含排队 deadline、有限重试、实时优先、单后台、确认 checkpoint、同场停止后自动纪要意图及退出资源确认已接入。界面提供会议库、洞察、纪要/所选场次报告、引用定位、编辑审核、独立公开内容与只读同机大屏；支持三格式导出和旧 JSONL 导入。

真实 Rust→随任务授予的本机模型→core 链路已用合成文本跑通；纪要草稿不公开，报告只读取所选场次已发布内容，上游隐藏会撤回大屏并失效依赖报告。长文本真实 tokenizer 的 13 块连续覆盖验证通过。开发 `.app` 已重建并通过随包校验和本机签名验证。

这仍不是 F05–F08 正式产品门槛全通过：模型可能把“尚未批准”错误归为 decision，也可能漏提细节；90 分钟完整纪要 180 秒、实时并发时延、32B 真实抢占和设备体验仍待测试。完整实现、复现和证据见 [第三部分本机验证](F05_F08_LOCAL_VALIDATION_ZH.md)。

## 第一步已落地的内容

### F00：验收和产品基线完成

- 新增 `profiles/ai-vision-forum/{profile,product}.json`：构建身份与运行策略分开；Forum 独立名称、bundle ID、数据目录；关闭商业登录和更新源。
- `acceptance-profile.json` 将 C1–C10 映射到实现阶段和验收证据，保留 D01–D07 未决项。所有产品验收仍为 `not_evaluated`。
- [ADR 0003](../adr/0003-forum-integration-baseline.md) 记录源码、机器、工具链和验收口径。不将 p95 自动替代原 SPEC 的 `<3s`，不以单机测试替代双会场验收。
- `scripts/validate_forum_profile.py` 检查跨文件配置；负例验证防止错误更新源、商业门槛或降低验收门槛被接受。

### F01：受控导入与桌面适配

- 通过 `scripts/import_hen_baseline.py` 从 Hen 固定 Git commit `49042697f11bc86dfa8e6bd10ef0dc07cffbaf84` 读取 85 个 Git blob，导入 `desktop/`。原 Hen 工作区保持不变。
- [来源清单](hen-import-manifest.json) 保存原路径、blob、SHA-256 和文件权限；[第三方说明](../../THIRD_PARTY_NOTICES.md)保留许可证和依赖来源。清单描述导入前内容，Forum 修改由 Git 记录。
- 导入 Tauri/Svelte 壳、音频桥接、ASR/翻译节点、模型下载器与 macOS 脚本；移除账号、激活/支付、旧更新源、VoiceLab 及其 IPC/前端入口。
- Tauri 与前端读取同一个 `product.json` 身份；构建时检查名称、ID、版本一致性。F01 保留翻译控制/字幕浮窗；第二部分现已连接新的会议数据库。
- 修复固定 OminiX-MLX 版本与 ASR macOS adapter 之间的两处错误类型转换，ASR 与翻译节点已能编译。
- Dora 选择器只接受原生 arm64 `dora-cli 0.4.1`；不误用本机 PATH 中的 Conda/Python 启动器。旧 Python `dora-rs 0.4.0` 与本次 Rust 节点不是同一个运行依赖。
- 显式打包音频动态库，增加应用包内资源探测；添加 Swift runtime rpath。Metal probe 需要把 `mlx.metallib` 放到实际可执行文件旁。
- 预检脚本只检查文件，不再仅凭配置文件存在就修改共享模型目录的“下载完成”标记。模型初始化脚本统一使用 Rust 下载器。
- 模型选择统一用于 readiness 和实际数据流；完整旧缓存只读复用，新下载固定到 `~/Library/Application Support/AI Vision Forum/models`。下载器在任何 repair/网络前拒绝外部目录、路径逃逸和符号链接；安装锁绑定模型根。25 项下载器测试通过，没有修改本机已有模型。

开发 `.app` 已在本机成功构建，路径 `desktop/dist/AI Vision Forum.app`；跨设备测试目录为 `desktop/dist/Forum-F01-Mac-Test-20260916/`。bundle ID 为 `org.aivisionforum.agent`，资源齐全，arm64 与 Swift rpath 检查通过；`codesign --verify --deep --strict` 通过的是本地 ad-hoc 签名。未启动应用，未进行干净 Mac、正式签名或公证验收。实际 Tauri hook 的工作目录是 `ui/`，构建命令已按实测修正。

前端 `svelte-check` 零错误/警告，11 项 UI 回归通过，Vite 构建通过。桌面 Rust 单测本次 36 项通过、1 项需要人工观察电源状态而忽略；Dora bridge 本次 38 项单测通过。这些测试不启动真实采音。

### F01：本机模型文件测试

开发机为 Apple M4 Pro、48 GiB、macOS 27.0，Rust 1.92.0。本轮没有打开麦克风、采集系统声音或下载新权重。

ASR 使用现有 Qwen3-ASR-1.7B-8bit，OminiX-MLX 固定源码 revision `6aac996db8b71fb7dae7a2409c46b4f2ade93092`。用 macOS TTS 生成中、英两句录音，再拼接为混合录音；这些是合成样例，不是真实会议评测集。

| 样例 | 音频长度 | 文件推理耗时 | 观察 |
|---|---:|---:|---|
| 中文 | 4,996 ms | 1,774 ms | 中文和“二零二六”保留 |
| 英文 | 5,111 ms | 753 ms | 英文和“twenty twenty six”保留 |
| 中英拼接 | 10,107 ms | 1,285 ms | 中英文均输出；未验证句内混说泛化能力 |

该进程模型加载约 1,360 ms。语言提示显式指定 Chinese/English，`detected_language` 为 null。固定绑定使用强制 language prefix；不能把混合样例成功解释成自动语种识别已经实现。还需句内混说、多人交替、噪声和真实语速测试，决定自动双语 adapter。

翻译使用现有 Qwen3.5-2B-MLX-4bit，直接调用生产 `backend_mlx` worker。三例包括“今天不会发布”、12 个样本、下午 3 点，以及 Friday/API 的混合文本。三个输出在人工检查中均保留主要含义、否定与数字。

| 翻译样例 | 首个回调 | 最终结果 |
|---|---:|---:|
| 中→英 | 202 ms | 428 ms |
| 英→中 | 172 ms | 354 ms |
| 混合→英 | 167 ms | 370 ms |

这是 debug 构建的第二次进程运行，模型加载约 1,287 ms；关闭 worker warmup，但 OS/Metal 缓存可能已热。首次运行中文首回调约 1,017 ms、最终约 1,269 ms。以上均不含采音、VAD、ASR、排队、UI 展示或审核，因此不是 C1 实时延迟验收结果，也不是大样本质量结论。

源码入口：`desktop/node-hub/dora-qwen3-asr/examples/forum_asr_probe.rs`、`desktop/node-hub/dora-qwen35-translator/examples/forum_translation_probe.rs`。本地输入、输出与构建日志在 Git 忽略的 `artifacts/local/` 下。已保存[本地模型文件指纹](f01-local-model-fingerprints.json)，远端 revision 未核实，不将其当作发行下载清单。

### F01：Python 进程边界

`services/meeting-worker/` 为独立 Python 3.12 包，不导入原 `server.py/session.py`。实现 JSON-RPC 2.0 / NDJSON 的 initialize、health.ping、shutdown；stdout 只输出协议，stderr 输出诊断。

14 项真实子进程测试覆盖握手、版本拒绝、非法 JSON/ID、重复初始化、通知、超长/未闭合/超时帧及正常退出。wheel 已离线构建并装入临时独立环境，从工作区外启动成功。

当前 capabilities 明确为空：`jobs.run/jobs.cancel` 返回未实现。后续补充至 24 项 worker/打包测试通过。

独立运行时已随 `.app` 打包：固定 Astral CPython 3.12.11、macOS 14+ arm64 wheels、MLX 0.32.2 与 mlx-lm 0.31.3，包含完整 URL/hash 清单及许可证，不复制 `.venv`。两个离线构建的 7,051 个文件/链接逐项一致。包内 checker 用标准库读 Mach-O，无需 Xcode/otool；环境隔离下真实 Metal 小矩阵与协议退出通过，显式诊断用已有 Qwen3-8B 完成推理。最终资源候选的真实生成约 3.405 秒，stdout 纯协议。

详见 [worker 本机证据](../../services/meeting-worker/packaging/LOCAL_VALIDATION.md)。实际桌面 `.app` 已完成构建、签名后默认 checker 和 `codesign --verify --deep --strict` 检查；这是本机 ad-hoc 开发包，第二台 Mac 尚待实测。

### F01：模型语言、并发与取消决策

新增 7 条合成文件（中、英、两种句内混说、交替发言、短句、静音），完成 Qwen 不同 hint、Whisper auto、临时 Qwen 原生 auto 变体对照。F01 当时继续使用显式语种并选 Whisper 为自动模式候选；本轮 F03 已集成自动识别和独立运行包。8B 的合法 JSON 出现虚构行动项，后续不能跳过证据校验和人工审核。

三进程并发均可执行，但中位耗时明显增加：ASR 639→986 ms、2B 299→509 ms、8B 7,530→11,625 ms。独立进程取消后退出并可重启；这不等于已实现生产任务调度/合作取消。详见[模型实验报告](F01_MODEL_FINDINGS_ZH.md)与 [ADR 0004](../adr/0004-f01-runtime-and-model-decisions.md)。没有开启麦克风、系统采音或下载新模型权重，未修改已有模型目录。

### F01：独立 Dora 与直接持有的子进程

此前严格 CLI-ACK 测试仍保留失败证据：官方 Dora 0.4.1 纯动态图即使 node/event stream 已 Drop，也可能不返回 `dora stop` ACK。另确认官方 spawner 将普通 executable 放入独立 PGID，只杀 daemon 组不能回收模型。

现实现每实例独占 0700 目录、原生 arm64 CLI 校验、独占 loopback C/D 端口与 Zenoh 禁止自动发现。私有图把 executable 注册为 dynamic；桌面直接持有 ASR/翻译 Child，传入核验过 UUID/node ID/daemon PID/event port 的 NodeConfig。默认端口和共享 Dora 网络不参与启动、查询或停止。不支持的节点 hooks/args 等显式拒绝。

CLI 命令与进程/线程退出均有等待期限。CLI ACK 与实际回收分别记录；未 ACK 但全部自有资源退出为 `StoppedByOwnedRuntimeFallback`。停止失败保留所有权和 worker handle，不提前发 Stopped。UI 的过渡状态提交与事件消费顺序已加锁，避免慢保存覆盖完成回执；单槽最新状态保证暂停轮询后仍能收到最终状态。

真实生产 controller + TranslationListener + 本地合成 executable 的双实例探针已通过：

- A 约 3,129 ms 完成回收后，B 仍继续从 36 增至 41 条字幕。
- B 的自有 daemon 被 SIGKILL，且合成节点忽略 TERM；约 3,078 ms 仍完成自有节点回收。
- C 的直接节点被 SIGKILL，`get_status` 报告子进程退出，随后完整回收。
- 9 个自有 coordinator/daemon/node PID 消失、18 个原监听端口关闭，生产 listener 已 join。各次 `acknowledged=false`、`contained=true`，没有把超时伪装为优雅 ACK。

证据：`artifacts/local/f01-owned-controller/9a1724fb-9521-480a-af47-cce571e102cb/report.json`，原始日志随证据保留；复现见[Dora probe 说明](../../desktop/moxin-dora-bridge/examples/README.md)。它不替代真实采音尾句、90 分钟会议、桌面宿主被强杀后恢复或原 Translator 应用的人工共存验收。

### F01：真实模型二进制的无麦克风串联

补充使用 production controller + 实际 Qwen ASR/2B binary + production TranslationListener，单次发送已有合成中文 PCM。等待模型 ready 后，收到原文“我们还没有批准这项预算，请在星期五下午三点前确认十二位嘉宾的名单。”，译文完整保留未批准、十二人及星期五下午三点。停止约 3,104 ms，`acknowledged=false/contained=true`；8 个自有模型流/peer PID 均退出，listener 已 join，独立 peer 在模型流关闭后从 68 继续到 71 条。

首轮在 `start()` 返回后立即发送的帧未到达 ASR，失败证据保留；第二轮由探针等待两个模型日志 ready 后仅发送一次成功。`start()` 返回约 610 ms，模型 ready 约 2,016 ms；ready 后接收最终原译文约 1,598 ms。探针日志门控不是生产启动协议，不能把控制器成功返回当作采音就绪，更不能把这次文件结果称为 C1 实时达标。该 F01 失败促成 F03 的生产就绪屏障和音频确认/重放；当前已实现并在最终 Runtime 回放验证。

复现入口 `desktop/moxin-dora-bridge/examples/forum_model_binary_probe.rs`，完整证据位置见[模型报告第 8 节](F01_MODEL_FINDINGS_ZH.md)。没有打开音频设备或修改现有权重。

### F01：跨设备测试准备

已经准备[设备验收表](F01_DEVICE_TEST_ZH.md)、包内独立验证器及两个 `.command` 入口。HTTPS 探针仅提供合成双语句子，显式私有地址、限时配对、可信 TLS、cookie 和绝对请求期限；5 项本机 TLS 测试通过，包括慢请求无法延长服务期限、私钥失败清理、无令牌拒绝。

**第二台 M5 Max、iPhone、Android 的实际结果仍待用户提供，现按用户要求延期。** 只收到配对请求不算手机画面/信任通过；本机环境隔离也不能替代干净机器。第二部分开发已获单独授权。

## 第二步已落地的内容：F02–F04

核心与生产节点已接入桌面：版本 4 SQLite 单写 actor、私有 UDS/producer outbox、源与译文完整关联、ASR ready 屏障、录音 journal、停止/重放封存、Whisper 自动识别、Qwen3.5-2B 双目标翻译及按音频段边界切换语向。界面提供录音选项、本机会议记录、分页与恢复入口；完整 Markdown 导出不受 200 行浮窗上限影响。

新增测试已经覆盖迟到/重复事件、修订和方向隔离、未确认尾段、已封存但识别失败的会议重试、超过 1,000 段的恢复分页、重复停止、队列满下停采和播报身份去重。真实 ASR 文件探针在完全没有 Translator 的情况下保存四个终态，并验证 ACK 丢失重放和进程回收。完整桌面 Runtime 最终回放已通过：七份合成音频全部保存终态、两目标 coverage 无缺口，随后同一实例成功恢复第二场短句；两次停止均无待处理项。首轮 Dora 连接过早关闭和第二轮默认队列覆盖的失败证据保留，修复后已重验。模型质量审核仍保留 API→IP、它→他 等偏差，不能据持久化完整推断语义全对。

详见 [第二部分实现与本机验证](F02_F04_LOCAL_VALIDATION_ZH.md)。第一部分章节描述的是当时的基线，不再代表当前接口：当前桌面已经连接会议数据库，Whisper adapter 已进入独立运行包；会议分析 worker 的正式 jobs 仍未实现。

## 下一步顺序与剩余门槛

1. 第二部分已交付 M0 开发候选及本机验证记录；真实设备、长会和语义质量门槛保持待验收。
2. 第二部分完成后汇报实际验证与限制，等待用户确认再进入 F05–F08。
3. 延期验收：在 M5 Max 完成独立 Python/Metal、桌面窗口、重开及原 Translator 共存；在 Mac 与手机完成 LAN TLS、配对、无令牌拒绝与停服测试。回收报告并修复，正式发行前仍须通过。

F05 之后按任务表继续。F12 未通过之前，不新建 Minutes 产品仓库，不声称 Forum 完成。

## 可复现验证入口

从仓库根目录执行（需要已准备的工具链/依赖）：

```bash
python3 scripts/validate_forum_profile.py
python3 scripts/validate_forum_profile.py --self-test
.venv/bin/python -m unittest discover -s services/meeting-worker/tests -v
npm --prefix desktop/forum-shell/ui run check
npm --prefix desktop/forum-shell/ui run test:p0
npm --prefix desktop/forum-shell/ui run build
```

Rust 从 `desktop/` 目录执行，以使用其固定工具链：

```bash
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo test --locked -p forum-contracts -p forum-core
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo test --locked -p moxin-dora-bridge --lib
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo test --locked -p forum-shell
```

上述是无麦克风测试。模型 probe 和应用打包单独执行，避免普通回归隐式加载模型。开发 .app 不是已通过验收的现场发行版。
