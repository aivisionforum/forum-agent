# forum-contracts — F02-a

不依赖 Tauri、Dora 或 MLX 的 Rust 数据契约。当前实现协议 v1 的三个内部事件：

- `Event<CaptureSegmentClosed>`：`audio.segment_closed`。
- `Event<TranscriptFinal>`：`transcript.final`。
- `Event<TranscriptRevision>`：`transcript.revised`。

事件 envelope 校验 schema/type、活动/会场/场次 UUID、生产者 run/seq。
`SessionSpec`、`TrackSpec`、`AudioRange` 定义基础实体与采样范围；`Revision`
必须大于零，JSON 中仍是整数。UUID 解析和非 nil 校验分别处理，所有实体的
`validate()` 都应在进入数据库前执行；`forum-core` 的写入口已执行对应验证。
反序列化拒绝未知字段和未知事件类型。

`SourceSpan::validate_against()` 使用 UTF-8 字节偏移与精确引文匹配，支持
中英、emoji 和组合字符；拒绝空引用、越界与非字符边界。它只证明文本匹配，
不能证明结论语义或审核状态。跨 session 和来源版本查询由 core 校验。

```rust
use forum_contracts::{Revision, SourceSpan, Uuid};

let span = SourceSpan {
    segment_id: Uuid::new_v4(),
    segment_revision: Revision::FIRST,
    start_utf8: 3,
    end_utf8: 7,
    quote: "📝".into(),
};
assert_eq!(span.validate_against("中📝文")?, "📝");
# Ok::<(), forum_contracts::ValidationError>(())
```

目前是 F02-a 子集，不是完整生成协议。尚未实现 TS/JSON Schema 生成、活动级
多场 job envelope、partial/translation/worker/publication 全部类型与兼容性升级。
这里的 `Event<T>` 只接受有完整 event/room/session 的单场持久事件；不能把
活动报告强塞到一个任意 session。

在 `desktop/` 验证：

```bash
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --locked -p forum-contracts -p forum-core
```
