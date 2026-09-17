# forum-core：会议、翻译与分析数据核心

本库提供 SQLite 单写 Store、有界 CoreHandle actor、会议生命周期、逐字稿修订、翻译 coverage、采音封存及恢复对账。这里没有模型、麦克风、Dora、网络或 UI；真实设备与节点接线由相邻组件实现。不能仅凭本库单测宣布 F02–F04 整体现场验收完成。

## 宿主调用

```rust,no_run
use forum_core::{CoreHandle, SessionSummary};
let core = CoreHandle::open("/absolute/owned/forum.sqlite", 128)?;
let sessions: Vec<SessionSummary> = core.call(|store| store.list_sessions(100))?;
// UDS ingest handler: core.ingest_json(event_value) → durable Receipt.
// 对请求超时保留完全相同 message_id/run/seq/body，重发求证提交结果。
core.shutdown()?;
# Ok::<(), forum_core::StoreError>(())
```

队列容量为 1–4096，`try_call` / `try_ingest_json` 立即尝试入队，满时 `QUEUE_FULL`；`CoreTicket::wait_timeout` 允许指定等待上限。`call` 默认 4 秒，SQLite busy timeout 为 3 秒。`ACK_UNKNOWN` 表示提交结果未知，不是未提交。宿主闭包只应做有界数据操作，不得放模型调用、网络或音频等待；超时不会中断已接受的闭包。`shutdown` 有限等待，只有连接、锁释放且线程退出才返回成功；超时保留 handle，之后继续求证关闭。

`Store::open` 使用 canonical 数据库路径旁的 `.core-owner` 文件锁。另一个进程与 symlink 别名不能获得写者身份；数据库硬链接明确拒绝，避免不同旁文件锁。macOS 上不能直接 flock 数据库 inode，否则会与 SQLite 锁冲突。旁文件不删除，实际锁由打开的文件句柄持有。内存 Store 仅用于测试。

## 数据 API

| API | 作用 |
|---|---|
| `create_session`, `create_track` | 目录登记，同 ID 同内容幂等、冲突拒绝 |
| `transition_session` | expected-state 事务：Created → Preparing → Ready → Recording → Stopping；异常可 Interrupted |
| `register_capture`, `register_gap` | 已准备音轨的预期音频范围与缺口；capture 登记时固定语向归属 |
| `ingest_final` | 核对 track/session/音频范围；成功、空、失败都保存；只允许旧 Failed ASR 用下一 revision 重算 |
| `revise_transcript` | 人工 expected_revision 乐观锁、版本保留、关联译文失效、完整来源范围重新入队 |
| `pending_translation_work` | 当前 source revision 的 pending 字节范围；包含旧语向未完成尾部 |
| `request_translation`, `finish_translation`, `fail_translation` | 原子请求/结果/失败，精确来源、revision/attempt/epoch fence、coverage 与 outbox |
| `unresolved_translation_page` | requested/failed 在 SQL LIMIT 前过滤，以首次创建序号/UUID 翻页，重试不移动排序键 |
| `translation_records_for_segments_at` | 与原文快照 cursor 对齐的历史译文；旧/失效结果保留 state 与出处 |
| `session_status`, `list_sessions` | 状态、封存里程碑、完整性、恢复目录与待翻译计数 |
| `snapshot_page`, `snapshot_tail`, `translation_page` | 同 cursor 稳定分页；keyset 排序，不被后续插入/修订改变 |
| `export_session` | 完整同 cursor sources、legacy snapshot、状态、译文、gaps；sources 含空/失败/缺结果所有捕获段 |
| `outbox_after` | 已提交内部事件，最多 1000；不得直接当成公开投影 |
| `capture_seal`, `registered_segment_ids` | 读取精确捕获清单/哈希及已登记段 |
| `terminal_segment_ids` | 所有已有 ASR 终态，不受分页上限影响 |
| `terminal_segment_ids_for_replay` | 仅成功/空，失败必须重新尝试，不受分页上限影响 |
| `recovery_segments` | missing/failed、录音引用、next_revision；按 1–1000 查询，仅用于恢复列表展示 |
| `recovery_revisions` | 每批最多 256 个指定 ID 的精确 next_revision/terminal，严格一进一出；生产恢复不能从截断列表猜 v1 |
| `capture_stopped`, `producer_sealed`, `seal_transcript` | 停采、producer 清单、全部原文终态分别对账；翻译可在 Completed 后继续补尾 |
| `unsealed_producer_runs`, `reconcile_abandoned_producer` | 宿主确认旧进程已回收且 outbox 重放完成后，用独立恢复证据解除旧 run 封存阻塞 |

`session_snapshot.transcript` 是兼容 API，只含成功文本；empty/failed/missing 在 `incomplete`。新界面和审计使用 `snapshot_page.items` 或 `export_session.sources`：每项 `transcript` 有值时包含所有三种终态，没有 ASR 结果时为 null。

## 语向、修订与翻译

`DirectionChanged.boundary=Some({track_id,start_sample,configured_source_language})` 用于真实采样边界切换：宿主/采音节点先结束旧段并取得 capture ACK，再提交 boundary 并 ACK，最后换采样配置。旧段晚到的 ASR、旧 pending 译文仍按旧 epoch 完成；跨边界的音频段、与 capture 语向不符的 final 被拒绝。多个音轨各有自己的边界，session epoch 是边界变更的全局递增编号，不表示所有旧来源都变成该方向。

`boundary=None` 是显式全场重译：更新当前来源 coverage、使旧结果 stale，保持原始 ASR provenance。空字段不参与序列化，兼容之前 canonical 事件 hash。生产现场切换必须传 Some，不能误用 None。

来源跨度始终是 UTF-8 字节范围与逐字 quote。支持 identity-v1、join-space-v1、join-newline-v1 可重建拼接；不隐式 trim，不丢句尾空格。部分来源会切分 coverage；多段合译中任一原文改写，整条译文 stale，同时未改动来源的范围会重新 pending，避免永久卡住。重试必须同 ID/revision、attempt+1 且请求内容相同；新 source revision/方向需要新 translation revision 或新 ID。旧完成事件完全相同重发仍 ACK，新的过期结果不提交。

## 封存与恢复

捕获停止不等于译文完成。`capture_stopped` 核对每个 track 的段集合、结束 sample、清单 SHA-256；`producer_sealed` 核对同 run 的终态段集合和 final_seq。`seal_transcript` 要求每个捕获段都有 success/empty/failed 终态以及所有 ASR run 已封存/被显式恢复对账。Failed 与音频 gap 保持 incomplete，不能伪装成功。

打开数据库会把未结束的 Preparing/Ready/Recording/Stopping/Draining 会议持久标记 Interrupted，不打开设备。Interrupted 可接旧数据尾部；恢复失败原文用 next_revision。`reconcile_abandoned_producer` 仅供可信宿主直接调用，要求捕获已停、准确清单 hash、已拥有旧 Child 的退出证据及 outbox 完整重放证据。节点普通 `ingest_json` 无权写恢复事件。该操作不是伪造旧进程的正常 seal，也不会补造丢失原文。旧 run 封存后只允许完全相同事件的 duplicate ACK。

## 迁移与检查

当前 DB version 为 5。版本 1→2 增加状态/coverage，3 增加 producer 恢复和译文历史，4 固定 capture 语向及采样边界，5 增加分析任务/快照/产物/审核/公开投影/旧数据导入。已有数据库升级前 `VACUUM INTO` 唯一备份文件并 fsync；未来版本拒绝、不降级。升级之前不存在的译文历史不能编造，游标早于历史保留起点返回 `RESET_REQUIRED`。尚未做历史压缩。

```bash
# repo 根目录执行，不下载模型。
CARGO_TARGET_DIR=/tmp/aivf-core-f02-target cargo +stable test --offline --locked --manifest-path desktop/Cargo.toml -p forum-contracts -p forum-core
CARGO_TARGET_DIR=/tmp/aivf-core-f02-target cargo +stable run --offline --locked --manifest-path desktop/Cargo.toml -p forum-core --example generate_contracts -- --check
CARGO_TARGET_DIR=/tmp/aivf-core-f02-target cargo +stable run --offline --locked --manifest-path desktop/Cargo.toml -p forum-core --example core_smoke
```

62 core + 4 contracts 单测通过：真实 SQLite 事务/触发器故障回滚、重开/丢 ACK、独立子进程锁、有界队列与超时、UTF-8、旧 ASR 恢复、异常 producer 对账、采音清单、源/译文快照分页、跨版本/attempt/epoch late fence、多来源释放、1002 段恢复、真实语向边界与重开。

未在这里实现：模型质量或硬件验收、producer 音频落盘策略、consumer ACK/压缩、实际模型进程调度和 HTTP 网关。内部 source/outbox 有完整私有文本，公开页面只能消费独立授权投影。


## F05–F08 分析与审核事务

所有入口均为 `Store` 方法，可通过 `CoreHandle::call` 使用。模型生成、文件读写和进程退出等待在 actor 外进行。

| API | 边界 |
|---|---|
| `create_analysis_job(&CreateAnalysisJob)` | 显式会话选择、同活动范围、配置SHA与有限预算；request_id/body幂等；自动任务相同快照去重，洞察新快照合并旧pending |
| `analysis_job`, `analysis_snapshot`, `next_analysis_jobs` | 单任务、精确compact JSON+SHA、全局待调度队列 |
| `claim_analysis_job(id,attempt)` | 同时仅一个 Running/CancelRequested；排队时间计入deadline，过期返回持久Failed |
| `wait_analysis_job`, `update_analysis_progress` | 有版本检查的等待原因和进度 |
| `cancel_analysis_job(id,expected_attempt)`, `complete_analysis_cancel` | 迟到UI不能取消新attempt；运行任务先CancelRequested，宿主确认进程退出才释放占用 |
| `interrupt_analysis_job`, `interrupt_running_analysis_jobs` | 宿主确认退出/启动恢复；保留原预算，不偷偷续时 |
| `retry_analysis_job(id,expected_attempt)` | 用户明确重试才新可见预算；最多3次、原输入与配置仍须current |
| `confirm_analysis_checkpoint`, `analysis_checkpoints` | 只有当前Running attempt能确认步骤，核对snapshot/config/result SHA；重启保留 |
| `reusable_analysis_checkpoints` | 同event/session集合/kind/config已确认块，最多1000条/16MiB；worker按当前块input SHA精确匹配并重校验证据，不直接复制旧artifact |
| `finish_analysis_job` | 同事务重检attempt/取消/deadline/snapshot/原文版本/公开依赖/音频gap、UTF-8引用和全部输入覆盖；已提交相同result重发幂等 |
| `list_analysis_jobs`, `list_artifacts` | 最近优先的created_at_ms/UUID keyset，after携带不可变analysis cursor；1–100条 |
| `artifact`, `artifact_revision`, `resolve_analysis_evidence` | 当前/历史版本与精确引用定位；引用匹配不等于模型陈述已被事实证明 |
| `edit_artifact`, `review_artifact` | expected_revision、操作者与理由；编辑新revision、回到Draft，自动结果不会覆盖人工批准的另一产物 |
| `publish_artifact`, `set_artifact_visibility` | Valid+Approved+complete+current+policy才可发布；公开标题、正文、证据均显式人工审阅，来源变更递归撤回 |
| `public_snapshot`, `public_changes` | 单独publication cursor；仅不含内部ID的PublicArtifact。旧cursor重放已经隐藏/撤回正文时也只返回tombstone |
| `export_artifact(id,revision,format)` | 内部JSON/Markdown/转义HTML导出；含状态与证据，不可直接作为LAN公开接口 |
| `list_sessions_page` | rowid DESC真实会议目录分页，新会议不会挤掉后页旧会议 |
| `import_legacy_transcript(spec,content)` | 16MiB/10000行、整份严格解析后单事务；按event+原文件SHA幂等，不读取外部路径 |

洞察选择尾部900秒相交的完整段；纪要选择整场source快照。未封存纪要、缺ASR、录音gap、未覆盖尾字节都保留partial。跨场报告只用所选每一场已有的已批准且公开产物；任何一场没有此类输入会拒绝，没有退回私有草稿或原文的路径。新生成的产物永远从Draft/Private开始。引用必须精确匹配版本和UTF-8字节；无支持的claim是NeedsReview，不能直接批准发布。此检查不替代人工判断语义是否得到引用支持。

公开表述和公开证据在同artifact revision内冻结。改变匿名表述先edit形成新revision，再审核发布，保证 `(artifact_id,revision)` 的引用正文唯一。隐藏/撤回/原文修订以及晚到gap的失效和publication tombstone都与数据变更在一个SQLite事务提交。内部审核命令存在publication_reviews，公开DTO不带内部引用或路径。

`canonical_json` 明确递归按key排序，不依赖serde_json preserve_order；模型配置、snapshot精确bytes、结果/步骤SHA采用该序列化。当前数值参数使用宿主固定展开值；跨语言不要擅自改写数字类型或重排精确snapshot文件。

旧JSONL要求每行有秒单位t_start/t_end、speaker_id、lang、text。TrackKind/RevisionOrigin明确为LegacyImport，1000Hz只是原时间戳的毫秒坐标，recording_ref为空，没有伪造capture或producer seal。原行、文件hash和speaker标签保留。由于文件不能证明录音/会议完整，导入会话incomplete、分析input_complete=false；可生成部分草稿，不能冒充完整纪要直接公开。

新增21项测试覆盖取消/attempt、同快照去重、排队预算、重启与确认cache、不同scope/config的cache隔离、原文人工和ASR修订、音频gap迟到、事务故障、精确UTF-8、尾段coverage、私有/隐藏泄漏、审核不被覆盖、公开表述版本、分页与原子导入。额外以 `--features serde_json/preserve_order` 跑完整核心测试，验证桌面依赖合并后的哈希行为。


自动会后纪要另有无模型依赖的持久停止记录：可信runtime仅在实际Stopped回调中调用`record_analysis_stop_intent(session_id)`。`pending_analysis_stop_intents(limit)`按marker升序返回`Vec<(Uuid,u64)>`，限制1–1000条；manager先创建或去重持久job，再调用`ack_analysis_stop_intent(session_id,marker)`。同场恢复后再次Stop采用事务DELETE+INSERT取得严格递增marker；旧ACK或重复ACK不会删除新Stop。退出或缺少模型配置时不需要构造job，记录仍可重启消费。三项新增测试验证无模型/无job重启保留、旧ACK与跨session隔离、真实SQLite插入失败回滚后仍保留旧记录。
