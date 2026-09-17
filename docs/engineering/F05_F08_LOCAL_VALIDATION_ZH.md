# 第三部分：单场论坛工程实现与本机验证

日期：2026-09-16。分支：`codex/vision-forum-integration`。

用户本轮授权“Git push, then start part 3”。先将第二部分的 `73f7bb1`、`3969fcb` 推送到 origin，然后实施 F05–F08。用户随后要求 git push，本次推送范围为第三部分工程代码与验证文档。第四部分 F09–F12 未开始；F01 外机验收仍延期。

## 1. 本轮实现

| 范围 | 实际交付 | 代码入口 |
|---|---|---|
| F05 | 独立 Python 分析进程、六类结构化任务、不可变快照、实际模型/提示词指纹、逐字 UTF-8 引用校验 | `services/meeting-worker/src/forum_meeting_worker/`、`desktop/crates/forum-runtime/src/worker.rs` |
| F06 | SQLite 持久 job/attempt/总 deadline、单后台许可、实时启动抢占、取消及进程组退出确认、已确认块缓存 | `desktop/crates/forum-core/src/analysis.rs`、`desktop/forum-shell/src/analysis.rs`、`forum-runtime/src/resource_budget.rs` |
| F07 | 统一会议库与分析工作区、任务状态、引用定位、编辑审核、独立只读公开大屏 | `desktop/forum-shell/ui/src/ForumWorkspace.svelte`、`DisplayApp.svelte`、`desktop/crates/forum-gateway/` |
| F08 | 会中每 3 分钟洞察/纪要块预计算、停止后持久纪要请求、所选场次已发布内容报告、撤回/失效传播、三格式导出、旧 JSONL 导入 | core/worker/UI 上述入口及 `desktop/crates/forum-core/src/legacy.rs` |

原实时 ASR/翻译和双语浮窗继续使用第二部分实现：自动模式 Whisper，显式语种 Qwen ASR，翻译 Qwen3.5 2B。新增分析默认使用已有本机 Qwen3 8B 4bit。没有将旧 Python HTTP 服务嵌入桌面，也不需要运行旧 `8710/control` 页面。

## 2. 数据与任务流程

1. 原文、译文、录音状态由 core 持久管理。任务创建时固定场次集合、输入版本、完整性、模型/提示词/profile/config hash，并生成不可变快照。
2. Rust 创建私有 job/attempt 目录，通过 NDJSON 向 worker 授予本机模型路径。Python 不访问数据库，不采音，不自动下载模型。控制进程保持可响应，计算放在独立 child。
3. 真实 tokenizer 对完整提示词计数，当前 context 8192、输出上限 1024、安全余量 128，输入最大 7040 tokens；完整段优先，过长段按 Unicode 边界切分。每个源字节都有覆盖状态，尾部短句不会因显示条数限制被截断。
4. worker 发出 checkpoint，core 验证并保存后 ACK。复用限定同活动、所选场次、任务种类、配置，以及完整块输入 hash；包括 ID、revision、UTF-8 范围和正文。新快照可以复用未变化的前块，变化的尾块重新计算。
5. worker 结果回执不直接等于成功。Rust 验证实际文件字节和退出状态，core 在最终事务再次检查 attempt、取消、快照、来源版本、录音缺口、引用和 coverage；不合格结果不能发布。
6. 生成结果先为私有草稿。人工编辑创建新 revision，审核与发布分开；公开标题、正文和证据需要独立确认。公开 payload 不携带内部原文 ID、路径或未经审核的引文。
7. 源文修订、迟到音频缺口、上游隐藏会使依赖结果失效，并撤回公开投影。活动报告只读取明确所选场次的已发布产物，没有内容时不回退到私有原文或全部会议。

数据库迁移版本为 5；wire schema/worker protocol 仍为 1。对象 hash 使用递归键排序的 UTF-8 JSON，已验证 serde_json `preserve_order` 不改变结果。实际数据结构见 `packages/contracts/forum.generated.ts` 和 `forum.schema.json`。

## 3. 资源调度、停止与恢复

后台同时只运行一个任务。实时开始先撤销后台许可，等待自有 worker/compute 进程组实际退出后才允许准备采音。实时模型准备、识别/翻译积压、恢复和停止收尾期间，分析任务显示等待原因；运行中的后台任务可被中断，保存已确认块。

默认小任务预算 90 秒，纪要/报告预算 180 秒，包含排队；超时明确失败。用户重试增加 attempt，并显示新的预算，最多 3 次；不会把原任务的等待时间抹掉。任务因抢占/退出中断后可手动重试。重启先将遗留 running 任务标为 interrupted。

可选环境变量 `FORUM_ANALYSIS_BATCH_MODEL_PATH` 指定已有本机大模型，使用 `meeting-32b-v1`，仅会后空闲许可下执行。会中洞察及本场自动纪要预计算固定使用 8B。尚未进行真实 32B 抢占或统一内存/时延验收；当前积压阈值属于工程初值。

运行生命周期直接通知分析模块，不依赖前端每次轮询都读到 Started/Stopped。停止后先保存持久纪要 intent，再构造 job；每次停止有新 marker，旧确认不会吞掉同场恢复后的新请求。即使退出已停止分析线程，最终 Stopped 仍能保存请求，重启后再处理。模型未准备时请求保留，并显示原因。

关闭主窗口仍隐藏窗口；退出软件请求停止采音和分析。实时清理超时保留 dispatcher/host/许可并继续重试，不能用“线程结束”代替资源退出证明。45 秒仍无法确认会显示错误并阻止退出；没有全局 `pkill` 或清理不属于本实例的端口进程。

## 4. 界面与操作

1. 打开 `desktop/dist/AI Vision Forum.app`，在主窗口进入论坛工作区。会议库支持分页；可选现有会议或导入旧 JSONL。
2. 在洞察或纪要页创建任务，查看排队、进度、失败原因，取消或重试。会中自动洞察和停止后纪要走同一持久任务接口。
3. 点引用可查看精确原文范围和版本。编辑产物后重新审核，发布时填写独立公开内容；无效、部分完成、私有专项任务不能绕过 core 发布规则。
4. 大屏只展示已发布投影。授权链接仅适用于同一台电脑，8 小时到期，可主动撤销；刷新恢复同场授权，每 2 秒整体替换，断线清空旧画面。手机/LAN 大屏属于后续工作。
5. 纪要可导出 Markdown、HTML、JSON；HTML 可在浏览器打印。宿主同时保存到 `~/Library/Application Support/AI Vision Forum/exports/`，界面显示保存路径。

旧导入格式每行是 `{t_start,t_end,speaker_id,lang,text}`，时间为秒，最多 16 MiB/10000 行。先校验整个文件，再一次事务导入；同活动同文件 SHA 去重，保留旧 speaker 标签但不冒充新的说话人识别结果。由于没有可验证录音与 producer 封存，旧记录保持不完整，只能生成部分私有草稿。

## 5. 本机验证证据

所有本轮模型探针使用合成文本，没有开启麦克风或系统采音，没有下载新模型权重。下面的数值是开发验证，不是正式 C1–C10 结论。

| 验证 | 结果/证据 |
|---|---|
| core/contracts | 最终 63 core + 4 contracts 通过（含 Tauri 启用的 `preserve_order`）；生成契约 `--check` 通过；`/tmp/forum-part3-core-final.log` |
| Python | worker、打包契约及 checker 回归 57 项通过；`/tmp/forum-f05-worker-packaging-tests.log` |
| Rust runtime/gateway | 资源门控、worker 崩溃/管道/目录/hash/FIFO、5 项真实 HTTP socket 测试通过；`/tmp/forum-part3-final-tests.log` |
| 桌面宿主 | 51 项通过、1 项人工电源观察忽略；`/tmp/forum-part3-shell-tests.log` |
| UI | Svelte 0 错误/0 警告，19 项测试通过，main/overlay/display 三入口构建通过；浏览器合成预览已检查会议列表、编辑区和 UTF-8 引用弹窗 |
| 真实 tokenizer 长输入 | 2500 条混合合成记录及短尾，13 块，最大 7040 tokens，全部输入连续覆盖；`artifacts/local/f08-real-tokenizer-probe.json` |
| 独立真实 8B | 严格结构与引文验证成功；`artifacts/local/f05-analysis-probe-4/report.json` |
| Rust→8B→core 完整链路 | `/tmp/forum-analysis-m1-02/report.json`：约 41.2 秒，纪要/所选场次报告均 succeeded；草稿私有、选场隔离、隐藏撤回和依赖报告 stale 均通过 |

8B 使用本机 Qwen3-8B-4bit，文件指纹 `sha256:4e04e63018860712807e422ec05100e7a0d259e2f6422237460d666d9235c2b4`。合成样例包含“尚未批准公开发布”“12 万而非 20 万”“负责人和日期未确定”。本次保留这些关键信息，但把“尚未批准”归为 decision，并未独立提取“同意先试点”。精确引用不能证明概括语义正确；仍须人工审阅。

开发中失败证据保留：早期 Python shutdown 的 stdin 线程问题；worker 对缺失字段及变更引文的拒绝；M1-01 中跨编译配置的 JSON hash 不一致及 macOS zombie leader 回收判定问题。均已针对原因修复，没有放宽引用/完整性规则。最终整包构建及随包运行结果在下方收尾记录补充。

### 整包收尾记录

`desktop/dist/AI Vision Forum.app` 已通过官方 Tauri 开发构建、随包 checker 和 `codesign --verify --deep --strict`。manifest 列出六个分析任务及六份提示词文件 hash，同时明确 `formal_product_acceptance=not_evaluated`。

用最终 `.app` 内 Python/已安装 worker（没有源码路径覆盖），在新临时数据库再次运行完整探针：**49,750 ms，纪要与报告均 succeeded，进度均 1/1**；私有草稿隔离、所选场次范围、隐藏撤回、依赖报告 stale 四项均通过，结束后无残留模型进程。报告和三格式导出保存在 `artifacts/local/f05-f08-packaged-final/`，原目录为 `/tmp/forum-analysis-m1-packaged-final/`。

`forum-runtime` 共 11 项通过，gateway 5 项通过；最终 shell 51 项通过、1 项人工电源观察忽略。综合初次测试日志保留一个测试夹具失败：数据库 handle Drop 后立即 reopen 早于 actor 释放锁；测试改为明确等待 `core.shutdown()`，重跑宿主全部通过。它没有通过额外 sleep 隐藏竞态。

本轮没有打开用户会议库或启动实际采音。测试数据库全部为新建临时目录；构建只更新开发 `.app`，不包含模型权重。

## 6. 未通过的正式门槛

- 90 分钟真实会议停止后 180 秒内产出完整纪要（C6）未验收。短文本 41 秒和 tokenizer 覆盖通过不能替代此项。
- 实时 ASR/翻译与 8B 并发的正式延迟、长期内存、设备拔出和退出采音验收未完成；32B 真实路径未实测。
- 真实中英圆桌的结论分类、否定/数字、行动项归属盲评未完成。当前分块保留条目，跨块综合概括和去重质量尚未证明。
- 第二台 M5 Max、iPhone/Android 和原 Translator 应用共存验收仍延期。开发包是本机 ad-hoc 签名，不是正式签名/公证发行版。
- 双轨说话人、双会场、公开查询、跨机同步、正式发行和现场排练留在第四部分，不计入本轮完成。

## 7. 复现

从仓库根目录检查前端：

```bash
cd desktop/forum-shell/ui
npm run check
npm run test:p0
npm run build
```

Rust 使用已安装 stable 工具链及已有离线依赖：

```bash
cd desktop
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --locked --offline -p forum-core -p forum-contracts -p forum-runtime -p forum-gateway -p forum-shell
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable run --locked --offline -p forum-core --example generate_contracts -- --check
```

完整模型探针源码为 `desktop/forum-shell/examples/analysis_session_probe.rs`，使用新临时目录，不打开用户会议库。设置 `FORUM_MEETING_PYTHON` 为包内 Python；使用已安装模块时不设置 `FORUM_MEETING_WORKER_SOURCE`。开发源码调试时才把后者设为绝对的 `services/meeting-worker/src` 路径。

```bash
cd desktop
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable build --locked --offline -p forum-shell --example analysis_session_probe
FORUM_MEETING_PYTHON='/ABS/AI Vision Forum.app/Contents/Resources/meeting-worker/python/bin/python3.12' \
  /tmp/aivf-cargo-target/debug/examples/analysis_session_probe /tmp/forum-new-analysis-probe
```
