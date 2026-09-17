# forum-contracts：F02/F04 协议 v1

不依赖 Tauri、Dora 或 MLX 的 Rust 数据契约。持久事件包括 capture、audio gap、ASR final/人工修订、会议状态/停采/producer 对账、原文封存、语向边界、翻译 requested/final/failed。类型及 core 返回值共同生成 `packages/contracts/forum.generated.ts` 和 `forum.schema.json`，生成命令见 forum-core README。

`Event<T>` 核对 schema/type、活动/会场/场次 UUID、producer run/seq；反序列化拒绝未知字段和未知类型。所有持久事件都属于完整单场 scope，不能把未来活动级报告强塞到任意 session。Revision 必须大于零，UUID 不为 nil，producer seq 从 1 开始；音频/序号字段限制在 JavaScript 可精确表示的整数范围。

`SourceSpan` 是 UTF-8 字节偏移与精确 quote。它支持中英文、emoji、组合字符和真实空格，拒绝零长度范围、越界、截断码点和文字不匹配；它只能证明精确文本，不能证明结论语义或公开发布许可。跨 session/current revision/attempt/epoch 在数据库事务中核对。

```rust
use forum_contracts::{Revision, SourceSpan, Uuid};
let span=SourceSpan{segment_id:Uuid::new_v4(),segment_revision:Revision::FIRST,
    start_utf8:3,end_utf8:7,quote:"📝".into()};
assert_eq!(span.validate_against("中📝文")?,"📝");
# Ok::<(),forum_contracts::ValidationError>(())
```

`TranslationRequested` 保存来源跨度、输入、可重建拼接版本、target language、revision/attempt/epoch 及 backend/model。`DirectionChanged.boundary=Some(...)` 是采样边界后的现场切换；None 仅用于显式全场重译。`ProducerReconciled` 只描述宿主证据，不允许普通 worker 经 ingest 写入或冒充旧 producer seal。

目前覆盖 F02/F04 的持久数据，不包含 F05 job 调度、模型报告、审核版本或发布网关。Python 同版本 schema 校验模块独立存在，不 import 旧 server，也没有开启尚未实现的 worker 分析能力。
