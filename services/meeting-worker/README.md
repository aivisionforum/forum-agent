# Forum Meeting Worker — F01 协议入口

这是可安装的 Python 3.12 worker，正式接口仅实现进程握手和健康检查。
另有需要显式启用的 F01 本地 MLX 打包探针。
**尚未实现洞察、纪要、报告或任务队列。** `jobs.run` 和
`jobs.cancel` 返回 `CAPABILITY_UNAVAILABLE`，不会制造分析成功结果。

## 安装和运行

在本目录使用独立 Python 3.12 环境安装：

```bash
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python .
.venv/bin/forum-meeting-worker
```

默认协议运行没有第三方依赖；构建使用 setuptools。不要使用旧 Forum 应用的
`server.py` 启动这个 worker。模块入口为 `python -m forum_meeting_worker`；
安装入口为 `forum-meeting-worker`。宿主应持有 stdin/stdout 管道，不通过
shell 拼接会议内容或命令。

源码测试无需安装包、下载模型或加载旧应用。在本目录执行：

```bash
python3.12 -m unittest discover -s tests -v
```

测试使用当前 Python 解释器启动真实 worker 子进程，将 `src` 放入子进程
模块路径；不依赖运行目录。已经安装的 wheel 可从任意目录启动模块入口。

## 已实现协议

协议版本 `1`，一行一个 JSON-RPC 2.0 对象，UTF-8 编码，以 LF 结束；允许
CRLF。stdout 只输出 JSON-RPC，诊断写 stderr，错误不会回显输入正文。
不支持 JSON-RPC batch；通知不响应。请求 ID 支持字符串、数字和 null，
不接受布尔值。

首先发送 `initialize`，参数必须包含：

```json
{"jsonrpc":"2.0","id":"init-1","method":"initialize","params":{"protocol_version":1,"instance_id":"90000000-0000-4000-8000-000000000001","job_root":"/ABSOLUTE_EXISTING_JOB_DIRECTORY","profile_id":"ai-vision-forum","profile_version":"v1"}}
```

`job_root` 必须由宿主提前创建，worker 不创建目录或修改数据库。
握手返回实际 build version，以及：

```json
{"health":true,"task_types":[],"model_clients":[]}
```

初始化后可发 `health.ping` 和 `shutdown`，均无参数或使用空对象参数。
shutdown 写出响应后退出；EOF 也正常退出。未初始化的已知方法返回
`NOT_INITIALIZED`；重复初始化返回 `ALREADY_INITIALIZED`，保留原身份。
无效初始化不会污染状态，宿主可以重新发送正确初始化。

每帧上限为 LF 前 1 MiB。收到过长帧、EOF 前不完整帧或不完整帧超时，
返回有业务错误码的协议错误并退出，防止把残留字节解释为新命令。
`--frame-timeout-seconds` 默认 `10`，从首字节开始计时；空闲连接不超时。
JSON 解析失败和普通方法错误不关闭连接。协议错误使用标准整数 code，
详细业务码放在 `error.data.code`。

退出码：`0` 正常 shutdown/EOF，`2` 参数或帧错误，`1` stdio 断开，
`130` 中断。当前读管道实现针对 macOS/POSIX；未声明 Windows 兼容。

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
完整分析任务和资源调度等待后续阶段授权；当前停在 F00–F01。

## 验证范围

24 个无模型测试覆盖原协议生命周期/帧边界，以及探针默认关闭、输入范围、
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
