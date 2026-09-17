# Forum Meeting Worker — F01 协议入口

这是可安装的 Python 3.12 标准库 worker，仅实现进程握手和健康检查。
**尚未实现洞察、纪要、报告、模型加载或任务队列。** `jobs.run` 和
`jobs.cancel` 返回 `CAPABILITY_UNAVAILABLE`，不会制造分析成功结果。

## 安装和运行

在本目录使用独立 Python 3.12 环境安装：

```bash
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python .
.venv/bin/forum-meeting-worker
```

运行没有第三方依赖；构建使用 setuptools。不要使用旧 Forum 应用的
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

## 打包边界

F01 可先构建 wheel，并用独立 Python 3.12 解释器运行上述入口。
这只验证包/进程协议，不代表 Tauri sidecar、独立解释器、MLX 原生库或
签名公证已完成。完整分析任务、可取消的计算控制循环、模型 endpoint
及资源许可在 F05/F06 实现，届时才更新 capabilities。

已有 Python 3.12 环境包含 setuptools 时，可离线验证 wheel 构建：

```bash
uv build --wheel --offline --no-build-isolation --python /absolute/python3.12
```
