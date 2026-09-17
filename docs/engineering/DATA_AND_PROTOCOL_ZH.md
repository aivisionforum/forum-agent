# Forum 数据模型与进程协议 v1

配套：[主工程方案](VISION_FORUM_ENGINEERING_PLAN_ZH.md)、[实施任务](IMPLEMENTATION_TASKS_ZH.md)。本文定义目标 v1 契约；示例仅使用合成内容。F02–F08 已实现单机原译文、分析任务、审核发布和 loopback 大屏；跨机同步、远程查询及双轨说话人仍是后续目标。准确字段以 `forum-contracts` 生成的 TS/schema 为准，实际入口和验证边界见 [F05–F08 本机记录](F05_F08_LOCAL_VALIDATION_ZH.md)。

### 2026-09-16 单机实现约定

- SQLite 迁移版本为 5，传输 `schema_version`/worker `protocol_version` 仍为 1，两者不混用。`analysis.rs` 集中实现任务、快照、checkpoint、artifact、review 和 publication；文中拆文件名称是职责划分。
- 分析种类使用单数 `insight`，其余为 `minutes/event_report/suggested_questions/redaction_review/closing_brief`。六类计算可用不等于完整活动及公开产品验收通过。
- 默认 `meeting-8b-v1`，宿主可显式指定 `meeting-32b-v1` 的本机路径。initialize 的 `model_grants` 决定允许的路径、实际模型文件指纹和 context/output 限制；没有 grant 不运行模型。前端不提供这些路径。
- `jobs.run` 必须附完整 `AnalysisConfig` 和宿主确认的 `confirmed_checkpoints` 文件描述；完整报文见 [worker README](../../services/meeting-worker/README.md)。generation 固定包含 `temperature/max_output_tokens/safety_tokens/max_retries/context_limit`。model manifest 使用 `sha256:` 前缀，其余 SHA 是 64 位小写十六进制。
- 配置/语义快照采用递归键排序、紧凑 UTF-8 JSON 后计算 hash，不能依赖 serde_json map 的编译特性。传输快照和结果另外验证实际文件字节 hash。宿主单文件上限 16 MiB，worker 独立入口上限 32 MiB；桌面有效限制为前者。
- core 确认 checkpoint 后才 ACK。跨 job/snapshot 缓存限定同活动、同明确场次集合、同 kind/config；每个块还校验来源 ID/revision/字节范围和全文 hash。追加原文后可复用满前块，变化的尾块重新计算。
- Runtime 的 Started/Stopped 直接通知分析模块，不依赖可覆盖的 UI 状态消息。Stopped 同步登记持久纪要 intent，重启后再构造有预算的 job；新一次停止更新 marker，旧 ACK 不能删除它。退出只有在实时资源、分析进程和 intent 保存均确认后完成。
- 当前大屏每 2 秒读取完整公开快照并整体替换；断线清空旧画面。仅 loopback GET，使用带有效期的场次 token；没有实现文中目标的远程控制、SSE 和跨机接口。
- 旧 JSONL 的 `t_start/t_end` 以秒解释，`legacy_import` 为来源类型；保留文件 hash/行号/旧 speaker 标签。没有原录音封存证明，导入保持不完整，只产生部分私有草稿。

## 1. 数据流与基本不变量

1. Rust `forum-core` 是会议数据库唯一写入者；UI、Dora nodes、Python worker 和远端设备通过明确的入口提交事件/结果。
2. 原文 final 保存与翻译成功无关；声纹分配、译文和 AI 结论都引用已存在的原文版本。
3. stable ID 表示同一实体，revision 表示该实体的不可变版本；内容修订不能覆盖旧行。
4. 持久事件在提交后广播；一次事务同时写事件、当前投影、依赖关系和发布 outbox。网络交付允许至少一次，消费者必须幂等，不承诺分布式 exactly-once。
5. 单机 `store_seq` 是数据库接收顺序，不是发言时间；字幕按 session 音频时钟排序。同机/跨轨允许真实重叠；跨机器不假设时钟精确一致。
6. 公开消息来自独立的发布投影，不能直接把内部事件去掉几个字段就全部转发。
7. 合同版本不兼容时拒绝启动对应组件并说明原因，不尝试静默解析成旧结构。

## 2. 时间、ID 与修订

| 字段 | 语义 |
|---|---|
| `event_id`（活动实体） | 一次论坛活动 UUID；传输事件用 `message_id`，避免同名混淆 |
| `room_id` | 活动内会场 UUID；显示名如 Room A，不用显示名作主键 |
| `session_id` | 创建会议时生成的 UUID；停止/归档不更换 |
| `track_id` | session 内声源 UUID；mic/system/room_mix/replay 是属性 |
| `segment_id` | VAD 开始新音频段时生成；该段所有 partial/final 共享 |
| `revision` | 稳定原文版本从 1 开始；人工纠错、重新识别产生新版本 |
| `partial_seq` | 段内临时快照递增编号；不与持久 revision 混用 |
| `producer_run_id` | 每次生产者进程启动生成 UUID；与 `producer_seq` 构成局部唯一键 |
| `message_id` | 每条可重发的持久事件 UUID；重复发送保留该值 |
| `store_seq` | core 分配的本机全局递增游标；客户端按 session 过滤可能看到跳号 |
| `direction_epoch` | 翻译方向设置版本；切换后旧输出不能覆盖新方向结果 |

音频位置使用处理流的 `start_sample/end_sample/sample_rate`，区间是 `[start,end)`；另存 raw recording 引用和采样率映射。session 时间使用共同 monotonic 时钟的 `start_ms/end_ms`。设备暂停/拔插或重采样漂移必须记录 gap/clock mapping，不能删除空白时间使音频看似连续。UTC 时间只用于展示与跨设备粗对齐，不用于 ASR 延迟或段排序。

原文 final v1 已存在时，任何旧 partial 都丢弃。人工修订使用 `expected_revision`，不匹配返回冲突。修订操作保留旧原文及来源，产生新 revision，失效依赖旧版本的译文和分析结果。

## 3. 内部事件契约

Rust `forum-contracts` 定义类型并生成 JSON Schema 与 TS 类型；Python 在入口按同版本 schema 验证。以下是传入 core 的持久事件示例，core 成功保存后回传 `store_seq`。ID 均为示例 UUID。

```json
{
  "schema_version": 1,
  "message_id": "10000000-0000-4000-8000-000000000001",
  "type": "transcript.final",
  "event_id": "20000000-0000-4000-8000-000000000001",
  "room_id": "30000000-0000-4000-8000-000000000001",
  "session_id": "40000000-0000-4000-8000-000000000001",
  "producer": {
    "name": "asr",
    "run_id": "50000000-0000-4000-8000-000000000001",
    "seq": 23
  },
  "payload": {
    "track_id": "60000000-0000-4000-8000-000000000001",
    "segment_id": "70000000-0000-4000-8000-000000000001",
    "revision": 1,
    "audio": {
      "start_sample": 32000,
      "end_sample": 80000,
      "sample_rate": 16000,
      "start_ms": 2000,
      "end_ms": 5000
    },
    "text": "我们先测试本地字幕。",
    "configured_source_language": "auto",
    "detected_language": "zh",
    "target_languages": ["en"],
    "direction_epoch": 1,
    "speaker_id": null,
    "status": "success",
    "backend": "qwen-asr-mlx",
    "model_manifest_id": "asr-baseline-v1"
  }
}
```

`status` 可为 `success/empty/failed`。empty/failed 记录音频范围和原因，不加入正常逐字稿，也不假装该段没有发言。模型和运行时精确版本通过不可变 manifest 关联。所有文本和日志作为数据处理，不执行其中指令。

事件按类型校验 scope：音频、原文、译文、单场状态必须有 event/room/session；活动级报告和跨场 job 的 `room_id/session_id` 可以为空，但必须有 event ID 和显式的 `session_ids`/artifact 输入集合。不能把活动级结果随便挂到某个当前 session 上；数据库和生成 schema 都必须表达这一区别。

| 事件 | 持久化 | 关键载荷/规则 |
|---|---|---|
| `transcript.partial` | 默认不持久化 | segment ID、partial_seq、音频范围、临时文本；可合并旧快照 |
| `transcript.final` | 是 | 原文 revision、范围、语种、状态；必须确认落盘 |
| `transcript.revised` | 是 | 新/旧 revision、修改原因、操作来源；失效依赖 |
| `translation.requested` | 是 | translation ID、revision、输入 spans、目标语种、epoch |
| `translation.partial` | 不持久化 | translation ID、revision、attempt、chunk_seq、累计文本；替换显示，不重复追加 |
| `translation.final` | 是 | 引用、译文、模型、耗时；只接受当前有效 attempt |
| `translation.failed` | 是 | 有界错误码、重试信息；原文照常存在 |
| `speaker.assigned` | 是 | segment、assignment revision、匿名 speaker、置信/未知原因 |
| `audio.segment_closed` | 是 | capture 侧预期闭合段 ID、音频范围、录音引用/无录音标记；先登记再分派 final ASR |
| `audio.gap` | 是 | track、采样/时间范围、原因、是否可用录音恢复 |
| `session.capture_stopped` | 是 | 各 track 最终采样位置、预期段清单及 hash；设备已释放 |
| `session.transcript_sealed` | 是 | 各 producer 的最终游标、未完成/失败段清单 |
| `job.changed` | 是 | job ID、attempt、状态、进度；不携带整份内容 |
| `artifact.revised/reviewed` | 是 | artifact/revision、审核操作及依赖版本 |
| `publication.changed` | 是 | 发布/撤回/过期版本；进入受限发布 outbox |

持久事件入口使用有界队列，ASR node 先把 final 写入自己所属的本地 outbox，再发送给 core。只有收到对应 `message_id` 的 durable ack 才清理 outbox。core 写入失败不 ack；重启后重发，唯一约束消除重复。相同 message ID 携带不同正文 hash 时返回 `EVENT_ID_CONFLICT`，不能覆盖已有事件。outbox 也写盘失败时记录可见的 data-loss 状态，按策略停止新采集并保留可恢复录音，不能无声降为内存模式。

旧消息重发保留原 message ID、producer run/seq 和正文；新的 run ID 只用于重启后新生成的消息。capture 层还必须先持久登记 `audio.segment_closed`，然后才分派该段的 final ASR；partial 可更早运行。否则 ASR 取走音频后、生成 final 前崩溃，该段会完全不在 final outbox 中。采集停止给出全部预期段的清单/hash；未正常闭合的段/track 在恢复时明确标记 interrupted。关闭录音也要登记预期段，仅将无法重放的音频标为不可恢复缺口。

partial 走独立可丢弃通道；final 队列拥塞不能阻塞实时音频回调，录音线程和持久化队列必须独立。F03/F04 验证 Dora 动态节点的 ack 返回通道；若现有图接口不便表达，使用 core 创建的受限本机 IPC 实现 ack，行为契约不变。

### 3.1 翻译来源 spans

单条译文可以覆盖多个原文段，也可以只覆盖一段的一部分。偏移统一为 **UTF-8 字节偏移**，必须位于字符边界；Python/TS 用统一辅助函数转换，禁止混用 JS UTF-16 索引。

```json
{
  "translation_id": "80000000-0000-4000-8000-000000000001",
  "revision": 1,
  "attempt": 1,
  "target_language": "en",
  "direction_epoch": 1,
  "source_spans": [
    {
      "segment_id": "70000000-0000-4000-8000-000000000001",
      "segment_revision": 1,
      "start_utf8": 0,
      "end_utf8": 30,
      "quote": "我们先测试本地字幕。"
    }
  ],
  "input_text": "我们先测试本地字幕。",
  "normalization_version": "identity-v1",
  "text": "Let's test the local subtitles first.",
  "status": "final"
}
```

实现 `AttributedBuffer`：每个 piece 保存原文 revision、源起止字节、文本及插入分隔符标记。append、trim、标点切分和 hard cut 同时变换 piece 范围；提交时返回精确 spans 与实际送入模型的 input_text。插入空格是合成分隔符，不伪装成原文引用。v1 优先保留原标点，取消现有不必要的有损截断；确需规范化时记录变换版本和偏移映射。

同一 translation ID 的模型重试递增 attempt，不制造第二条字幕；修改来源、目标语言或生成策略时产生新的 translation revision/实体关系。原文修订使依赖旧 revision 的译文 stale；新译文完成前 UI 可显示旧版本但必须明确过期，不允许冒充当前版本。纯同语种透传使用明确状态，不运行一次无用翻译再隐藏。

ASR 持久化旁路与翻译并发，可能出现译文先到 core。来源 revision 尚未写入时返回可重试的 `DEPENDENCY_NOT_READY`，译文保留在 producer outbox，待 ASR durable ack 后重试；不丢弃译文或插入伪造原文满足外键。若来源最终失败，结束时记录缺失依赖的失败任务，不能宣称已完成对齐。

ASR final 提交事务同时为所需目标语言创建 `translation_coverage` 待处理范围，保存 source revision/epoch 和完整源跨度。`translation.requested` 将范围关联到具体任务；最终完成再标记覆盖成功。core 可以从这份 ledger 重建尚未 commit 的 AttributedBuffer 尾部。停止时 flush 剩余文本，必须先将目标语言与所有未覆盖 spans 持久登记并收到 ack，再结束 translator；超时/崩溃也不会让短句因未达到长度或标点门槛而失去恢复任务。

## 4. 本地存储设计

建议位置：应用数据目录下 `store/forum.sqlite`、`sessions/<session_id>/audio/`、`jobs/<job_id>/<attempt>/`、`exports/`、`logs/`。不得写入签名 .app Resources 或仓库 `data/` 作为正式产品存储。

| 表/集合 | 核心字段与约束 |
|---|---|
| `events` / `rooms` | 活动 ID、profile version；room 属于 event |
| `sessions` | ID、event/room、owner device、title、state、起止时间、sealed cursor、完整性状态 |
| `tracks` | ID、session、source kind、设备描述、时钟映射、采样参数 |
| `segments` | ID、session、track、current_revision；独立记录处理状态 |
| `capture_segments` | 预期闭合段、音频引用/范围、dispatch 状态、ASR 结果 receipt；用于逐段 seal 对账 |
| `segment_revisions` | `(segment_id, revision)` 主键，文本、范围、语种、模型、来源、created_seq |
| `speaker_assignments` | segment + assignment revision、speaker、方法、置信、人工修订；不覆盖旧值 |
| `translations` / `translation_revisions` | 实体 ID、current revision；各版本正文、target、epoch、attempt、状态 |
| `translation_spans` | translation/revision + order；外键指向准确 segment revision 和源跨度 |
| `translation_coverage` | source revision/target/epoch 的预期范围与 pending/requested/final/failed 状态；不以已有请求数代替覆盖 |
| `ingested_events` | store_seq、自定义 message_id 唯一、producer run/seq 唯一、body hash、载荷 |
| `snapshots` / `snapshot_segments` | ID、截止 store_seq、segment revision 集合、正文 hash、选择条件 |
| `jobs` / `job_attempts` | job 类型、会场/场次范围、快照、幂等键、优先级、状态、deadline、attempt |
| `job_steps` | 分块编号、输入 hash、状态、结果 hash；用于断点重算 |
| `artifacts` / `artifact_revisions` | 洞察/纪要/报告等稳定 ID 与版本；正文、model/prompt/profile、validation 状态 |
| `artifact_sources` | segment revision、speaker assignment 或其他 artifact revision 的依赖边 |
| `review_actions` | artifact revision、操作、操作员 ID/本机操作者、时间、原因、预期版本 |
| `publications` / `publication_events` | 当前发布版本、公开文本、授权范围、发布游标、撤回/过期事件 |
| `public_evidence` | 经审核脱敏的公开引用文本及版本；内部映射指向真实来源，不把内部全文透传出去 |
| `outbox` | 事务提交后的通知/同步消息、目的地、重试及确认游标 |
| `peer_cursors` | 设备身份、来源发布游标、快照版本、最后成功同步时间 |
| `remote_publications` / `remote_public_evidence` | 来源设备、公开 ID/revision、脱敏正文、依赖及接收游标；不要求远端原文存在于本地 |
| `imports` / `export_manifests` | 原数据指纹、导入映射；导出源版本、格式、文件 hash |

FK 必须启用；一个 session 的 segment 不能被另一 session 的 translation 假借引用。多字段 session 一致性在 SQL 复合外键或同事务校验中强制，不能只由 UI 约定。schema migration 有版本，启动升级前备份；旧程序遇到更新的 schema 时只读提示或拒绝打开，不能降级覆盖。

下面是部分核心 DDL 草案，完整迁移在 F02 实现并测试；其余表按上面的关系补齐：

```sql
CREATE TABLE ingested_events (
  store_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL UNIQUE,
  event_id TEXT NOT NULL REFERENCES events(id),
  session_id TEXT REFERENCES sessions(id),
  producer_run_id TEXT NOT NULL,
  producer_seq INTEGER NOT NULL CHECK (producer_seq >= 0),
  event_type TEXT NOT NULL,
  body_sha256 TEXT NOT NULL,
  body_json TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  UNIQUE (producer_run_id, producer_seq)
);

CREATE TABLE segment_revisions (
  segment_id TEXT NOT NULL REFERENCES segments(id),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  text TEXT NOT NULL,
  start_sample INTEGER NOT NULL CHECK (start_sample >= 0),
  end_sample INTEGER NOT NULL CHECK (end_sample >= start_sample),
  sample_rate INTEGER NOT NULL CHECK (sample_rate > 0),
  created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
  source_json TEXT NOT NULL,
  PRIMARY KEY (segment_id, revision)
);

CREATE TABLE translation_spans (
  translation_id TEXT NOT NULL,
  translation_revision INTEGER NOT NULL,
  span_order INTEGER NOT NULL CHECK (span_order >= 0),
  segment_id TEXT NOT NULL,
  segment_revision INTEGER NOT NULL,
  start_utf8 INTEGER NOT NULL CHECK (start_utf8 >= 0),
  end_utf8 INTEGER NOT NULL CHECK (end_utf8 >= start_utf8),
  PRIMARY KEY (translation_id, translation_revision, span_order),
  FOREIGN KEY (translation_id, translation_revision)
    REFERENCES translation_revisions(translation_id, revision),
  FOREIGN KEY (segment_id, segment_revision)
    REFERENCES segment_revisions(segment_id, revision)
);
```

core 事务示意：

```text
ingest(event):
  validate_schema_and_session_ownership(event)
  begin transaction
  if message_id already exists:
    require same body hash; return original receipt
  insert ingested_event; allocate store_seq
  apply revision-aware projection
  invalidate affected dependencies and enqueue retraction if necessary
  append durable outbox entries
  commit
  return ack(message_id, store_seq)
```

若进程在 commit 后、ack 前退出，producer 重发会得到同一 receipt；若在 commit 前退出，则整个事务回滚。UI 广播失败通过 outbox/replay 恢复，不能撤回已成功保存的原文。

## 5. Python worker 协议

使用 JSON-RPC 2.0 对象，每行一个 UTF-8 JSON。stdout 仅协议，stderr 仅日志。为读取器设置行长度上限（初值 1 MiB）、队列和解析超时；大输入通过宿主创建的不可变快照文件传递，不塞进单行控制消息。RPC id 表示一次调用，job ID 表示业务任务，attempt 表示一次执行，三者不混用。

初始化握手：

```json
{"jsonrpc":"2.0","id":"init-1","method":"initialize","params":{"protocol_version":1,"instance_id":"90000000-0000-4000-8000-000000000001","job_root":"/ABSOLUTE_APP_DATA/jobs","profile_id":"ai-vision-forum","profile_version":"v1"}}
```

worker 返回实际支持的协议版本、任务类型、build version 和模型客户端能力；不在 initialize 时加载大模型。宿主决定 worker 的可执行路径、资源路径、模型 endpoint/凭证，前端和任务正文不能提供任意可执行路径或 URL。

```json
{
  "jsonrpc": "2.0",
  "id": "run-1",
  "method": "jobs.run",
  "params": {
    "job_id": "a0000000-0000-4000-8000-000000000001",
    "attempt": 1,
    "kind": "insight",
    "session_ids": ["40000000-0000-4000-8000-000000000001"],
    "snapshot": {
      "id": "b0000000-0000-4000-8000-000000000001",
      "relative_path": "input.json",
      "sha256": "SHA256_OF_EXACT_INPUT_BYTES",
      "input_cursor": 1842
    },
    "model_profile": "meeting-8b-v1",
    "prompt_version": "insight-v1",
    "remaining_budget_ms": 90000
  }
}
```

示例 hash/路径为占位值；上述仅展示字段分组，省略了必填的 `config` 与 `confirmed_checkpoints`，不能直接作为请求执行。完整报文参见 worker README。实际实现要求合法 SHA-256 和已创建的绝对 job 根目录。`relative_path` 相对该 job/attempt 目录，不允许绝对路径、`..` 或越界 symlink。大报告的 snapshot 包含多个 session/已发布 artifact 的准确版本集合，不能只靠一个全局 input_cursor 表示全部输入。

`remaining_budget_ms` 是整个 attempt 剩余预算，worker 用 monotonic clock 扣减；每次模型请求和重试只能使用剩余预算，不能每次重置。core 自己也保留总截止时间，防 worker 卡死。后台暂停超过原预算后继续，创建新 attempt 和可见的新预算，不能悄悄突破原任务时限。

```json
{"jsonrpc":"2.0","method":"jobs.progress","params":{"job_id":"a0000000-0000-4000-8000-000000000001","attempt":1,"phase":"extracting","completed_units":2,"total_units":6}}
```

```json
{"jsonrpc":"2.0","id":"run-1","result":{"job_id":"a0000000-0000-4000-8000-000000000001","attempt":1,"status":"succeeded","snapshot_id":"b0000000-0000-4000-8000-000000000001","snapshot_sha256":"SHA256_OF_EXACT_INPUT_BYTES","result_ref":"result.json","result_sha256":"SHA256_OF_EXACT_RESULT_BYTES"}}
```

结果先写临时文件再原子改名。core 在事务外读取固定结果字节并检查路径、大小、hash 和 schema；在最终写事务内重新检查 job/attempt 仍可提交、未取消、快照身份和引用来源当前有效，再创建 artifact revision 并更新 job/outbox。取消或原文修订先提交时，迟到结果不能成为当前有效版本；不得出现“验证后、提交前”绕过失效传播的间隙。worker 的 succeeded 仅表示计算完成；core 验证未通过时业务 job 仍失败。结果文件不自动成为用户导出文件。

| method | 行为 |
|---|---|
| `initialize` | 版本/能力握手；不录音、不加载全部模型 |
| `health.ping` | 快速返回进程健康；不把模型推理完成当健康条件 |
| `jobs.run` | 执行一个有资源许可的 attempt；忙时返回 RESOURCE_BUSY |
| `jobs.cancel` | 按 job/attempt 发取消；返回请求已接收，另发最终 cancelled |
| `shutdown` | 停止接新任务，限时收尾并退出 |

reader/control loop 必须与任务执行分离，生成期间仍能接收 cancel/ping。不可中断的模型请求最终由 core 结束专用模型进程；worker 不自己扫描/杀进程。取消后的旧 result 可以保留为诊断文件，但不能写入已发布投影。

统一错误码：`UNSUPPORTED_PROTOCOL`、`INVALID_SNAPSHOT`、`EVENT_ID_CONFLICT`、`REVISION_CONFLICT`、`DEPENDENCY_NOT_READY`、`MODEL_UNAVAILABLE`、`RESOURCE_BUSY`、`DEADLINE_EXCEEDED`、`CANCELLED`、`INVALID_MODEL_OUTPUT`、`WORKER_EXITED`、`STORAGE_UNAVAILABLE`。JSON-RPC error 使用标准整数 code，业务错误码放 `error.data.code`，UI 映射为中文状态。

## 6. 作业、资源许可和停止收尾

持久 job 状态：

```text
queued -> waiting_resources -> running -> validating -> succeeded
                                              +----> succeeded_partial
                                  |          |
                                  +------> failed
                                  +------> retry_wait -> queued
queued/waiting/running -> cancel_requested -> cancelled
running/validating --进程异常--> interrupted -> queued 或 failed
```

`succeeded` 需要有效结果与完整 coverage；存在失败分块但有有效部分结果时，job 终态为 `succeeded_partial`，保存 `coverage=partial` 的内部草稿，UI 显示“部分完成”。全部失败或格式无效为 failed。重试 partial/failed/interrupted 时回到 queued 并增加 attempt，可复用仍有效的 checkpoint；更换输入或配置则创建新 job。Job 表保存幂等键、目标实体、不可变快照、当前 attempt、总预算、重试上限、进度、完成块、错误和最终 artifact revision。

自动洞察幂等键包含 `session + kind + input snapshot hash + model + prompt + profile`。同一 session 最多一个自动洞察 running，等待期间的新请求合并到最新输入；手动刷新也走同一调度器。旧结果绑定原 session，可保存为历史快照；若已有更晚当前结果，不能覆盖新面板。

后台准入伪代码：

```text
on start_session:
  stop admitting batch jobs
  cancel/terminate owned batch computation within bounded grace
  confirm process exit and live model readiness
  start capture; expose preparing state until ready

on background_tick:
  if capture unhealthy or source persistence behind: do not admit
  if live latency/backlog exceeds calibrated budget: pause background
  if memory/model residency not allowed: keep waiting with reason
  else run one bounded step with total remaining deadline

on worker_result:
  validate fixed result bytes, hash, schema and evidence shape
  in final write transaction:
    require expected attempt, not cancelled and matching snapshot
    recheck current source versions
    persist version and job state or reject as stale/failed
  never choose latest session
```

第一版 grace/timeout 作为测试配置而非散落常量：采样停止反馈目标 ≤1 秒；后台合作取消 grace 初值 2 秒，随后终止自有进程；总退出收尾初值 15 秒，尾部识别有独立最大预算。真实机器验证后固定发布值。超时先记录任务和数据状态，再释放资源；不能为等“完美尾句”让麦克风一直开着。

停止不能依赖 queue.empty：以 capture seal 的全部预期段为基准，对账每段的 success/empty/failed/待恢复状态和 durable receipt，同时记录 accepted final、inflight ASR 和 producer seal。不能只核对已经产生 final 的段。所有预期段有终态且收尾记录持久化后才发 transcript sealed；failed/缺口使完整性为 incomplete，即使存在 seal 也不能显示完整逐字稿。

翻译按 coverage ledger 对账，未生成请求的短句尾部也必须有可恢复归属；已有请求完成不等于所有原文都有译文。译文待处理可跨 session 保留，新会话不复用旧 context/epoch。采集已停止但尾句识别失败的 session 能继续查已有原文并重处理可用录音；无录音的缺口保留，不能凭摘要补造发言。

## 7. 证据、审核与发布

artifact 类型至少包括 `insight / minutes / event_report / suggested_questions / redaction_review / closing_brief`。每个 revision 记录 profile、prompt、model manifest、生成时间、快照、结构化正文、evidence 和 coverage。

状态分三个维度，避免把“引用存在”误当“审核通过”：

- `validation_status`: pending / valid / invalid / stale。
- `review_status`: draft / approved / rejected；审核只针对一个 revision。
- `publication_status`: private / published / hidden / withdrawn。

`can_publish = validation valid AND review approved AND source_versions current AND coverage complete AND projection_policy satisfied`。检查与发布、outbox 写入必须在同一最终事务内完成。主持人建议始终 private。按需求展示“自动生成草稿”只能在操作台实现；公开洞察墙不沿用旧 auto_approve 路径。

证据结构是 `{segment_id, segment_revision, start_utf8, end_utf8, quote}`，或引用已发布 artifact 的准确 revision。core 从不可变来源提取字节并核对 quote，范围越界、字符边界错误、空来源均拒绝。允许展示归一化引文时必须同时保存实际 source span 与规范化规则，不用全文模糊匹配代替准确引用。

上述完整证据结构是内部契约。公开产物只含 `public_evidence_id/revision + 已审核脱敏文本`，不直接复制内部 quote、路径或检索上下文。core 保留公开证据到真实来源的映射；公开引用的有效性与内部来源版本一起失效。公开检索只索引发布投影，snippet/highlight、snapshot 和跨场同步遵循同一过滤规则。

审核命令携带 `expected_revision` 与当前 validation/source 状态；冲突返回最新版本供操作员重新检查。修改正文新建 revision，旧批准不继承。来源修订或撤回沿依赖图使下游 artifact stale，并在同一事务加入撤回 outbox。已导出的静态文件无法远程收回，应用记录其来源已变更，并要求重新导出。

纪要可从全部稳定原文生成草稿，并明确区分模型候选与已确认决策；公开发布仍需审核。活动报告默认输入已发布纪要/洞察，不回退草稿。跨场合并必须保留各来源版本，不以相同文本或 Speaker A 标签自动合并身份。

## 8. UI 与网关接口

桌面读写通过 Tauri commands；Rust command 层只转换 DTO、检查版本/权限并调用 core。长操作返回 `{job_id,state}`，不让一次 IPC/HTTP 请求等几分钟。

| 桌面 command（拟定） | 输入要点 | 返回 |
|---|---|---|
| `preflight_run` | profile、音源/模型配置 | checks、blocking、warnings |
| `session_create/start/stop` | session/config 或 session + expected state | 当前状态；收尾异步事件 |
| `session_snapshot` | session + view + 分页范围 | 一致快照、store cursor、当前 partial |
| `session_list/transcript_page` | 活动筛选、稳定分页游标 | 元数据或原文/译文投影 |
| `transcript_revise` | segment、expected_revision、text、reason | 新 revision、失效依赖清单 |
| `artifact_review/publish/hide` | artifact、revision、动作 | 新审核/发布状态或冲突 |
| `job_create/cancel/retry` | kind、显式 session/version 集合 | job 状态 |
| `export_create/import_legacy` | 用户选定目标/来源、格式、版本 | job/导入清单；不接受任意远端路径 |
| `display_create/revoke` | session、view、发布策略、有效期 | 只读 URL/窗口配置 |
| `models_list/prepare` | manifest IDs | 安装状态；下载进度异步 |

浏览器只读接口拟定：

```text
POST /v1/display/bootstrap              # 用短期凭证换只读会话，不是控制入口
GET  /v1/display/sessions/{id}/snapshot # 发布投影及 publication cursor
GET  /v1/display/events?after={cursor}  # WS 升级，含 view/scope 授权
GET  /v1/display/search?q=...           # 仅当前授权已发布文本，限长限频
GET  /v1/display/artifacts/{id}         # 授权的已发布 revision
```

peer 单独暴露配对和发布同步端点，不与浏览器凭证共用权限。桌面控制 command 不对应任何默认公开 HTTP 路由。HTTP/WS 载荷验证只读 view、session 和期限；HTML/Markdown 按不可信文本渲染，禁止模型输出注入脚本或任意资源 URL。

### 8.1 快照与断线恢复

同机桌面用 `store_seq`，公开浏览器用专门的 `publication_seq`；不能给公开用户提供原始 store replay。快照在一致读事务中返回 `snapshot + cursor`，客户端随后订阅 cursor 之后的数据，服务端必须从日志补发这段间隙，不能只接入内存广播。

事件消息包含 cursor、message ID、entity ID、revision 和变更类型；客户端忽略已消费事件/旧 revision。session 过滤后的游标跳号是正常的，不能当丢包；服务器明确返回 `reset_required` 才重新取快照。进程重启/日志保留边界导致无法补发时也走 reset。切会后解除旧订阅，所有异步结果仍校验 session。

partial 单独作为临时快照/频道，不推进 durable cursor；新连接得到当前 partial，随后跟踪它的 partial_seq。发布策略要求人工批准的字幕只发送批准片段，内部临时字幕不出现在公开 snapshot 中。

## 9. 长会议、跨场报告与可恢复生成

快照包含精确输入实体集合和 hash；用实际模型 tokenizer 计算 `context_limit - system - schema - output_reserve - safety_margin`。按完整 segment/话题切块，允许少量重叠；每块输出直接来源引用。合并按来源 ID/revision/span 去重，保留不同观点。

`coverage_manifest` 至少记录总 segment 数、块与源范围、处理成功/失败区间、所选 session/发布 artifact 版本、忽略原因、重试和全局完整性。每块以 `input hash + effective_config_hash` 作为 checkpoint 身份；配置摘要覆盖精确 model manifest、提示词内容、profile/匿名策略、生成参数和 schema。完成后由 core 确认保存；worker 重启只复用输入和配置均匹配的已确认块，未确认或配置失效的块重新计算。

报告末尾的关键决策也必须进入覆盖测试；不允许重新引入“每场只取前 4000 字符”。没有可用材料的选中场次显示 missing，生成结果是 partial draft；不能静默跳过并标记全活动报告完成。发布报告前再次验证所有依赖当前有效。

## 10. 协议必须通过的测试

协议测试用无模型固定输入，不把生成速度当一致性测试前提：

- message 重发/正文冲突、producer 重启、revision 冲突、旧 partial 晚到。
- 多原文合并为一条译文、一原文拆多条；中英/emoji/组合字符/标点/空格的 UTF-8 spans。
- commit 后 ack 前崩溃、outbox 重放、DB/磁盘失败；ASR 出队后 final 前崩溃仍能从预期段清单发现遗漏。
- 尾部仍在推理时停止；短于 10 字/无标点且尚未 commit 的翻译尾句停止或崩溃后仍可恢复。
- A 场任务运行时切 B/C 场，结果仍属于 A；取消后旧 attempt 回来不覆盖。
- 空审核集合不回退 draft；修改已审核正文不继承批准；来源变更传播 stale/撤回。
- 结果验证后提交前发生取消/原文修订；审核后发布前发生来源撤回；事务内围栏必须拒绝过期状态。
- worker 非法行、超长行、stdout 混入日志、路径越界、hash 错误、协议不匹配、卡死。
- snapshot 与订阅间发生消息、断线重连、日志游标过期、公开 payload 不含私有字段。
- 多机发布修订/撤回重复与乱序，来源游标缺口恢复，离线不影响本地记录。

数据库与事件协议在 F02/F03 冻结首个可用版本；后续增加字段先定义兼容性和迁移，再修改实现。只在 Markdown 中改字段而未更新生成类型与测试，不算完成契约变更。
