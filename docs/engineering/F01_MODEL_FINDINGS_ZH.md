# F01 模型可行性结果与语言后端决策

日期：2026-09-16，PDT。范围：F00–F01 的本机合成文件测量与工程决策。没有修改生产 ASR/翻译节点、桌面 UI 或模型缓存；没有开启麦克风、系统录音或下载新权重。本报告不代表真实会议字幕 `<3 秒`、90 分钟纪要、说话人识别或正式活动验收通过。进入下一部分实现前仍需用户确认。

## 1. 本轮决定

1. **当前桌面仍要求显式选择源语言。** 锁定的 Qwen Rust binding 不提供原生语言检测结果；配置项不能冒充 `detected_language`，空字符串或 `auto` 字面值也不能代表自动模式。
2. **F03 自动语言模式以 Whisper large-v3-turbo adapter 作为候选实现基线。** 本机已有模型在本组合成文件上能检测中文/英文并较好保留句内混说，但专有名词、代词和静音仍存在错误。这是后续实现的选择，不是当前包已经具备的功能。
3. **保留 Qwen3-ASR-1.7B 的显式语言路径，另保留原生 auto binding 修复候选。** 隔离实验能在仅提供 `language ` 协议前缀时生成语言标签，但仍会把英文为主的混说中的中文翻成英文；目前不足以作为忠实原文的自动转录方案。
4. **保留 Qwen3.5-2B-MLX-4bit 翻译和 Python Qwen3-8B-4bit 洞察/摘要候选。** 三者能在该机器同时运行，不过显著争用算力。8B 输出合法 JSON 也会捏造行动项；后续 F05/F08 必须验证证据并经过人工审核。

## 2. 样本、机器与可复现边界

机器为 Apple M4 Pro / Mac16,8 / 48 GiB，macOS 27.0（26A428）。Rust probe 为 **debug** 构建，使用 OminiX-MLX `6aac996db8b71fb7dae7a2409c46b4f2ade93092`。Python 使用现有 `.venv` 的 CPython 3.12.11、MLX 0.32.2、mlx-lm 0.31.3、mlx-whisper 0.4.3、NumPy 2.4.6。

| 角色 | 本机已有权重 | 来源标识 |
|---|---|---|
| Qwen ASR | `~/.OminiX/models/qwen3-asr-1.7b`，约 2.46 GB safetensors | 本机文件指纹见 `f01-local-model-fingerprints.json`；远端 revision 未凭空补写 |
| 2B 翻译 | `~/.OminiX/models/Qwen3.5-2B-MLX-4bit`，约 1.72 GB safetensors | 同上 |
| 8B 摘要 | Hugging Face 本机 `Qwen3-8B-4bit` snapshot | `545dc4251c05440727734bcd94334791f6ab0192` |
| Whisper | Hugging Face 本机 `whisper-large-v3-turbo` snapshot | `a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb` |

合成文本提交在 [`fixtures/f01-model-probes/corpus.json`](../../fixtures/f01-model-probes/corpus.json)。使用本机已安装的 Yue (Premium) 与 Samantha (English (US)) 声音，通过 `say -o` 写文件，转换为 16 kHz、单声道、16 位 PCM。未播放到扬声器。

| 文件 | 时长 | 内容 |
|---|---:|---|
| `zh_negation` | 6.140 s | 中文否定、十二位嘉宾、星期五下午三点 |
| `en_negation` | 5.286 s | 英文对应事实 |
| `mixed_zh_single_voice` | 5.283 s | 单个中文 TTS 声音读句内 API latency / five seconds / three seconds |
| `mixed_en_stitched` | 5.114 s | 一句话内拼接英文、中文、英文 TTS 片段 |
| `alternating_turns` | 9.564 s | 两个合成声音交替发言；间隔 450 ms，无重叠 |
| `short_en` | 2.191 s | 短英文否定句 |
| `silence` | 1.600 s | 数字静音 |

拼接声音不等于真实双语说话者，交替声音也不等于真实多人会场。未覆盖口音、混响、远场噪声、打断、交叠、设备丢帧或实际审核延迟。音频及逐段时间、SHA-256 保存在 machine-local `artifacts/local/f01-model-feasibility/audio-manifest.json`。

## 3. configured 与 detected 的证据

当前生产 `desktop/node-hub/dora-qwen3-asr/src/backend/macos.rs` 将语言字符串直接传给 `transcribe_samples_with_config`。锁定依赖的 `qwen3-asr-mlx/src/model.rs:693` 无条件构造带指定语言和 `<asr_text>` 的 assistant 前缀，返回值为字符串；生产 `src/main.rs` 的语言元数据来自上游配置/环境，不能据此声称检测到了该语言。

官方 Qwen3-ASR 的 Python 接口允许 `language=None` 并返回语言；其自动模式不附加强制语言后缀。这个官方接口能力并未自动进入当前 Rust binding。参考 [官方说明](https://github.com/QwenLM/Qwen3-ASR#quick-inference)、[官方 prompt 与解析实现](https://github.com/QwenLM/Qwen3-ASR/blob/main/qwen_asr/inference/qwen3_asr.py)、[锁定的 Rust 实现](https://github.com/OminiX-ai/OminiX-MLX/blob/6aac996db8b71fb7dae7a2409c46b4f2ade93092/qwen3-asr-mlx/src/model.rs)。

25 个 Qwen 提示组合均成功结束进程，但成功返回不代表内容正确。几个决定性结果：

| 输入 | Qwen 提示 | 观察 |
|---|---|---|
| 纯中文预算句 | `Chinese` | 保留中文原文，否定和期限正确 |
| 同一纯中文文件 | `English` | 直接变成英文译文，原文语言已改变 |
| 中文为主的句内混说 | `Chinese` | 保留 `API latency`、`five seconds`、`three seconds` |
| 英文为主、嵌入中文“因为它还没有审核” | `English` / 空字符串 / `auto` 字面值 | 中文片段被翻译成英文 |
| 同一混说文件 | `Chinese` | 本例保留中英混写 |
| 1.6 s 静音 | `Chinese` / `English` | 分别为空字符串 / `Oh.` |

因此不能简单给所有片段配置 English，也不能用 `auto` 字符串绕过 API 缺口。更不能把发生在 ASR 阶段的隐式翻译当作已经保存了真实逐字稿。

### 两个隔离的原生 auto 实验

实验脚本只复制锁定 ASR crate 到忽略的 `artifacts/local` 目录，并生成来源 SHA、补丁和独立 Cargo.lock；生产 Cargo manifest、缓存 checkout、模型文件均未改变。

- **去掉完整强制语言后缀，并保留协议分隔符解码：** 6 条有声文件全部得到空串；静音产生带协议片段的空文本。不能把官方入口的可用性推断成本机绑定可用。
- **只提供 `language ` 协议标记，不给语言值：** 7 条均得到可解析的模型标签。中文为 `Chinese`、英文为 `English`、静音为 `None` 且文本为空，证明该绑定有可修复的自动语言路径。不过英文为主的混说仍把中文翻成英文，另一例把 `API latency` 改为 `API 延迟`。检测到一个主导语言不等于保留了所有混说原文，也不等于检测了每段语言跨度。

第二种实验的 7 条推理为 271–1072 ms，load 824 ms，进程约 5.96 s 退出，采样 RSS 峰值约 2519 MiB。这些是后续缓存条件下的 debug 实验，不宜据此与首轮 load 作性能排名。第一轮进程约 5.09 s 正常退出；空文本是内容失败，不是异常退出。

原始证据：`artifacts/local/f01-qwen-auto-experiment/` 与 `artifacts/local/f01-qwen-auto-experiment-seeded/` 中的 `provenance.json`、`auto-language.patch`、`results.jsonl`、`process-metrics.json`。这些补丁未合入生产依赖。

### Whisper 自动识别对照

使用同一组 7 个 PCM 文件，`language=None`、`task=transcribe`、temperature 0、关闭跨文件上下文。返回的 `language` 是模型从最多前 30 秒得到的主导语言；另存 top-3 概率，仅作诊断，不把概率当校准过的可靠性保证。

- 中文/英文/短句语言为预期的 zh/en；两类混说与交替发言总体保留两种语言。
- `API latency` 被识别为 `IP Latency`；“它”变为“他”；数字的书写形式也可能变化。
- 数字静音仍输出 `Thank you.`。生产输入必须保留 VAD/无语音状态，不能把任何非空模型输出都发布成字幕。
- 单文件 `transcribe` 时间为 745–1036 ms；该时间包括自身自动检测，但不包括脚本另做的 top-3 诊断检测。结果没有经过真实会议或持续流测试。

`quality.json` 额外计算了去标点/空格后的字符编辑比例，用于定位差异；该数值没有统一数字口读、英文缩写或跨语种表达，不是正式 CER/WER 质量门槛。

## 4. 并发、排队、RSS 与取消

先分别运行 Qwen ASR 的 25 个组合、2B 翻译的 24 条文本（6 条内容重复四轮）、8B 的两条相同摘要任务。之后同时启动三个独立进程运行同样工作。没有 Dora、设备采集、数据库、桌面 UI 或真实 meeting-worker RPC，不能把模型耗时加总后当作端到端字幕延迟。

| 模型 | 单独运行：单任务中位 / 最大 | 三进程并发：单任务中位 / 最大 |
|---|---:|---:|
| Rust Qwen ASR，25 条诊断组合 | 639 / 1071 ms | 986 / 2179 ms |
| Rust 2B 翻译，24 条 | 299 / 542 ms | 509 / 1350 ms |
| Python 8B 摘要，2 条 | 7530 / 7685 ms | 11625 / 13814 ms |

摘要输入为 8 段虚构转录、381 prompt tokens，每次产生 341 tokens。第二条摘要在 probe 自有串行循环中的等待为单独运行 7693 ms、并发运行 13822 ms。这个等待从“模型 ready 后一次性接收任务数组”起算，体现前一个任务的阻塞；**它不是产品持久队列的排队测量**。ASR 的队列数字同样属于 probe 顺序遍历整组文件的等待，不能代表实时音频负载。

进程整体观察：

| 进程 | 单独运行 wall / RSS 峰值 | 并发 wall / RSS 峰值 |
|---|---:|---:|
| ASR | 18.49 s / 2540 MiB | 27.92 s / 2541 MiB |
| 2B 翻译 | 8.81 s / 1300 MiB | 17.45 s / 1301 MiB |
| 8B 摘要 | 19.64 s / 4762 MiB | 29.33 s / 1440 MiB |

RSS 用 `ps` 每约 100 ms 采样直接子进程；wall 包含进程启动、导入/加载和整个任务集。macOS RSS 不能完整描述 MLX/Metal 统一内存，页驻留和共享映射会随运行变化：并发摘要 RSS 较低不表示模型更省内存。摘要自身 MLX allocator 峰值两种运行均约 **5.16 GB**；本轮并发采样 RSS 总峰值约 4975 MiB，不能拿这个数字给最低内存要求背书。仍只验证了这台 48 GiB 机器。

取消测试只向自己创建的子进程发送 SIGTERM，并在 2 秒未退出时才准备升级 SIGKILL；本轮均未升级：

| 角色 | 取消触发点 | SIGTERM 到退出 | 退出码 | 新进程重启 |
|---|---|---:|---:|---|
| ASR | 已记录 case_started、开始处理文件 | 37.91 ms | -15 | 单条完成，exit 0，约 1.63 s |
| 翻译 | 首条完成，仍有多条待处理 | 18.90 ms | -15 | 单条完成，exit 0，约 1.72 s |
| 摘要 | 已产生首 token | 74.76 ms | -15 | 单条完成，exit 0，约 10.49 s |

这证明独立模型进程可终止并重新加载；没有证明 token 级协作取消、Dora shutdown、模型状态复用、durable ack/outbox、崩溃恢复或公共页面撤回。翻译取消点也不能声称精确发生在某一 token 内。后续需要由 F04/F05 的生产生命周期实现承接。

所有正常 baseline/concurrent/restart 进程 exit 0；3 个被取消的进程均被 wait/reap。四个模型目录在整组实验前后文件列表、大小、mtime_ns 相同；Qwen 两个额外实验也做了同样检查，未发现模型目录写入。

## 5. 摘要内容失败及后续约束

temperature 0 的两次 baseline 和并发输出都能解析为 JSON，但出现了原文没有的行动项：把“设备预算尚未批准、暂时不能确认”变成由 **speaker_1 负责确认设备预算**。原文没有把这个行动分配给 speaker_1。

该问题不能靠 JSON schema 或“引用了一个 turn id”自动解决。F05/F08 后续必须区分决定、建议、未决和行动；行动的 owner/due 未明确时保留 unknown/null；对声称事实检查所引用 evidence 的实际支持关系。公开洞察继续人工审核，模型产物是候选，不自动发布。这里记录失败证据，没有在 F01 实现审核功能。

## 6. F03 可实施的 adapter 边界与打包成本

建议保留已有协议中的 `configured_source_language` 与 `detected_language` 分离，并进一步明确其证据来源；新增字段须在下一阶段与契约/生成类型一起审核，不能只改本报告。

- 显式 Qwen 模式：记录实际 hint，`detected_language` 保持未知；原文先持久化，再进入翻译。不要把翻译目标反向当 ASR 源语言提示。
- Whisper auto 候选：独立 ASR 子进程接收有界的 16 kHz float32 PCM 段，输出文本、模型返回的主导语言和时钟范围。与 8B 摘要进程分开，避免摘要阻塞语音请求。
- 混说：不得仅凭单一 detected label 改写或丢掉其他语言内容。脚本文字中中英字符并存只能作为“混说待检查”的启发信号，不能冒充音频语言检测。需要进一步验证自动路由和中英双目标输出，保留原始识别文本及不确定状态。
- 调度：实时原文/翻译优先，后台摘要只允许一个待执行实例并合并过时请求；本轮约 1.5–1.7 倍中位耗时退化提供了排队隔离的依据，但具体时间预算仍待生产链路测量。
- 原生 Qwen auto：后续修复需覆盖语言协议生成、tokenizer 特殊标记、混说是否被隐式翻译、静音/短句、词级证据；目前两个临时变体都没有满足完整忠实混说要求。

**当前 packaged meeting-worker 尚未包含 Whisper adapter。** 要按本轮环境复现自动候选，至少固定 `mlx-whisper==0.4.3`，并在现有 Python 3.12.11 / MLX 0.32.2 基线上锁定所有传递依赖。该版本元数据还依赖 numba、SciPy、tiktoken、torch、tqdm、more-itertools、Hugging Face Hub 和 NumPy，不能视为一个可直接复制的纯 Python 小模块。本机相关版本包括 numba 0.67.0、SciPy 1.18.1、tiktoken 0.14.0、torch 2.13.0。

后续需为这些依赖检查 macOS deployment target、原生库、许可证/SBOM、包体积和干净 Mac 导入/推理。直接安装官方完整依赖会带来 torch 等额外成本；若裁剪未使用路径，必须单独维护来源与回归验证，不能在此假定裁剪安全。本次传入 NumPy PCM 数组绕过 ffmpeg；若未来传音频文件路径，则该库的解码路径还要求 ffmpeg CLI，必须另行打包和审计。Whisper 权重约 1.61 GB，目前只复用本机缓存，未纳入发布下载清单。

## 7. 复现入口与证据

从仓库根目录执行，前提是本机已有上述完整缓存和 Python 环境；命令不会下载权重：

```sh
python3 scripts/probe_forum_models.py prepare
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked \
  --manifest-path desktop/Cargo.toml -p dora-qwen3-asr --example forum_asr_language_probe
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked \
  --manifest-path desktop/Cargo.toml -p dora-qwen35-translator --example forum_translation_probe
cp /tmp/aivf-cargo-target/debug/mlx.metallib /tmp/aivf-cargo-target/debug/examples/mlx.metallib
python3 scripts/probe_forum_models.py run \
  --summary-model "$HOME/.cache/huggingface/hub/models--mlx-community--Qwen3-8B-4bit/snapshots/545dc4251c05440727734bcd94334791f6ab0192" \
  --whisper-model "$HOME/.cache/huggingface/hub/models--mlx-community--whisper-large-v3-turbo/snapshots/a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb"
```

翻译 probe 使用已有 `forum_translation_probe` example。实际主入口 [`scripts/probe_forum_models.py`](../../scripts/probe_forum_models.py) 顺序运行单独基线，再明确启动三进程并发，最后逐个取消/重启。并行测量时应避免其他任务使用 GPU。

auto 实验用 [`scripts/probe_forum_models_auto_patch.py`](../../scripts/probe_forum_models_auto_patch.py) 的 `--source` 指向已存在的上述 pinned checkout、`--mlx-prebuilt` 指向现有预编译 MLX 资源。默认构建“无语言前缀”变体；`--seed-language-marker` 构建“只提供 language 协议标记”变体，必须使用新的 `--output`，旧实验不会覆盖。它只构建临时可执行文件，**不会加载模型**；构建后以 MODEL、audio-manifest.json、OUTPUT_JSONL 三个本地路径参数运行该文件。

主要 machine-local 证据：

- `artifacts/local/f01-model-feasibility/report.json`：进程、退出码、RSS、并发、取消与重启。
- `baseline/{asr,translator,summary,whisper}/`、`concurrent/`、`lifecycle/`：完整输入归属、输出、耗时和 stderr。
- `quality.json`：提示组合、实际文本与原文差异，不是正式通过结果。
- 两个 `f01-qwen-auto-experiment*` 目录：隔离 patch、来源哈希、构建日志及原始语言协议输出。

公开源码只包含虚构文本、probe 与本报告；上述生成音频、绝对本机模型路径和指标保留在忽略的 `artifacts/local` 中。

## 8. 实际模型 binary 与 private Dora 集成补测

新增独立 example `desktop/moxin-dora-bridge/examples/forum_model_binary_probe.rs`，使用实际 debug ASR 与 translator 可执行文件。生产 controller 将它们转换为 dynamic 节点，通过 `DORA_NODE_CONFIG` 传入已核验的私有端点，并直接持有各自 Child/process group。探针只把已有的 `zh_negation.wav` 转为 float32 PCM，没有创建麦克风、系统录音或音频播放设备；最终结果通过生产 `TranslationListenerBridge` 读取。

本轮只进行了以下两次尝试，没有继续扩大试验：

1. **首轮立即发送：失败。** controller 的 `start()` 返回后立即发送唯一合成音频帧；两个实际模型随后都注册、加载成功，翻译 warmup 也成功，但 60 秒内 ASR 日志没有收到音频、listener 没有最终文本。进程约 67.99 秒后以 exit 1 结束并执行 owned cleanup；事后核对三个 producer/model PID 均已消失。这个失败说明 `start()` 返回不等于音频订阅和模型已经 ready，具体丢失环节尚未定位。
2. **只重试一次，在两个实际模型的 ready 日志出现后发送：成功。** 生产 listener 收到原文“我们还没有批准这项预算，请在星期五下午三点前确认十二位嘉宾的名单。”，对应译文为 “We have not yet approved this budget. Please confirm the list of twelve guests by Friday at 3:00 PM.”。此次 start 返回约 610 ms、两模型 ready 约 2016 ms、释放合成帧到最终原文译文约 1598 ms。这是单文件功能证据；本机可能同时构建应用，不能把这些数字用作正式字幕延迟基准。

成功轮停止耗时约 3104 ms，记录为 `acknowledged=false`、`contained=true`、`StoppedByOwnedRuntimeFallback`：Dora CLI 超过 3 秒等待，由自持 runtime 完成回收，并非正常 CLI 完成回执。该 flow 的 5 个进程（coordinator、daemon、producer、ASR、translator）全部消失，生产 listener 已 join，私有 lookup 端口关闭。另一个独立、纯文本的 peer 在模型 flow 停止后仍继续接收 68→71 条，之后单独停止，其 3 个进程和 lookup 端口也完成回收。实际 ASR/translator 的 PPID 为探针 controller，PGID 各等于自身 PID，确认它们没有交给共享 daemon 代管。

成功轮总进程时间约 11.21 秒、exit 0。两轮前后模型文件列表、大小、mtime_ns 均相同，没有发现模型缓存写入。本补测只增加 example，没有修改生产启动逻辑；日志门控仅属于探针。F03 仍需正式启动 barrier、生产端确认/缓冲和首帧验收，不能据此声称生产不会丢首帧；持久确认机制仍需与后续队列/协议一起设计。

复现命令（仓库根目录，已有完整缓存；先生成第 7 节的合成文件）：

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked \
  --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --example forum_model_binary_probe
mkdir -p artifacts/local/f01-model-binary-integration-ready
FORUM_AGENT_DORA_BIN="$HOME/.cargo/bin/dora" HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
  /tmp/aivf-cargo-target/debug/examples/forum_model_binary_probe \
  artifacts/local/f01-model-binary-integration-ready /tmp/aivf-cargo-target/debug \
  artifacts/local/f01-model-feasibility/audio/zh_negation.wav \
  "$HOME/.OminiX/models/qwen3-asr-1.7b" \
  "$HOME/.OminiX/models/Qwen3.5-2B-MLX-4bit"
```

首轮证据为 `artifacts/local/f01-model-binary-integration/40151569-bd17-4916-9c0d-5abcc6e063a6/`；成功轮为 `artifacts/local/f01-model-binary-integration-ready/a93e2c81-fa40-4b0b-97d5-27d28b1bb0de/`。其中保存 `report.json`、`model-child-processes.txt`、`post-exit-check.json` 和复制出的 `runtime-logs/`；成功轮另存 example/binary SHA-256。各自上层的 `invocation.json` 记录进程退出和模型文件不变检查。保留首轮失败，不用第二轮成功覆盖它。
