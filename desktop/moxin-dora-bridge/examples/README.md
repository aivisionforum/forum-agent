# F01 私有 Dora 实例验证

这是独立 spike，生产 `controller` 和 widget 仍使用原有调用路径。本例不启动麦克风、ASR、翻译模型或 Python meeting-worker。

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
  --output artifacts/local/f01-dora-isolation
```

每次运行生成独立 UUID 目录，保存两个实例的 CLI 输出、stdout/stderr、逐条节点事件及 `report.json`。只使用 Python 标准库，不使用安装在 Python 中的 `dora-rs`。

本机 2026-09-16 观察：5 个 adapter 测试通过；两套真实私有实例里的 `same-dynamic-node` 分别连到各自 UUID。A 收到 Manual Stop 并退出后，B 仍为 Running 且继续收到至少 5 个 timer 事件；B 也能收到 Stop 并退出。全部 6 个持有的长期子进程已退出，默认端口和已有 Hen 实例未被访问。

存在尚未解决的停止确认问题：纯动态节点收到 Stop 并退出后，`dora stop` 仍未在限定时间返回。第一轮等了 12 秒；第二轮将 CLI 等待限制为 3 秒，同时独立验证节点退出和 B 不受影响。报告因此保留 `isolation_behaviors_passed: true` 与 `passed: false`，进程退出码为 1。清理由脚本对自己创建的服务进程执行 terminate 完成。根因尚未确定，不能据此宣称生产停止流程、模型共存、崩溃恢复或完整产品隔离已经通过。

最近完整证据目录：`artifacts/local/f01-dora-isolation/1888e993-ad4a-4e25-bba9-d50604270902/`；首轮失败证据目录：`artifacts/local/f01-dora-isolation/d373e30b-0c7a-489e-98eb-d969056a7377/`。
