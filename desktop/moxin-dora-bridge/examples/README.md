# F01 私有 Dora 实例验证

本目录包含独立协议 spike 和生产控制器探针；两者都不启动麦克风、ASR、翻译模型或 Python meeting-worker。

## 独立协议与上游停止行为

本节的 `forum_dynamic_node_probe.rs` 与 Python driver 独立于生产 `controller` 和 widget。

`forum_dynamic_node_probe.rs` 通过新 adapter 请求指定 loopback daemon 的动态节点配置，核验数据流 UUID、节点 ID、dynamic 标记、回包大小和超时。官方 node-api 0.4.1 实际使用 message 0.7.0：请求是 8 字节小端长度加 bincode，NodeConfig 回复是同样长度前缀加 JSON。节点实际事件端口由 daemon 另外分配，不等于配置查询端口。

父脚本仅创建并终止自己持有的 coordinator、daemon 和测试节点进程。两套实例各用独立目录和非默认端口，协调服务、节点事件通道及 Zenoh 均限制到 loopback；Zenoh multicast/gossip 关闭。脚本按具体 daemon PID 核实事件端口归属，随后才通过 stdin 允许子进程调用 `DoraNode::init`。后者的上游注册调用没有总超时，所以只能在本例受监督的子进程边界内运行，不应直接移入 UI 主进程。

在仓库根目录执行（macOS，需要 `lsof` 和原生 Dora CLI 0.4.1）：

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked \
  --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --lib dynamic_node_endpoint::tests
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked \
  --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --example forum_dynamic_node_probe
python3 desktop/moxin-dora-bridge/examples/run_forum_dora_isolation_probe.py \
  --dora /Users/zenghaochen/.cargo/bin/dora \
  --node /tmp/aivf-cargo-target/debug/examples/forum_dynamic_node_probe \
  --output artifacts/local/f01-dora-shutdown \
  --shutdown-policy owned-runtime-fallback
```

每次运行生成独立 UUID 目录，保存两个实例的 CLI 输出、stdout/stderr、逐条节点事件及 `report.json`。只使用 Python 标准库，不使用安装在 Python 中的 `dora-rs`。若要严格要求 CLI 成功回复，用 `--shutdown-policy strict-cli-ack`（也是省略参数时的默认值）；纯动态 flow 在此模式下应返回退出码 1，不能把清理成功当成 CLI 确认。

本机 2026-09-16 观察：5 个 adapter 测试通过；两套真实私有实例里的 `same-dynamic-node` 分别连到各自 UUID。A 收到 Manual Stop，完成 EventStream/DoraNode 的显式 Drop，并退出；脚本随后回收 A 的 daemon/coordinator，核验 PID 消失及其监听端口不再接受连接。此时 B 仍为 Running 且继续收到至少 5 个 timer 事件；B 随后也完成同样的回收。全部 6 个持有的长期子进程已退出，默认端口和已有 Hen 实例未被访问。

停止确认的根因已经在 crates.io 官方 0.4.1 源码中定位。CLI 安装元数据确认它来自 registry 0.4.1；daemon/coordinator 源码包的 `.cargo_vcs_info.json` 对应提交 `27cf18cd76aed1ca635162b83411adc2e035b36c`，CLI 包对应 `1ba780964bb64791ec78ee18f60888490dfbe7bf`。本机另一个 OminiX/dora checkout 是 0.3.12，不作为本次版本证据。

- node-api 0.4.1 的 `EventStream::drop` 发送 `EventStreamDropped`，`DoraNode::drop` 发送 `CloseOutputs` 和 `OutputsDone`；探针为两个 Drop 返回分别写了持久化里程碑。因此并非遗漏 Drop。
- 官方 daemon `src/spawn/prepared.rs:73` 为 dynamic node 保存 `process=None` / `pid=None`；`:254` 直接返回 Dynamic，关闭完成信号的 sender。`:110` 的 restart loop 收不到进程完成事件后退出，无法产生 `SpawnedNodeResult`。
- 官方 daemon `src/lib.rs:1697` / `:1743` 接到 OutputsDone / EventStreamDropped 只处理 channel 关闭；`:1955` 的 `handle_outputs_done` 不更新运行节点集合。只有 `SpawnedNodeResult` 路径调用 `handle_node_stop_inner`（`:2019`），移除节点并产生 AllNodesFinished（`:2062` 起）。纯动态 flow 没有这条触发路径；`--force` 也只能操作非空 process handle，不能弥补这一缺口。
- 官方 coordinator `src/lib.rs:603` 将 CLI stop 的回复挂起，只有收到全部 daemon 的完成结果后才回发 DataflowStopped（`:348`、`:395`、`:404`）；CLI `src/command/stop.rs:96` 等待的就是此回复。因此 Stop 交付和整条 flow 的完成确认是两件事。

官方源码和原始 `.crate` 保存在 `artifacts/local/f01-dora-official-source/`。没有修改或替换官方二进制，也没有注入假的 AllNodesFinished / DataflowStopped。含静态 ASR/translator 进程的 flow 可能产生正常完成事件，但 daemon 的条件允许忽略仍然运行的 dynamic node，生产监督器仍需先核验自己持有的动态桥接已退出。

本例把停止结果分成三类：`StoppedGracefully` 表示 CLI 确认且持有的进程与端口已回收；`StoppedByOwnedRuntimeFallback` 表示 `acknowledged=false, contained=true`；未完成所需验证则为 `StopFailed`。CLI 等待限定 3 秒，节点退出等待限定 6 秒，每个自有服务先 terminate 并等待最多 3 秒，必要时 kill 后再等待最多 3 秒。只回收本次创建且仍持有句柄的进程，禁止对共享 daemon 使用这一策略。原生 DoraNode 的 Drop 仍可能阻塞，因此本例的节点必须处于受监督的进程边界。

严格模式对照证据：`artifacts/local/f01-dora-shutdown/1d2e100e-7984-40d1-89d2-bba838d226e1/report.json`，`passed=false, cli_acknowledgement_passed=false, containment_passed=true`。显式 fallback 模式证据：`artifacts/local/f01-dora-shutdown/a51dabac-76e2-437a-aa91-5233a8dfb474/report.json`，`passed=true, cli_acknowledgement_passed=false, containment_passed=true`。这里的通过仅表示指定的自有实例回收验收通过，不代表上游 CLI 问题修复、真实音频链路、模型共存或崩溃恢复已经通过。


## 生产控制器、字幕监听器及进程回收

`forum_owned_controller_probe.rs` 使用真实 `DataflowController`、`DynamicNodeDispatcher` 和 `TranslationListenerBridge`。原始图含一个本地合成 executable 和一个字幕动态节点。合成 executable 只把 100 ms timer 转成带实例标记的字符串，不运行模型。它同时作为探针的子进程入口，必须通过控制器设置的 `DORA_NODE_CONFIG` 注册，不能经默认端口连接。

官方 daemon 0.4.1 的 `src/spawn/prepared.rs:234–252` 通过 `process_wrap::tokio::ProcessGroup::leader()` 给每个静态节点另建进程组。因此仅终止 daemon 的进程组不能证明 ASR/translator 被回收。当前生产控制器把私有图内的本地 executable 节点转换为 `path: dynamic`，再用已核验 daemon PID/端口/UUID 的 NodeConfig 直接启动原 executable，并保留 `Child` 和其进程组。CLI、coordinator、daemon、模型 binary 因而全部由桌面控制器直接监督；没有依赖轮询发现 PID 的窗口。原图保留不变，副本使用 TCP 和独立的 0700 UUID 目录。未实现的 args、operator/custom runtime、build/git、restart 等字段明确拒绝。

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked \
  --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --lib
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --offline --locked \
  --manifest-path desktop/Cargo.toml -p moxin-dora-bridge --example forum_owned_controller_probe
mkdir -p artifacts/local/f01-owned-controller
FORUM_AGENT_DORA_BIN=/Users/zenghaochen/.cargo/bin/dora \
  /tmp/aivf-cargo-target/debug/examples/forum_owned_controller_probe \
  artifacts/local/f01-owned-controller
```

本机 2026-09-16 的最终证据：`artifacts/local/f01-owned-controller/9a1724fb-9521-480a-af47-cce571e102cb/report.json`，摘要为 `artifacts/local/f01-owned-controller-summary.json`。对应的私有图、Zenoh 配置和原始进程日志已复制到同一目录的 `a-runtime/`、`b-runtime/`、`c-runtime/`。

- A/B 的同名 `synthetic-translator` 和 `moxin-translation-listener` 分别绑定自己的 UUID、daemon 和 event socket，字幕历史始终保留正确实例标记。合成节点自报的 PPID 等于控制器 PID，PID 等于独立 PGID，确认由桌面直接持有。
- A 正常停止约 3129 ms；明确记录 `acknowledged=false, contained=true, StoppedByOwnedRuntimeFallback`。A 的监听器完成断开后，B 从 36 条继续收到 41 条，状态仍 Running。
- 对仍持有的 B daemon 注入 SIGKILL；B 合成节点故意在 daemon 崩溃后继续存活并忽略 SIGTERM。B 停止约 3078 ms，监督器最终回收其直接 Child 进程组，报告仍是未获 CLI 确认的 fallback。
- C 对自己的合成模型 Child 注入 SIGKILL；`get_status()` 返回明确的 `Owned executable node ... exited` 错误，而不是因 Dora 的 pure dynamic flow 仍显示 Running 而掩盖模型退出。其后停止和回收成功。
- 三次运行持有的共 9 个 coordinator/daemon/node PID 均已不存在；按这些具体 PID 记录的共 18 个监听端口全部不再接受连接。检查只涉及本次自有 PID、UUID 和端口。

生产 bridge 库测试 38 项通过，日志在 `artifacts/local/f01-owned-runtime-tests.log`。这包括 fake CLI 超时/失败路径、失败后保留 ownership、未知 UUID 不发送 stop、目录替换/脚本拒绝，以及已观察消失的进程组不再接收信号的回归测试。真实探针验证的是控制器隔离、合成数据流和故障回收，不等于真实麦克风、模型并行性能、字幕质量或整个桌面进程被强杀后的恢复已验证。
