# Forum Meeting Worker — 本地会议与跨会场分析

这是由桌面 Rust 宿主管理的 Python 3.12 文本计算进程，支持
`insight`、`minutes`、`event_report`、`suggested_questions`、
`redaction_review`、`closing_brief` 六种任务。原文任务限定单场；报告和闭幕稿
接受宿主明确选择的 1–100 个场次的公开版本。它不导入旧 Forum
server/session 全局变量，不采集音频、不启动 Dora、不写会议数据库，也不发布内容。
所有内容先作为带引用、等待审核的草稿返回，由 core 校验和存储。

## 进程和协议

UTF-8 NDJSON JSON-RPC 2.0，单行最大 1 MiB，stdout 仅协议，stderr 仅诊断。
`initialize` 不加载模型、不创建目录。宿主提供原有 protocol_version、instance_id、
job_root、profile_id、profile_version，并可提供：

```json
{"model_grants":[{"profile":"meeting-8b-v1","model_path":"/ABS/LOCAL/SNAPSHOT","model_manifest_id":"sha256:ACTUAL_DIGEST","context_limit":8192,"max_output_tokens":1024}]}
```

默认模型为本机已有 Qwen3 8B；可选 `meeting-32b-v1` 支持内置 Qwen2/Qwen3
架构，仅在宿主授予相应本机路径和资源后使用。worker 不下载权重、不解析模型 URL、
不执行 custom model/tokenizer code。模型只在任务计算子进程中加载；实际文件指纹必须
与 host grant 一致。指纹算法与 Rust forum-runtime 相同，包含权重、tokenizer/config、
chat_template.jinja 等输入，流式只读，允许 Hugging Face 文件软链接。
这是内容指纹验证，不是完整模型注册中心。

控制进程与计算子进程分离，运行中仍可 `health.ping`、`jobs.cancel`。
计算子进程继承宿主的进程组，不调用 setsid。每个 worker 只接受一个不可变 job/attempt；
下一次尝试启动新 worker。取消通知后计算必须退出，才返回 jobs.run 的取消错误。
shutdown 的响应只是已接受退出请求，宿主仍必须确认整个自有进程组退出；超过宿主
2 秒宽限由其回收进程组。Python 自身也会有界取消/terminate/kill 自己持有的计算 child。

```json
{"jsonrpc":"2.0","id":"run-1","method":"jobs.run","params":{"job_id":"UUID","attempt":1,"kind":"minutes","session_ids":["UUID"],"snapshot":{"id":"UUID","relative_path":"input.json","sha256":"RAW_SHA256","input_cursor":42},"model_profile":"meeting-8b-v1","prompt_version":"minutes-v1","remaining_budget_ms":90000,"config":{"model_profile":"meeting-8b-v1","model_manifest_id":"sha256:ACTUAL_DIGEST","prompt_version":"minutes-v1","prompt_sha256":"RAW_SHA256","profile_id":"ai-vision-forum","profile_version":"v1","profile_sha256":"RAW_SHA256","projection_policy_hash":"RAW_SHA256","effective_config_hash":"RAW_SHA256","generation":{"temperature":0.0,"max_output_tokens":1024,"safety_tokens":128,"max_retries":1,"context_limit":8192}},"confirmed_checkpoints":{"relative_path":"checkpoints.json","sha256":"RAW_SHA256"}}}
```

宿主预先创建 `job_root/<job_id>/<attempt>/`，attempt 为当前用户拥有的 0700 目录。
input.json 是 core AnalysisSnapshot，checkpoints.json 是 core 已确认的
AnalysisCheckpoint[]，没有确认结果时写 `[]`。两者最大 32 MiB，只允许目录内文件名；
读取通过 dirfd + O_NOFOLLOW，拒绝软链接输入/attempt、越界、非普通文件及 hash 变化。
worker 固定读取快照字节，创建 result.json/checkpoint-N.json 时 0600、fsync、create-only，
不会覆盖旧 attempt 结果。它不扫描其他会议目录，也不在空选择时回退到全部会议。

`jobs.run` 延迟返回最终响应，期间发送 `jobs.progress` 和 `jobs.checkpoint` 通知。
后者包含完整 AnalysisCheckpoint，host 确认入库后可发 `jobs.checkpoint_ack`
`{job_id,attempt,step_index,result_sha256}`。仅下一次 run 的 host-confirmed 文件能授权
复用，磁盘上自行发现的 checkpoint 不会被信任。host 只查询同 event、明确 session 集合、
kind 和完整 effective config 的 core 已确认缓存。worker 按完整输入块 SHA + effective config SHA
复用，输入 hash 包含 source ID/revision、session、UTF-8 范围和实际文本。可跨新 job/snapshot
复用已满前块，追加尾句后旧尾块重算；源版本变化使对应块失效。旧 checkpoint 身份仅为来源记录，
所有引用都在当前块重新逐字校验，claim ID、coverage 和最终身份按当前 job/snapshot 重建。
最终响应包含 status、snapshot_id/sha256、result_ref 和 result_sha256。

## 分块与结果边界

真实 MLX tokenizer 对完整 chat template、系统提示、JSON 输出契约和本块输入计数。
generation.context_limit 必须等于宿主 grant，并纳入完整配置 hash。
预算扣除 max_output_tokens 和 safety_tokens；完整段优先，单段超长时按 Unicode
字符边界切分并保留原文 UTF-8 全局字节位置。输入最多 100000 单位、最多 4096 块；
达到限制显式失败，不截掉尾段。总预算从 jobs.run 开始，包含路径校验、指纹、加载、
分块和全部重试；每块最多 max_retries 次额外生成（0–2）。

模型只能返回结构化 claims 和 unit_id/逐字 quote；不接受 Markdown 修复或宽松 JSON。
worker 将引用映射成 core SourceSpan/ArtifactEvidence，要求 quote 在该块中唯一且字节精确，
要求姓名/日期归属出现在引文中，拒绝伪造来源。`cited` 仅表示有合法引用，**不表示模型
概括在语义上正确**。无引用的问题/不确定项为 unsupported，仍需审核。

每一输入字节都有 processed/failed/ignored_empty 覆盖记录；缺少 ASR、失败片段或
snapshot.input_complete=false 会保持部分结果。部分块有效、部分块失败会返回
succeeded_partial；全部计算块失败返回错误。拼接保留各块已验证条目，包括最后短块，
不把全会议再次塞进一个超长 prompt。当前是确定性分块组合，尚未承诺全场去重和高质量
综合概括；它们需要真实会议质量评估。redaction_review 只提供建议，不自动改原文。
event_report 和 closing_brief 仅接受 core 冻结的公开正文，不接受原始片段或私有字段；
每条公开输入须归属所选场次的非空子集，全部输入合起来须覆盖每个所选场次。
跨会场 owner、活动、准确发布版本和失效传播由 core 权威校验；worker 不联网查询，
不自行扩大输入，也不自动审核或发布结果。

## 验证

无模型测试包含真实管道 ping/cancel/总 deadline、路径/哈希、变更模型、完整尾段覆盖、
UTF-8 引用、已确认 checkpoint 恢复、未知配置拒绝及部分结果。`--allow-test-models`
只用于测试，启用 reserved `test-fake-v1`；生产宿主不得传该参数，fake tokenizer 不作为
真实 token 数量证据。运行：

```bash
python3.12 -m unittest discover -s services/meeting-worker/tests -v
```

手动真实 8B 合成文本探针（路径均为已有本机资源；output 必须不存在）：

```bash
python3.12 services/meeting-worker/tests/run_local_analysis_probe.py \
  --python /ABS/BUNDLE/python/bin/python3.12 \
  --model /ABS/EXISTING/Qwen3-8B/SNAPSHOT \
  --output /ABS/NEW/PROBE_DIRECTORY
```

探针记录完整配置、prompt/model SHA、实际结果、进程退出、缓存前后 size/mtime 和
诊断耗时，不开启麦克风，也不证明真实会议质量、实时性能或 32B 本机验收。

## macOS arm64 独立运行时

仓库根目录的 `desktop/scripts/package_meeting_worker.py` 生成可迁移的
`meeting-worker/` 资源目录。构建机需要 Python 3.12+、macOS arm64；
最终应用不需要用户安装 Python、uv、Conda、pip 或开发环境。
构建从固定上游归档和 wheel 开始，不复制仓库 `.venv`。
构建时从同版本 full 归档读取许可证；该归档也锁定 URL、长度、SHA-256，
使用固定的构建专用 zstandard wheel 解压，不依赖 Homebrew zstd 命令，
不将 full 归档的编译对象或解压工具打进应用。

`packaging/runtime-macos-arm64.lock.json` 固定全部下载 URL、字节长度、
SHA-256 和版本：

- CPython 3.12.11，Astral `python-build-standalone` 的 `20250612`
  `aarch64-apple-darwin-install_only` 发行，与最初 uv 管理的开发解释器版本一致。
- MLX 0.32.2 / mlx-metal 0.32.2，明确选 macOS 14.0 arm64 wheel，
  不让 macOS 27 构建机自动选仅支持 26 的 wheel。
- mlx-lm 0.31.3、Transformers 5.17.0、NumPy 2.4.6，及其完整的 34 个
  运行依赖 wheel。构建工具另锁 setuptools 80.9.0、wheel 0.45.1、zstandard 0.25.0。

归档来源和用途见 [Astral 官方发行说明](https://github.com/astral-sh/python-build-standalone/releases/tag/20250612)
与 [分发文档](https://github.com/astral-sh/python-build-standalone/blob/main/docs/distributions.rst)；
MLX 和 MLX-LM 的官方包信息见 [MLX 0.32.2](https://pypi.org/project/mlx/0.32.2/)
和 [MLX-LM 0.31.3](https://pypi.org/project/mlx-lm/0.31.3/)。每个传递依赖的
PyPI metadata 地址也记录在锁文件中。以后升级依赖必须显式更新锁和重新验证。

在仓库根目录运行，例如：

```bash
mkdir -p '/tmp/forum-worker-candidate/AI Vision Forum.app/Contents/Resources'
/absolute/path/to/python3.12 desktop/scripts/package_meeting_worker.py \
  --output '/tmp/forum-worker-candidate/AI Vision Forum.app/Contents/Resources/meeting-worker' \
  --cache /tmp/forum-worker-artifacts
```

首次只下载解释器和软件依赖，**不下载模型权重**。有缓存后加 `--offline` 可
断网重建；输出必须换成一个尚不存在的目录。现有目录、根目录、`..` 路径、
输出符号链接、缓存文件符号链接或不匹配哈希都会被拒绝。脚本不支持覆盖或
删除已有产物；递归清理只发生在脚本自己新建的随机 staging 目录。

固定 `SOURCE_DATE_EPOCH`、固定构建工具和按哈希安装保证输入可重建；
`runtime-manifest.json` 记录 worker 源文件及生成 wheel 的 SHA-256。
此处不承诺签名、公证、DMG 或文件系统时间戳逐字节一致。

资源目录结构为：

```text
meeting-worker/
  bin/meeting-worker             # 只使用相对资源路径的启动器
  python/bin/python3.12          # 独立 CPython
  python/lib/...                 # 标准库、MLX dylib/metallib、全部运行 wheel
  runtime-manifest.json          # 版本、来源、哈希、源文件校验
  THIRD_PARTY.md                  # 依赖来源与许可证索引
  third-party/python/            # 官方 PYTHON.json 和完整 Python 依赖许可证
  checks/check_meeting_worker_bundle.py
```

根构建脚本将整个目录映射到 `.app/Contents/Resources/meeting-worker`。
宿主以资源根解析 `bin/meeting-worker` 并持有其标准管道；也可直接执行
`python/bin/python3.12 -I -B -m forum_meeting_worker`。入口不含源码目录、
Conda 或构建 staging 的绝对路径。上游原生库、Metal 资源和 wheel 许可文件
保留在原位置；未对这些库重新签名。普通握手不导入 MLX。

`check_meeting_worker_bundle.py` 验证资源路径、35 个已安装包的精确版本、
Python 隔离标志、`sys.path`、原生库加载路径和真实协议子进程：

```bash
/absolute/path/to/python3.12 desktop/scripts/check_meeting_worker_bundle.py \
  --app '/tmp/forum-worker-candidate/AI Vision Forum.app' \
  --report /tmp/worker-handshake-result.json
```

检查器用 Python 标准库读取 Mach-O/FAT，验证 arm64、最低 macOS 版本与
加载路径，**不依赖 Xcode、CLT 或 otool**。默认还独立执行一次 2×2 float32
Metal 矩阵乘法、`mx.eval` 和 GPU synchronize，校验真实 GPU 设备与数值；
不加载大模型。检查子进程用环境白名单，
清除开发 PATH、PYTHONPATH、PYTHONHOME、Conda、venv、DYLD、HF token 等，
从新的临时工作目录启动，HF 缓存也隔离。报告路径必须是新文件，防止覆盖
已有证据。这些是**本机隔离环境验证，不是第二台干净 Mac 的验收**。
第二台 Mac 可以直接用包内 Python 执行随包检查器，例如把资源目录记为
`FORUM_WORKER_RESOURCE` 后执行：

```bash
"$FORUM_WORKER_RESOURCE/python/bin/python3.12" -I -B \
  "$FORUM_WORKER_RESOURCE/checks/check_meeting_worker_bundle.py" \
  --resource-dir "$FORUM_WORKER_RESOURCE" --report /tmp/forum-worker-clean-mac.json
```

## 仅用于 F01 的模型探针

默认不存在 `diagnostics.model_probe` 方法。只有显式传
`--allow-model-probe`，完成 initialize 后，才可发送：

```json
{"jsonrpc":"2.0","id":"probe-1","method":"diagnostics.model_probe","params":{"model_path":"/ABSOLUTE/EXISTING/QWEN3/SNAPSHOT","prompt":"请用一句中文概括这段合成测试文本。","max_tokens":64}}
```

仅接受已有的绝对本地 Qwen3 目录、1–4096 字符 prompt、1–128 输出 token；
要求本地 tokenizer 和完整权重文件列表，拒绝模型 ID、缺失权重、路径越界的
权重索引、`model_file`/`auto_map` 自定义代码。HF snapshot 到已有 blob 的符号
链接可只读复用。探针强制离线、关闭远程代码和隐式令牌，既不修复也不写入
模型目录。stdout 连原生库 fd 1 输出也重定向保护，日志走 stderr。

响应有 `diagnostic_only: true`、实际生成文本、版本、解释器/模型路径、模型
加载时间、首 token 时间、总时间、输出 token 数、吞吐和峰值内存。时间从
探针调用开始计算，包含首次库导入；不等同于常驻模型延迟。可用检查器执行：

```bash
/absolute/path/to/python3.12 desktop/scripts/check_meeting_worker_bundle.py \
  --app '/tmp/forum-worker-candidate/AI Vision Forum.app' \
  --model '/absolute/existing/Qwen3-8B-4bit/snapshot' \
  --max-tokens 64 --timeout 180 --report /tmp/worker-mlx-result.json
```

诊断计算同步阻塞该专用进程，检查器在超时后终止进程。这不是正式任务调度，
没有任务级取消、恢复、事件引用、会议摘要或 JSON 质量保证，也不会改变
initialize 的正式 capabilities。不要将探针作为生产 `jobs.run` 的替代品。
正式任务请使用前述 F05/F08 jobs.run；资源准入与整个进程组的实际退出由 Rust 宿主管理。

## 验证范围

最初 F01 的无模型测试覆盖原协议生命周期/帧边界，以及探针默认关闭、输入范围、
拒绝自定义代码和缺权重、原生 stdout 保护、输出目录保护、缓存哈希/符号链接。
Mach-O 合成样本另验证 thin/FAT arm64、截断数据、外部开发库路径和系统下限。
真实本机包探针已从临时 `.app` 资源目录运行已有 Qwen3-8B-4bit；证据说明见
`packaging/LOCAL_VALIDATION.md`。这只证明该依赖组合能加载并推理，不代表
翻译/纪要质量通过、正式应用打包已签名、公证通过或跨机器支持验证完成。

独立 wheel 仍可用于开发：

已有 Python 3.12 环境包含 setuptools 时，可离线验证 wheel 构建：

```bash
uv build --wheel --offline --no-build-isolation --python /absolute/python3.12
```
