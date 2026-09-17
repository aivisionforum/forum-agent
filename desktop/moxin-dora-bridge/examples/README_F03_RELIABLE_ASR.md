# F03：可靠原文与录音恢复验证

`forum_reliable_asr_probe.rs` 使用生产 `CaptureSession`、`DataflowController`、`dora-qwen3-asr`、`forum-runtime` 本地 socket/outbox 和 `forum-core` SQLite actor。图中没有 translator，不打开麦克风、系统采音或扬声器，不下载模型。输入为仓库已有的合成语音 WAV，以及 30 ms 合成 PCM。

## 重跑

从仓库根目录运行；路径均可换为已核验的本地资源。先串行编译，避免运行中的二进制发生变化。

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked --manifest-path desktop/Cargo.toml -p dora-qwen3-asr --bin dora-qwen3-asr
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --example forum_reliable_asr_probe
FORUM_AGENT_DORA_BIN=/Users/zenghaochen/.cargo/bin/dora \
FORUM_ASR_PYTHON=/tmp/forum-f02-asr-runtime-2/python/bin/python3.12 \
FORUM_ASR_SCRIPT=/tmp/forum-f02-asr-runtime-2/asr/asr_worker.py \
FORUM_WHISPER_MODEL_PATH=/Users/zenghaochen/.cache/huggingface/hub/models--mlx-community--whisper-large-v3-turbo/snapshots/a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb \
/tmp/aivf-cargo-target/debug/examples/forum_reliable_asr_probe
```

输出报告在 `artifacts/local/f03-durable-asr/<run>/report.json`，合成 PCM/manifest/SQLite 保留在报告指明的独立 `/tmp/f03-asr-<run>/` 目录。探针只创建和回收自己的 Dora coordinator、daemon 和 ASR 子进程。每次均使用新的会话、进程组和私有端口。

已通过的报告：`a904f28e-d4f8-4c89-aecb-caf22b77d6a9/report.json`。

- ASR 未 ready 前，30 ms 首段不能 dispatch。
- 30 ms 段有明确 `Empty / segment_shorter_than_100ms` 终态；其余三个文件有独立 `Success` 原文，包含中文、英文、句内混说。
- 首个 final 已提交但故意丢失 ACK，随后以同一消息重试。最终只有四个终态，ASR outbox 清空，producer seal 已持久确认。
- 报告中的 elapsed 是文件注册/dispatch 到持久原文的功能观测，不能当作真实会议延迟指标。
- Dora CLI 0.4.1 此图的正常停止未在 3 秒内确认；生产监督器只回收其拥有的进程，报告 `acknowledged=false, contained=true`。不会把此结果标成优雅停止。

## 回归测试

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked --manifest-path desktop/Cargo.toml -p forum-runtime
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --lib
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked --manifest-path desktop/Cargo.toml -p dora-qwen3-asr --bin dora-qwen3-asr reliable::tests
```

这些测试不加载模型。覆盖持有文件锁、错误 ACK 不删除 outbox、跨 run 重放原消息、失败批次只尝试一次 RPC、core 不可用时 PCM/事件保留、活跃 VAD 段崩溃后沿用 ID、录音关闭时拒绝伪恢复、样本时钟 gap、空目标原文模式、方向边界 ACK 失败后恢复、第二次 ASR 仍失败时的 producer seal 栅栏。

## 生产行为和范围

采音最小 barrier 只有 ASR ready；translator 迟到或不存在不会阻断原文。先把处理后的 16 kHz PCM、段 ID/样本范围和 capture 登记持久化，得到 core ACK 后才向 Dora 发送。每段 final 直接写 core；字幕 UI 从 core 投影读取，legacy listener 不再覆盖它。

`request_capture_stop()` 仅要求释放采音设备和提交尾段；它不关闭 Dora source。采音/replay 完成后仍持有 Dora node，等待 `dispatcher.stop()` 的独立 teardown 信号，让队列中的 ASR/翻译完成。Replay 完成另发送带确切 `(segment_id, revision)` 列表的 `capture_dispatch_complete` 栅栏；producer seal 同时核对当前 run 已处理版本和所有预期终态。

方向切换先关闭并 ACK 旧段，再提交含样本边界的 prospective direction event，最后激活新方向。初始参数、方向事件和每段完整 metadata 都保存在 manifest；未 ACK 的方向事件必须先重放。自动/双语模式不能用单目标 swap。

背压使用最多八段的本地待发送队列，Dora中一次只留一个未持久确认的段；实时采音循环不等待模型推理。满队列或 60 秒无确认会明确停止，已保存 PCM 可用于恢复。Replay 分批按 ≤256 个段 ID 查询 revision，逐段确认，不能用截断的 session status 猜测版本。停止时保留 source node 直到 core drain 完毕，避免 Dora 提前关闭整条图。

新代码在 ASR/translator ready 前按实际模型文件内容计算 `sha256:` 身份；配置/分词文件和 safetensors 均参与，HF 文件 symlink 支持，只读流式哈希。上面的既有功能报告早于这项加固，仍保留原开发 slug，不能当作权重指纹证据。该功能没有引入正式模型注册表。

当前可靠链路的模型验证范围为 Apple Silicon macOS。每次选择一个显式 track（麦克风或系统音），处理 PCM 的采样时钟不等同于原始设备音频，也不提供说话人分离。可靠麦克风分支使用 CPAL，不使用没有 stop 确认的 legacy Swift AEC 单例。Whisper 自动识别在段结束后调用持续 Python worker，未宣称模型本身实现流式 ASR。尚未进行真实设备权限/释放验证、长会压力测试、音频质量或会议延迟验收。逐批写 chunk/checkpoint 时仍会原子重写完整 manifest，长会 I/O 放大需要后续压力验证与日志压缩设计。
