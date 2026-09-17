# ADR 0003：Vision Forum 整合开发基线与验收配置

日期：2026-09-16。状态：接受为开发基线；正式产品验收未执行。

## 决策范围

按用户已确认的方向，先在当前 Forum 仓库 `codex/vision-forum-integration` 完成 Vision Forum 版，再从 F12 的稳定版本派生 Minutes。F00 的产物是可校验配置、需求映射和明确未决项，不是模型质量、实测性能、安装或现场验收报告。

本决策落实三份工程文档：[产品与架构](../engineering/VISION_FORUM_ENGINEERING_PLAN_ZH.md)、[数据与协议](../engineering/DATA_AND_PROTOCOL_ZH.md)、[任务与验收](../engineering/IMPLEMENTATION_TASKS_ZH.md)。原 [SPEC](../SPEC.md) 保持不变。ADR 0001/0002 的历史模型和性能记录属于旧应用；它们不作为新整合产品通过的证据。新路径采用 Rust core 唯一写入、Dora 音频运行时、无状态 stdio Python worker 和独立只读发布网关，不能重新接入旧 Python 全局 session manager 作为业务所有者。

## 配置契约

- [product.json](../../profiles/ai-vision-forum/product.json) 是构建身份：`org.aivisionforum.agent`、`AI Vision Forum`、`forum-shell`、独立 URL scheme/数据目录/Keychain 命名空间。开发版本为 `0.1.0-dev.1`；仅 macOS 14+、Apple Silicon。Forum 无商业登录、付款或许可证门槛；不导入 Hen 商业账号运行依赖。
- [profile.json](../../profiles/ai-vision-forum/profile.json) 是运行策略：活动品牌、两房各一机、语言、音源、录音、审核、模型角色与网络策略。模型名字是待 F01 对测并锁定 manifest 的候选，不代表已验证或已下载。运行 profile 不允许改变 bundle ID、更新源或凭据命名空间。
- [acceptance-profile.json](../engineering/acceptance-profile.json) 将 C1–C10 映射到阶段、方法、硬件、指标、失败条件和所需证据。全部产品验收状态均为 `not_evaluated`，且保留旧 SPEC 的优先级；P2 也属于本轮最终 Forum 完成范围。
- 正式更新默认关闭，endpoint/public key 为空，避免开发包安装 Translator 更新。签名、公证和正式 feed 由 F11 独立验证；配置文件不包含发布凭据。当前 `false` 表示本基线没有正式签名/公证验收证据，不声称未来构建永远不签名。
- 配置是后续实现的输入契约；F00 不声称现有旧应用已读取这些配置或已执行其中的审核策略。

## 验收口径与未决项

1. **C1 不降格。** 原 SPEC 为 `<3s`，未注明分位数、起止端点和公开审核范围。开发统计固定报告首字幕、句末稳定原文、句末最终私有译文及公开展示的 count/p50/p95/p99/max，包含缺失/失败样本。`p95 ≤3s` 只是开发参考。D01 未明确前，不能称原指标通过；正式失败不能靠改为 p95 来消除。
2. **公开审核默认严格。** 公开字幕只展示经人工批准的版本，不发布未审核 partial。公开摘要、纪要、报告及公开引用须通过审核和脱敏。内部操作员快字幕与人工审核等待、公开端到端延迟分开记录；公开延迟不能扣去审核时间。人工审核与自动遮名都不是“绝不出现任何身份线索”的已验证保证；SPEC 对所有显示/输出的绝对匿名要求与保留内部原文存在范围冲突，D02 留待组织方明确并现场验证，不擅自改写原要求。
3. **C3 正式样本开启全程录音和匿名多人标签。** 允许用户关闭录音进行降级使用，但该次不满足完整录音验收。mic/system 只是声源标签，不能替代多人分离。缺口、unknown、overlap 必须记录；稳定原文保存不受录音选项影响。
4. **C6 采用完整草稿候选。** 90 分钟会议从停止请求被接受到完整草稿校验、持久化并可见，候选上限 180 秒，计入收尾、排队、生成与验证。入队或 partial 不算完成；超过候选即记录候选未通过。F01 用硬件和连续会议实测校准，变更须记录原因，不能以降低门槛掩盖失败。
5. **正式运行是两房各一机。** 两台目标 Mac 独立 mixer 输入、独立 SQLite 和设备 ownership，各自连续运行 90 分钟；单机演示不能代替 C2。目标两机内存和模型准入值待发布矩阵，不以开发机 48 GiB 推导任意 M 系列或 16 GiB 保证。
6. **无互联网不等于无局域网。** 会前准备完成后不依赖云端推理、账号或在线下载；C2/C5/C9 使用受控 LAN。手机证书信任、TLS 配对、Python 分发与 Dora 隔离、正式签名仍是明确未决项。

未决项 D01–D07 在验收 JSON 中指定责任方、解决阶段、所需证据和被阻塞的正式能力。F00 可以在这些问题被明确列出时完成；F12 不可以带着相同的未决项宣称产品完成。真实录音、转写、审核材料仅保存在本地授权证据位置，仓库仅存可分发 fixture 和不含会议内容的指标。

## 来源与机器快照

| 项目 | 实施开始时的只读记录 |
|---|---|
| Forum 旧应用来源 | `8c3168c34c88f4b80300b08c567a5ed7a7a93dc7` |
| 当前含工程文档的 HEAD | `16d1a9ffe5c79a044271d601fee01edb75c07477` |
| Hen 固定导入来源 | `49042697f11bc86dfa8e6bd10ef0dc07cffbaf84`，`1.2.0-beta.4`；源工作区当时干净 |
| 开发机 | Apple M4 Pro、arm64、48 GiB；macOS 27.0 / 26A428 |
| 观察到的工具链 | Rust/Cargo 1.92.0、Node 20.20.0、npm 11.7.0、系统 Python 3.14.3 |
| 计划 worker | Python 3.12；系统 Python 版本不是 worker 已构建证据 |

Forum 已有未提交修改位于 `constants.py`、`llm.py`、`server.py`、`session.py`、`static/control.html`，另有两份未跟踪评估文档；精确路径列于验收 JSON。它们保留，不将工作区现状冒充上游固定 commit。F01 的受控源码导入另附来源清单；本 ADR 不替代逐文件导入映射，也不修改 Hen 源仓库。

## 验证

从仓库任意工作目录运行：

```bash
python3 scripts/validate_forum_profile.py
python3 scripts/validate_forum_profile.py --self-test
```

脚本只读三份 JSON、检查跨文件身份和 C1–C10/未决项引用，附负例验证防止降低正式时限、打开自动公开字幕、启用商业门槛或继承错误更新源。它不启动应用、麦克风、模型或网络，也不将配置校验当作产品验收。
