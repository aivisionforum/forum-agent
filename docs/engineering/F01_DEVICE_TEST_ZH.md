# F01 第二台 Mac 与手机验证

状态：待实测。已确认可使用 M5 Max、iPhone 和 Android。本表完成并由用户确认之前，第一部分仍进行中；不进入第二部分。

## 测试包与范围

测试包文件夹包含 `AI Vision Forum.app`、`Verify on this Mac.command`、`Start LAN Test.command` 和本说明。

这是 macOS 14+、Apple 芯片的开发验收包，使用本地 ad-hoc 签名，尚未正式签名/公证。包内包含独立 Python 3.12 与 MLX；不要求另一台机器安装 Python、Homebrew、Rust、Node、Conda 或 Xcode。模型权重另行准备。

本次验收覆盖安装、窗口、包内运行时与局域网 HTTPS 配对可行性。手机页面只显示合成双语样例；实时会议网关、可靠尾句和纪要 UI 属于后续部分，不能用本页替代其验收。

## 1. 复制到 M5 Max，检查包内运行时

1. 将整个测试包压缩文件通过 AirDrop/本地文件传输复制到 M5 Max，并解压到可写目录。保留三个文件在同一文件夹。不要只复制 `.command`。
2. 双击 `Verify on this Mac.command`。它使用包内 Python 检查独立运行环境、MLX、进程握手、签名及 macOS 信息；不会打开麦克风。
3. 若 macOS 阻止未公证的开发包启动，请记录提示。仅对本次明确交付的包，在“系统设置 → 隐私与安全性”使用系统提供的允许打开入口；不要关闭 Gatekeeper 或安装开发工具来绕过验收。
4. 完成后把桌面 `Forum-F01-check-日期时间` 文件夹中的结果发回当前任务。任何报错保留原文，不能把“能在开发机跑”算作通过。
5. 双击 `.app`：确认主窗口完整、有 Forum 名称、模型状态明确；点击“测试字幕”检查字幕浮窗；关闭浮窗和应用，再打开一次。此步骤不用启动真实采音。
6. 在有原 Hen Translator 的机器，保持它正常运行，再打开/关闭 Forum，确认 Translator 没有断开或被停止。真实采音的操作由你在设备上控制；请记录两边的现象。

如果另一台机器已有开发工具，请如实注明；独立进程环境会排除开发路径，但不能把它写成“没有开发工具的干净机器”。

## 2. 验证独立 GPU 推理

默认验证以小型 Metal 计算检查包内 MLX 能加载并执行。真实 8B 模型可使用第一台机器已有的完整 Qwen3-8B-4bit 快照，复制时必须包含真实权重文件（不能留下指向第一台 Hugging Face blobs 的失效符号链接）。不要把只有 config/tokenizer 的目录当成模型。

需要真实 8B 诊断时，在第二台终端中执行，替换两个绝对路径：

```bash
FORUM_TEST_APP="/绝对路径/AI Vision Forum.app"
FORUM_TEST_MODEL="/绝对路径/完整Qwen3-8B-4bit目录"
"$FORUM_TEST_APP/Contents/Resources/meeting-worker/python/bin/python3.12" -I -B \
  "$FORUM_TEST_APP/Contents/Resources/diagnostics/check_meeting_worker_bundle.py" \
  --app "$FORUM_TEST_APP" --model "$FORUM_TEST_MODEL" \
  --report "$HOME/Desktop/forum-f01-8b-check.json"
```

诊断不会下载权重。输出包括加载、首 token、总耗时和 Metal 内存数据；它是打包可行性验证，`jobs.run/jobs.cancel` 仍不表示正式摘要任务已经实现。

## 3. 手机局域网与 TLS 配对

1. Mac、iPhone 和 Android 连接到允许设备互访的同一局域网。公共/访客 Wi-Fi 的客户端隔离可能使测试不可用，应记录网络条件。
2. 在运行测试包的 Mac 双击 `Start LAN Test.command`，输入该 Mac 当前的局域网 IPv4。脚本只绑定这个私有地址；有效期 20 分钟。
3. 终端输出完整配对链接 `https://IP:端口/#pair=…`、测试 CA 路径和 SHA-256 指纹。将 `forum-f01-ca.crt` 传到手机，核对为本次生成的证书。不要传 `server.key`。
4. iPhone：安装该测试证书配置后，还需在“设置 → 通用 → 关于本机 → 证书信任设置”中启用其根证书信任。手动安装不会自动授予 SSL 信任，步骤依据 [Apple 官方说明](https://support.apple.com/102390)。
5. Android：在“设置 → 安全和隐私 → 更多安全设置 → 加密与凭据”相关入口安装本次 CA；具体路径因厂商而异。参考 [Google Pixel 官方说明](https://support.google.com/pixelphone/answer/2844832?hl=en)。
6. 在 iPhone Safari、Android Chrome 打开本次完整配对链接。应显示“HTTPS 与配对通过”，计数每秒更新，同时看到中英合成句。浏览器不得保留证书错误提示；仅点“继续访问不安全页面”不能算 TLS 通过。
7. 两台手机同时观察约一分钟；刷新页面应继续有权限。用另一个无痕窗口打开不带 `#pair=…` 的网址，应要求完整配对链接，不应显示受保护句子。
8. Mac 终端按 Control-C：更新应停止、端口应关闭，结果保存在 `Forum-F01-LAN-日期时间/report.json`。重新启动会生成新的证书和配对令牌，旧会话不继续生效。
9. 测试结束后，在手机设置中移除本次 `Forum F01 temporary test CA` 证书/配置。脚本已删除 CA 私钥，退出时会删除服务端私钥；证书本身两天到期。

证书安装只用于验证局域网 TLS 路径。正式产品需要在后续网关阶段设计易用的证书/设备配对流程，本次手工安装不能算产品体验已完成。

## 4. 请回报以下结果

| 验证项 | 结果/证据 |
|---|---|
| M5 Max macOS 版本、内存；是否安装过开发工具 | 待填写 |
| 默认检查是否成功；`worker-check.json` | 待填写 |
| 首次打开系统提示、主窗口、测试字幕、关闭重开 | 待填写 |
| 原 Hen Translator 运行时打开/关闭 Forum，原应用未受影响 | 待填写 |
| 真实 8B 诊断（如准备了权重） | 待填写或未测 |
| iPhone 型号/iOS/Safari、证书信任后连接 | 待填写 |
| Android 型号/系统/Chrome、证书信任后连接 | 待填写 |
| 同时观看、刷新、无令牌拒绝、停止后断开 | 待填写 |
| LAN `report.json`；异常原文 | 待填写 |

报告中的自动配对记录只证明收到过请求，不能替代手机上实际可见、TLS 可信以及应用能打开的人工确认。设备结果回收后，更新 `PROGRESS_ZH.md` 的第一部分出口；全部通过再向用户请求进入第二部分。
