# forum-core — F02-a 持久化地基

本库是一个由 `&mut Store` 独占写入的同步 SQLite 连接，供后续 Rust actor
持有。当前没有线程、模型、音频设备、Dora 接线、网络或 UI；不把这个子集
称为完整 F02。

## 实际 API

| API | 行为 |
|---|---|
| `Store::open(path)` / `in_memory()` | 初始化 v1 schema；正式文件启用 WAL/FULL/FK；拒绝较新 schema |
| `create_session(&SessionSpec)` | 原子创建活动/room/session 目录实体；同 ID 相同内容幂等，不同内容拒绝 |
| `create_track(&TrackSpec)` | 校验 session 与采样率，登记唯一 track |
| `register_capture(&Event<CaptureSegmentClosed>)` | 先登记预期音频段、范围、可选录音引用与内部 outbox |
| `ingest_final(&Event<TranscriptFinal>)` | 精确核对 capture/track/session/audio，保存原文 v1、coverage 和 outbox |
| `revise_transcript(&Event<TranscriptRevision>)` | expected_revision 乐观锁；新建原文版本，旧 coverage stale，新 coverage pending |
| `transcript_revision(session, segment, revision)` | 读取指定历史版本，不改动当前版本 |
| `resolve_span(session, &SourceSpan)` | 读取精确历史来源并核对字节跨度、引用；不自动审核或证明当前有效 |
| `session_snapshot(session)` | 同一读事务返回 cursor、按音频时间排序的当前成功原文和未完成/失败段 |
| `coverage(session)` | 返回每个源版本/目标语言/epoch 的完整预期翻译范围 |
| `outbox_after(cursor, limit)` | 读取已提交内部事件；上限 1,000，供未来 ack/replay 消费层使用 |

使用完整可运行例子：

```bash
# 在 desktop/ 执行，默认创建唯一的 /tmp 数据库，只写合成中文。
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable run --locked -p forum-core --example core_smoke
```

例子依次创建 session/track、登记 capture、写 final、修订原文、关闭并重新打开
数据库，然后输出 snapshot。这里的“关闭”是释放数据库连接，不是已实现
会议 stop/seal 状态机。可提供一个数据库路径参数；存在的数据库会新增独立
UUID session，不覆盖其原会话。终端会输出所使用的路径。

## 持久化保证

三个事件写入口使用 SQLite `BEGIN IMMEDIATE`。message ID、producer run/seq、
SHA-256 内容一致时，重发返回原 receipt，不重复写入。相同 ID 或生产者序号
对应不同内容则失败。hash 基于拒绝未知字段后的 typed JSON 序列化，忽略线上
JSON 空白与对象字段顺序；不是原始传输字节 hash。

事务内同时写事件、原文版本/current projection、翻译 coverage 和内部 outbox；
commit 后才返回 durable receipt。数据库触发器故障测试验证：即使错误发生在
最后的 outbox 写入，前面的 receipt/原文/coverage 也一起回滚。同一事件可以在
恢复后重新提交，commit 后丢失 ack 的重发也不会产生第二句。

capture 必须先存在。失败/空 ASR 结果保存为有原因的缺口，不进入成功逐字稿，
也不生成假翻译任务。成功 final 即使没有翻译 node，也会保存完整 UTF-8 范围的
目标语言 pending intent。原文修改保留旧行；人工来源显式记录 operator/reason，
payload 的 backend/model 字段保留最初 ASR 的来源信息，不能将编辑结果宣称为
模型新生成结果。

内存数据库不支持 WAL，其余事务规则相同。文件 schema 首次从 0 创建到 1；
现在没有历史升级路径。较新 user_version 在任何 schema 操作前被拒绝。

## 当前限制和后续工作

- `Store` 不是完整异步 actor，也未实现跨进程文件所有权锁；宿主必须只启动
  一个 writer owner，未来 actor/进程 supervisor 负责唯一所有权。
- session/track API 目前用于初始目录登记；它们还不是完整生命周期事件。
  preparing/recording/stop/seal/interrupted 状态机、设备释放与 producer 对账待接入。
- schema 只有当前用到的实体。后续增加表/列必须版本化 migration，并先实现
  升级备份；当前不能凭此声明生产升级/降级兼容。
- snapshot 是一致查询，并未持久化 worker 输入文件/hash/分页快照；outbox 只有
  durable 查询，没有消费者 ack、压缩保留边界或 WS 推送。大会议分页尚待实现。
- coverage 只支持 pending/stale。尚无真正翻译 requested/final/failed、attempt、
  AttributedBuffer、多段合并、部分范围完成或自动重试。
- 只失效当前存在的 coverage。artifact/审核/公开证据和发布投影尚不存在，
  `resolve_span()` 不能用作公开发布许可。
- `outbox` 含完整内部文本，严禁直接用于公开网关。未来公共数据必须经过
  独立审核、脱敏和授权投影。
- 未接模型、Dora、真实录音，也没有宣称崩溃前未送达 core 的音频可以恢复。

## 检查

```bash
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --locked -p forum-contracts -p forum-core
cargo +stable fmt --check -p forum-contracts -p forum-core
```

当前 tests：3 个契约测试、14 个真实 SQLite 测试。覆盖 UTF-8/revision、final
原文先存、幂等及冲突、跨场引用、先登记 capture、修订与旧版保留、empty/failed、
事务回滚、重开恢复/丢 ack、未来 schema 拒绝、目录实体原子性、重叠音轨与游标。
