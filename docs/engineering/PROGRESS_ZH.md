# Vision Forum 实施进度与验证记录

更新：2026-09-16。分支：`codex/vision-forum-integration`。

本文件记录已经执行的代码工作；[主方案](VISION_FORUM_ENGINEERING_PLAN_ZH.md)、[协议](DATA_AND_PROTOCOL_ZH.md)和[任务表](IMPLEMENTATION_TASKS_ZH.md)仍定义最终目标。没有验证证据的项目不标为通过。

## 按四个部分推进

| 顺序 | 范围 | 交付与出口 | 当前进度 |
|---|---|---|---|
| 第一步：建立整合基础 | F00–F01 | 固定产品与源码基线；桌面、模型、Python 进程和打包可行性验证 | F00 完成；F01 进行中 |
| 第二步：做可靠的实时双语字幕 | F02–F04 | 原文、译文、录音与会议关联；停止不丢最后一句；重启可恢复 | F02-a 存储基础完成；完整 F02 尚未通过 |
| 第三步：做单场论坛产品 | F05–F08 | 分析 worker、模型调度、统一操作台、审核洞察、会议库、纪要与报告 | 待实施；当前只有 worker 握手入口和导入的翻译 UI |
| 第四步：完成 Forum 版 | F09–F12 | 双轨与说话人、双会场、公开查询、正式安装、现场排练 | 待实施 |

依赖顺序以任务表为准。F01 的语言、运行时隔离、并发和安装验证没有全部结束，因此不开始大规模 UI 重写或声称 M0 字幕里程碑通过。F02 的无模型数据契约与数据库事务可以独立推进，这是原方案允许的并行工作。

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
- Tauri 与前端读取同一个 `product.json` 身份；构建时检查名称、ID、版本一致性。保留翻译控制/字幕浮窗，尚未连接新的会议数据库。
- 修复固定 OminiX-MLX 版本与 ASR macOS adapter 之间的两处错误类型转换，ASR 与翻译节点已能编译。
- Dora 选择器只接受原生 arm64 `dora-cli 0.4.1`；不误用本机 PATH 中的 Conda/Python 启动器。旧 Python `dora-rs 0.4.0` 与本次 Rust 节点不是同一个运行依赖。
- 显式打包音频动态库，增加应用包内资源探测；添加 Swift runtime rpath。Metal probe 需要把 `mlx.metallib` 放到实际可执行文件旁。
- 预检脚本只检查文件，不再仅凭配置文件存在就修改共享模型目录的“下载完成”标记。模型初始化脚本统一使用 Rust 下载器。
- 模型选择统一用于 readiness 和实际数据流；完整旧缓存只读复用，新下载固定到 `~/Library/Application Support/AI Vision Forum/models`。下载器在任何 repair/网络前拒绝外部目录、路径逃逸和符号链接；安装锁绑定模型根。25 项下载器测试通过，没有修改本机已有模型。

开发 `.app` 已在本机成功构建，路径 `desktop/dist/AI Vision Forum.app`。bundle ID 为 `org.aivisionforum.agent`，资源齐全，arm64 与 Swift rpath 检查通过；`codesign --verify --deep --strict` 通过的是本地 ad-hoc 签名。未启动应用，未进行干净 Mac、正式签名或公证验收。实际 Tauri hook 的工作目录是 `ui/`，构建命令已按实测修正。

前端 `svelte-check` 零错误/警告，11 项 UI 回归通过，Vite 构建通过。桌面 Rust 单测 35 项通过、1 项需要人工观察电源状态而忽略；Dora bridge 全部 32 项单测通过。这些测试不启动真实采音。

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

当前 capabilities 明确为空：`jobs.run/jobs.cancel` 返回未实现。未将协议握手包装为已实现洞察或纪要；独立 Python 解释器、MLX 原生库随 .app 分发及干净机器验证仍待完成。

### F01：独立 Dora 实例验证与明确失败项

新增显式 loopback daemon endpoint adapter，按实际依赖 `dora-node-api 0.4.1 / dora-message 0.7.0` 请求 NodeConfig，校验 flow UUID、node ID、回包大小与 deadline。生产 widget 尚未切换到该 adapter；上游 `DoraNode::init` 注册阶段仍需由独立进程 supervisor 限时管理。

真实测试启动两组私有 coordinator/daemon 和无音频 timer 节点，端口及 Zenoh 均显式限制在 loopback。相同 node ID 分别连接不同 flow UUID；A 收到 Stop 正常退出后，B 仍为 Running 并继续收到 5 个 tick。按自有 PID 核验监听端口；测试创建的 6 个长期进程全部退出。

**生命周期门槛仍未通过：** 两个节点均收到 Stop 并以 0 退出，但 `dora stop` 没在 3 秒内返回完成确认；首次测试等到 12 秒也未完成。因此报告保留 `isolation_behaviors_passed=true`、`passed=false`。不能把停止事件已交付替代完整回收确认，更不能据此宣称生产 Translator 共存已经验收。

复现入口：[Dora spike 说明](../../desktop/moxin-dora-bridge/examples/README.md)。本次本地报告：`artifacts/local/f01-dora-isolation/1888e993-ad4a-4e25-bba9-d50604270902/report.json`；adapter 的 5 项测试通过。

## 第二步已落地的内容：F02-a

新增 `desktop/crates/forum-contracts/` 和 `forum-core/`，目前为无模型、无 UI 的数据与事务基础。

- 类型校验 UUID、非零修订号、音频范围、UTF-8 字节跨度，区分成功、空结果和 ASR 失败。
- SQLite 开启外键、WAL、FULL synchronous；先登记 capture 闭合段，再接受匹配范围的 final。
- 同一事务保存事件、不可变原文版本、每个目标语种的翻译 coverage 和内部 outbox。翻译未运行也保留原文和待翻译意图。
- 相同消息重复提交只返回原 receipt；消息 ID 或 producer run/seq 对应正文不同则拒绝；跨场引用拒绝。
- 人工修订检查 expected_revision，保留历史原文并将旧翻译 coverage 标为 stale。
- snapshot 与 cursor 来自同一读事务；数据库重开后仍可读出原文、缺失段、待翻译意图和增量事件。

17 项测试（3 项 contracts、14 项 core）通过，包括真实 SQLite 写入中途失败回滚、文件数据库重开、跨 session、emoji/组合字跨度和修订冲突。

**完整 F02 尚缺：** 有界单写 actor 与应用集成、完整会议状态机、TS/JSON Schema 生成及 Python 同版本验证、面向 UI 的稳定分页契约。当前 `Store` 是由 `&mut self` 约束的同步单连接实现，不宣称已经完成多进程写入 ownership。F03 的真实采音持久化/producer ack 尚未接入。

## 下一步顺序与剩余门槛

1. 完成 F01 的 Dora 隔离适配：私有实例的消息隔离已经用 timer 节点证明；先解决 Stop 完成确认超时，补有界进程监督与回收，再接生产音频节点。当前生产 controller 不再扫描或停止别的 flow；共享网络已有 flow 时拒绝启动。该保护不是完整实例隔离，也不代表已经能自动管理私有运行时。
2. 完成 F01 的 Rust 实时推理 + Python 8B 并发、实际取消/重启、内存释放测试，确定机器资源预算；补自动中英/混说验证。
3. 完成 Python/MLX 打包和干净 Mac 验证，以及真实两设备 LAN/TLS/手机访问可行性。这些需要相应运行环境；本机编译不能替代。
4. 补齐 F02 的 actor、会议状态机与生成契约；然后按 F03→F04 接真实音频、durable ack、译文来源与可靠停止，交付 M0。

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
