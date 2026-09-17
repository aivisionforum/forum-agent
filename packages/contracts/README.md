# Forum 同源契约

`forum.generated.ts` 与 `forum.schema.json` 由 Rust `forum-contracts` 和 `forum-core` 的实际 Serde/JsonSchema 类型生成。包括所有事件 payload、SessionSummary、SnapshotPage、TranslationRecord、TranslationPage、SessionExport、AnalysisJob/Snapshot/Result/Checkpoint、ArtifactRecord、PublicArtifact/PublicationChange 等前后端返回类型；不能手工维护另一套字段。schema_version=1，DB user_version 是独立编号。

```bash
CARGO_TARGET_DIR=/tmp/aivf-core-f02-target cargo +stable run --offline --locked --manifest-path desktop/Cargo.toml -p forum-core --example generate_contracts
# CI 中检查产物无漂移：
CARGO_TARGET_DIR=/tmp/aivf-core-f02-target cargo +stable run --offline --locked --manifest-path desktop/Cargo.toml -p forum-core --example generate_contracts -- --check
```

生成器检查同名定义一致性、required 字段确实存在，并保留名为 title 的普通字段。事件 type 与对应 payload 固定；schema_version 固定、Revision 最小 1、UUID 不为 nil，整数限制在 JavaScript 精确范围。TypeScript 类型只帮助编译，不能替代运行时边界验证。SQLite 的 scope/current revision/attempt/epoch/事务核对始终具有最终效力。

`utf8.ts` 明确将浏览器 UTF-16 选区换成 UTF-8 字节，拒绝截断代理对或码点。不要直接把 JavaScript string.length 当源文 byte offset，也不要无提示 trim 引用。Python 同版本验证器在 `services/meeting-worker/src/forum_meeting_worker/schema_validation.py`：传入本 schema 文件后可进行结构及精确跨度验证；不 import 旧 server；结构验证本身不初始化模型。

```bash
PYTHONPATH=services/meeting-worker/src .venv/bin/python -m unittest discover -s services/meeting-worker/tests -p test_schema_validation.py -v
node desktop/forum-shell/ui/node_modules/typescript/bin/tsc packages/contracts/utf8.ts packages/contracts/forum.generated.ts --outDir /tmp/forum-contracts-js-f02 --module commonjs --target es2020 --strict --skipLibCheck
FORUM_CONTRACTS_JS=/tmp/forum-contracts-js-f02/utf8.js node --test packages/contracts/test_utf8.mjs
```

当前 Python 5 项、TypeScript/Node 3 项测试通过。Python 验证器只解释本生成器使用的 schema 子集，不是任意第三方 JSON Schema 执行器；它不承诺代替数据库验证真实来源或公开发布许可。


F05–F08 的 job state、validation、review、publication 是四套独立状态。UI不能把Succeeded当Approved或Published。`AnalysisEvidence` 使用精确原文/产物版本和UTF-8范围，公开端只接受`PublicArtifact`的人工审阅正文与opaque引用ID。`AnalysisConfig.model_manifest_id`是`sha256:<64hex>`，其余profile/prompt/policy/effective摘要为64位小写hex。Rust `canonical_json`显式排序嵌套对象，schema/TS生成器也使用固定顺序，不受桌面serde_json preserve_order特性影响。
