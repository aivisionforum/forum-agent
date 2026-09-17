# F01 独立 worker 的本机验证记录

日期：2026-09-16。机器：Apple M4 Pro、48 GiB 统一内存、macOS 27.0、arm64。
此记录证明本机隔离进程及临时 `.app` 资源布局可运行；**第二台 M5 Max 的
干净环境验证仍须在该机器实际执行，未在这里宣称通过**。

## 固定输入

- Astral CPython 3.12.11 / 20250612；install_only 归档 SHA-256：
  `c6d4843e8af496f034176908ae3384556680284653a4bff45eff07e43fe4ae34`。
- Python 许可证 full 归档 SHA-256：
  `77ea9c53e050615364b11134efeac8cd6d8cc53401c21b311b1bd3c5fe830f23`。
- MLX 与 mlx-metal 0.32.2，固定 macOS 14.0 arm64 wheel；mlx-lm 0.31.3、
  Transformers 5.17.0、NumPy 2.4.6，完整传递依赖来自锁文件。
- 当前锁文件 SHA-256：
  `3bd55a1477625ea4658d26114efc573d46be76a7f6aefee3fca58fd51f5fbada`。
- wheel 构建使用固定 setuptools/wheel；许可证解压使用固定 zstandard，
  这些构建工具不进入最终运行环境。

## 子进程与本地模型

以环境白名单从临时目录运行，无开发 PATH、PYTHONPATH、PYTHONHOME、
Conda、VIRTUAL_ENV、DYLD 或 HF 令牌。包内 Python 的 `prefix`、`base_prefix`、
全部 `sys.path` 都位于资源目录，`isolated=1`、`no_user_site=1`；安装包清单
精确匹配 34 个运行 wheel 加 worker。

真实 initialize → ping → 不支持的 jobs.run → shutdown 返回正确响应并退出，
stdout 逐行均为对应 JSON-RPC 对象。默认 worker 不导入 MLX；检查器另起
短子进程在 GPU 上执行 2×2 float32 矩阵乘法，`mx.eval` 与 synchronize 后
得到 `[[19,22],[43,50]]`，识别设备为 `Device(gpu, 0)` / Apple M4 Pro。
一次本机检查包含导入的矩阵耗时为 0.194 秒，不作吞吐基准。

显式启用诊断后，已有 `mlx-community/Qwen3-8B-4bit` snapshot
`545dc4251c05440727734bcd94334791f6ab0192` 完成真实生成；未下载权重。
首次包探针使用合成文本“今天的圆桌讨论决定先完成实时翻译，再验证会议摘要”，
输出“今天圆桌讨论决定先完成实时翻译，再验证会议摘要。”：

| 指标 | 首次包探针实测 |
| --- | --- |
| 库导入及模型加载 | 2.412 秒 |
| 调用至首 token | 3.522 秒 |
| 调用至生成结束 | 4.074 秒 |
| 输出 token | 16 |
| 解码吞吐 | 30.26 token/秒 |
| MLX 报告峰值内存 | 4.801 GB |

加入完整许可证、自包含检查器和 C stdout 缓冲保护后，最终候选再次完成相同
请求：总 3.405 秒、16 token、峰值 4.801 GB，worker wheel SHA-256 为
`18738d004e1127ec62854e73c69e9f3066ee72fb4bdb2ed01bc74256124b026a`。
两次为冒烟验证而非稳定性能基准，不从数值变化推断吞吐提升。

这些数据只证明本地模型可加载、执行和退出；没有进行摘要质量、事实准确性、
长会议、生产任务取消/恢复或多模型资源调度验收。

## 二进制、许可证和重建

标准库 Mach-O/FAT 解析器检查 33 个原生文件的 arm64 slice、最低系统版本和
动态加载路径；未发现 Conda/Homebrew/源码目录依赖。CPU/Metal wheel 最高最低
系统要求为 macOS 14.0。此检查不调用 otool 或 Xcode CLT。

19 份 Python/静态依赖上游许可证及 PYTHON.json 已与 wheel 自带许可文件
一起保存；资源根包含 THIRD_PARTY.md、完整 manifest 和可用包内 Python
执行的自检脚本。

无模型自动化测试共 24 个通过：协议 14 个、模型探针边界 4 个、打包保护 3 个、
Mach-O 解析/拒绝非便携输入 3 个。构建同时运行 `pip check`，依赖关系通过。
两个全新目录各执行一次 `--offline` 构建，7051 个文件/符号链接的字节、权限
和链接目标全部一致。比较不包含文件系统时间戳、签名或 DMG。已删除命令行
wrapper 对应的 RECORD 行，避免遗留 staging shebang 哈希影响重建结果。

完整重建和最终候选证据由以下本机 JSON 文件记录（临时目录不会随发行包交付）：

- `/tmp/forum-worker-f01-mlx-probe.json`：首次真实 Qwen3 推理与隔离路径。
- `/tmp/forum-worker-f01-final-check.json`：包内解释器启动随包检查器，默认 Metal 和协议通过。
- `/tmp/forum-worker-f01-ready-check.json`：最终候选再次用包内 Python 完成 Metal、协议及真实 Qwen3 推理。
- `/tmp/forum-worker-f01-rebuild-comparison.json`：两个独立构建目录的文件字节、权限、符号链接比较。

`evidence/` 保存最终报告副本；模型路径中的本机用户名替换为 `~` 并明确记录
这一处脱敏，原始报告仍在上述本机路径。可集成资源目录为
`/tmp/forum-worker-f01-ready/AI Vision Forum.app/Contents/Resources/meeting-worker`。

当前不宣称代码签名、公证、DMG、第二台 Mac、LAN、多设备浏览器或正式会议
任务已通过本文件的验证；这些由整体 F00–F01 验收记录分别说明。
