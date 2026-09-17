# F01 可分发 runtime 的第三方文件

`runtime-macos-arm64.lock.json` 是解释器、全部运行 wheel、构建工具和许可证
归档的固定输入清单。构建脚本同时生成资源根 `THIRD_PARTY.md` 和
`runtime-manifest.json`，逐项记录软件版本、官方来源、SHA-256。

CPython 的 install_only 归档仅含 Python 主许可证，未包含所有静态链接库
许可证。为此另下载同 release、同版本、同架构的 full 归档，校验完整 SHA-256
后仅读取 `python/PYTHON.json` 和 `python/licenses/*`。产物保留 19 份上游
许可证文本，涵盖 CPython、OpenSSL、libffi、SQLite、zlib、bzip2、liblzma、
expat、libedit、ncurses、mpdecimal、Tcl 以及上游 full 归档附带的其他库。
保留全部文本，不在这里自行重新判断或变更上游许可。

运行 wheel 的 `.dist-info`、`licenses`、NOTICE 和包内许可文件原样保留。
构建专用 setuptools/wheel/zstandard 不安装进最终 Python；pip/ensurepip 及绝对
staging shebang 的命令行脚本不保留。最终用自己的相对路径启动器运行模块。

这里是软件依赖来源及文件保留说明。模型权重没有随包附带；从现有缓存运行
Qwen3 探针不改变其模型来源与许可，也不意味着第三方模型已获发布授权。
