# 第四部分：macOS 打包、完整性与发布边界

## 当前产物与隔离层次

主构建入口是 `desktop/scripts/build_macos_app.sh`，默认 `--profile dev`。
它从当前源代码生成 Tauri app，复制固定依赖的运行时，验证完整性后才替换
同一输出目录中的旧 Forum app；检查失败保留或恢复旧包。不能覆盖其他产品
目录或 app symlink，也不能替换正在运行的 Forum app。

运行时有两套独立 Python 环境：

| 组件 | 进程 | Python 环境 |
| --- | --- | --- |
| 会议分析 | 独立 meeting worker/计算子进程 | `Resources/meeting-worker/python` |
| 可选 Whisper ASR | 独立 ASR worker | 与 meeting 共享已验证的固定依赖环境 |
| 匿名声纹 ECAPA | 独立 speaker worker/计算子进程 | `Resources/speaker-worker/python` |

共享安装环境不等于共享模型进程。ECAPA 的依赖与解释器完全独立，避免改变原有
ASR/MLX 环境。所有进程仍须遵守宿主的退出、deadline 和资源调度；打包不证明
多模型并发性能通过。运行时不包含权重、不自动下载 ECAPA 模型，也不包含开发者
`.venv` 或源代码 checkout 路径。

## 本机开发包

在项目根目录执行，例如：

```sh
RUSTUP_TOOLCHAIN=stable \
FORUM_AGENT_BUILD_PYTHON=/absolute/path/to/python3.12 \
FORUM_AGENT_WORKER_CACHE=/absolute/pinned-artifact-cache \
FORUM_AGENT_CARGO_TARGET_DIR=/tmp/forum-shell-cargo-target \
bash desktop/scripts/build_macos_app.sh --profile dev --offline
```

`--offline` 同时限制 Cargo、npm 和两个 Python packager 使用已有缓存。缓存不足
会明确失败，不把缺包情况伪装成离线构建成功。npm 使用 `npm ci` 校验并安装
`package-lock.json`，Cargo 使用 `--locked`。Python/原生 wheel 按 URL、版本、尺寸
和 SHA256 固定，安装后再核对依赖闭包。使用的具体 Rust 编译器版本写入 app
manifest；`stable` 是本机工具链选择，不代表跨日期固定的编译器版本。

开发包签名方式是 ad-hoc，实际签名必须通过 `codesign --verify --deep --strict`；
这不等于 Developer ID 或 Apple notarization。DMG 也明确区分 profile：

```sh
bash desktop/scripts/build_macos_dmg.sh --profile dev
```

默认文件名包含 `-dev.dmg`，卷内附开发测试说明。DMG 输出必须是新文件，不覆盖
已有候选。创建后执行 `hdiutil verify`，记录容器尺寸、SHA256、app 签名类别及
notarization 状态。制作 DMG 不会提交 Apple，也不会改变麦克风权限。

## App 完整性检查

`Contents/Resources/release-manifest.json` 记录构建 profile、版本、签名预期、
模型权重缺省状态、Rust 版本和锁文件摘要。`build-inputs/` 保留：

- meeting/ASR 和 speaker 的运行时 lock；
- Cargo.lock 与前端 package-lock.json；
- 数据协议 schema 和 Forum profile；
- 随包离线模型准备工具的源码副本与摘要。

`desktop/scripts/check_forum_app.py` 验证输入摘要、app 标识/版本、两个 Python
组件的每个实际安装源码文件、ASR 适配器摘要，以及宿主二进制架构/最低系统版本。
构建期间还与当前 repo 文件比较，拒绝混入旧代码或旧依赖的运行时。

`--runtime-checks` 在隔离环境中启动各组件自带的检查器，验证 interpreter/sys.path、
依赖版本、native loader 路径和无模型的协议启动/关闭。构建在签名前及移动到最终
资源路径后各检查一次，之后每次可只读重复检查：

```sh
"desktop/dist/AI Vision Forum.app/Contents/Resources/meeting-worker/python/bin/python3.12" \
  -I -B desktop/scripts/check_forum_app.py \
  --app "desktop/dist/AI Vision Forum.app" --profile dev --runtime-checks
```

声纹 standalone 已在全新 portable Python 内以 `-I -m forum_speaker_worker`
执行真实 ECAPA 推理；模型来自本机已有缓存。该 probe 使用生成波形，只能证明
真实模型与协议运行，不能证明多人区分、重叠检测或正式 C3 准确性。
详见 `services/speaker-worker/packaging/LOCAL_VALIDATION.md`。

## Developer ID 发布候选与 Apple 公证

`--profile release` 在任何编译前要求已有 `APPLE_SIGNING_IDENTITY`。没有凭证就
失败，不退回 ad-hoc 并继续称为 release。脚本不创建、导出、更换证书，也不向
用户 Keychain 写入账号、密码或配置。

```sh
APPLE_SIGNING_IDENTITY='Developer ID Application: ...' \
bash desktop/scripts/build_macos_app.sh --profile release --offline
bash desktop/scripts/build_macos_dmg.sh --profile release
```

签名通过单进程文件魔数扫描识别 app 内 Mach-O、FAT 和 MetalLib，逐个签署嵌套代码；
主程序留给最后的 app 容器签名，避免 Python 文本文件扫描产生大量子进程。
扫描不跟随软链，扫描失败立即停止，最终仍执行严格的整个 app 验签。
release 模式启用 hardened runtime
并验证实际证书链、TeamIdentifier、runtime 标志及完整签名。构建 app 或制作 DMG
仍不会自动公证。release 候选应先在目标机器验证所有 ML/native 调用在 hardened
runtime 下的行为；本机 ad-hoc 测试不覆盖这一关。

只有明确执行下一步时才提交已检查的 release DMG：

```sh
APPLE_SIGNING_IDENTITY='Developer ID Application: ...' \
APPLE_NOTARY_KEYCHAIN_PROFILE='existing-profile' \
bash desktop/scripts/macos_sign_and_notarize.sh \
  --dmg /absolute/path/to/release-candidate.dmg --profile release
```

只引用已有的 notarytool Keychain profile，不把密码放入命令参数。先核对 DMG 与
候选 manifest 的 SHA256，再签容器、提交 Apple。仅当 `notarytool` 返回 Accepted、
`stapler staple/validate` 通过且 `spctl` 接受之后，才记录
`accepted_and_stapled`。提交失败/超时/拒绝都不能当作通过；签名、提交中和公证完成
状态分别记录。不能仅凭 app 的构建 manifest 推断公证结果。

## 仍须验收的边界

- 第二台无开发环境的 Apple Silicon Mac 安装、首次启动、权限与离线运行。
- 真实双轨、多人/噪音/插话、长会和多模型并发指标。
- Developer ID 和 hardened runtime 下的端到端推理、Apple 公证及 Gatekeeper。
- app 声明的最低 macOS 必须覆盖所有实际 native 文件；检查出高于声明的库时应
  根据真实 API 重编译或提升产品最低版本，不能只改 dylib metadata 来伪装兼容。
- F12 现场排练和 C1–C10 正式指标不能由构建/签名检查替代。

### 遗留原生 AEC 资源的处理

旧导入包中的 `libAudioCapture.dylib` 实际要求 macOS 15。Forum 的可靠采集链路
使用 CPAL（麦克风）及 ScreenCaptureKit（系统音频），双轨回声处理使用新 Rust
逻辑，不调用遗留 `NativeAudioCapture`。因此移除了 Tauri 的该 dylib resource
声明，并从新候选包排除旧缓存残留；保留导入的源文件和遗留代码用于后续审查。
此处理保留 Forum 的 macOS 14 最低声明，不修改原生库 metadata 冒充支持。
实际 macOS 14 目标机器仍需验收。

## 新 Mac 会前离线模型准备

新机器需要先通过可信渠道准备好对应的**本地模型文件夹**（例如将已完整准备的
文件夹复制到移动硬盘）。模型下载/账号/许可证取得流程不在本工具内。App 随包
提供标准库工具 `Contents/Resources/setup/prepare_forum_models.py`；它使用随包
Python，不要求用户安装 Python、Homebrew 或开发环境。

以下命令假设 app 已复制到 `/Applications/AI Vision Forum.app`。将
`/Volumes/MeetingModels/...` 替换为实际已有目录：

```sh
"/Applications/AI Vision Forum.app/Contents/Resources/meeting-worker/python/bin/python3.12" \
  -I -B "/Applications/AI Vision Forum.app/Contents/Resources/setup/prepare_forum_models.py" \
  --type whisper --source /Volumes/MeetingModels/whisper-large-v3-turbo

"/Applications/AI Vision Forum.app/Contents/Resources/meeting-worker/python/bin/python3.12" \
  -I -B "/Applications/AI Vision Forum.app/Contents/Resources/setup/prepare_forum_models.py" \
  --type qwen3-8b --source /Volumes/MeetingModels/Qwen3-8B-4bit

"/Applications/AI Vision Forum.app/Contents/Resources/meeting-worker/python/bin/python3.12" \
  -I -B "/Applications/AI Vision Forum.app/Contents/Resources/setup/prepare_forum_models.py" \
  --type ecapa --source /Volumes/MeetingModels/ecapa-model
```

它只写当前用户 `~/Library/Application Support/AI Vision Forum/models/` 下的
固定目标，和宿主默认查找路径一致：

| `--type` | 固定目标子目录 | 支持的模型/用途 |
| --- | --- | --- |
| `whisper` | `whisper-large-v3-turbo` | MLX Whisper large-v3-turbo，自动语种 ASR；需 config.json、weights.safetensors |
| `qwen3-8b` | `qwen3-8b` | MLX Qwen3-8B 4-bit/group-64，会议洞察/纪要；需 config、tokenizer 两份 JSON 及完整 safetensors 或分片/index |
| `ecapa` | `ecapa` | 已验证的官方 ECAPA embedding checkpoint，匿名说话人；需 embedding_model.ckpt |

源目录可以来自 Hugging Face snapshot；文件 symlink 只读展开为独立副本，源文件
不被改写。目标及其父路径拒绝 symlink，CLI 不接受任意安装目标。工具以独占锁
防止两个安装同时写入，使用随机临时目录，核对 config/关键文件、safetensors
结构、分片清单、复制前后状态及每份文件 SHA256，然后通过 macOS 原子的
no-replace rename 发布。完整、残缺或软链目标都不覆盖；若已有目录，先由操作员
检查和处理该目录，工具不会代为删除或“修复”。复制中断不会让宿主看到半份模型。

ECAPA 必须匹配当前支持的 checkpoint SHA256：
`0575cb64845e6b9a10db9bcb74d5ac32b326b8dc90352671d345e2ee3d0126a2`。
Qwen/Whisper 输出与宿主算法一致的 `sha256:...` 内容指纹，可从已准备机器的
`.forum-model-install.json` 读取，传 `--expected-manifest sha256:...` 在复制时
核对。Receipt 使用隐藏文件名，不改变宿主的 Qwen fingerprint。安装成功仅表示
文件完整性通过；不会宣称模型推理或会议质量已验收。

固定语种 Qwen3-ASR 与字幕翻译模型沿用 app 原有模型准备入口；上述三种是自动
语种、会议分析及可选声纹的补充。安装后无需环境变量，默认目录即可被宿主识别。
如果使用过显式 `FORUM_WHISPER_MODEL_PATH`、`FORUM_ANALYSIS_MODEL_PATH` 或
`FORUM_SPEAKER_MODEL_PATH`，应移除或修正旧 override，否则宿主仍遵从该显式选择。

## 2026-09-16 本机最终开发候选

最终 app 为 `desktop/dist/AI Vision Forum.app`，对应安装盘是
`desktop/dist/AI-Vision-Forum-v0.1.0-dev.1-part4-final-dev.dmg`。
这是 **dev / ad-hoc** 测试包；未执行 Developer ID 签名或 Apple 公证。
安装盘大小为 **917,471,386 字节**，SHA256：

```text
b46fd0b7d318fc8807160e6045ca7b3efc82226db9a74c687e4a2ba2678ea371
```

本轮已实际完成：

- 从全量回归第 5 轮通过后的冻结源码离线重建；276 份源码摘要与该回归记录一致。
- 最终 app 的两套 portable Python、ASR 适配器、依赖/源码/原生 loader 检查通过。
- 所有嵌套 Mach-O/FAT/MetalLib 签名及最终 app 的严格验签通过；魔数扫描已与
  `file(1)` 对 45,725 份实际文件比较，识别 330 份嵌套代码，无漏项或多项。
- 新 app 内安装的 speaker module 直接使用已有本机 ECAPA checkpoint 完成推理：
  3 秒生成波形、192 维归一化 embedding、耗时 3.614 秒、ping 0.327 ms；退出正常，
  模型缓存未改变。此波形不是多人语音，不证明正式 C3 准确率。
- `hdiutil verify` 通过；新 DMG 只读挂载后，以**盘内** Python 再次通过完整性、
  runtime、签名检查及模型准备 CLI 启动检查，随后已卸载。

过程中未启动主 app UI、麦克风或用户会议数据库，未下载或打包模型权重。
正式干净 Mac 安装、系统权限、真实多人/双轨与长会指标仍待设备验收。
同目录较早的 `-dev.dmg` 和 `-part4-dev.dmg` 是历史候选，不能替代上述最终文件。

本机证据（`artifacts/local` 为本地运行产物）包括：

- `artifacts/local/f11-final-build-source-check.json`；
- `desktop/dist/forum-app-final-check.json`；
- `artifacts/local/f11-part4-final-app-ecapa-probe/report.json`；
- `artifacts/local/f11-part4-final-dmg-check.json`；
- `artifacts/local/f11-part4-final-package-evidence.json`。
