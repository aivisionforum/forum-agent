#!/bin/bash
set -euo pipefail
KIT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$KIT_DIR/AI Vision Forum.app"
RESOURCE_DIR="$APP_DIR/Contents/Resources"
RESULT_DIR="$HOME/Desktop/Forum-F01-check-$(date +%Y%m%d-%H%M%S)"
mkdir -m 700 "$RESULT_DIR"
echo "正在检查独立 Python、MLX 和进程握手。无需安装开发工具，不会打开麦克风。"
"$RESOURCE_DIR/meeting-worker/python/bin/python3.12" -I -B \
  "$RESOURCE_DIR/diagnostics/check_meeting_worker_bundle.py" \
  --app "$APP_DIR" --report "$RESULT_DIR/worker-check.json"
/usr/bin/codesign --verify --deep --strict "$APP_DIR"
/usr/bin/sw_vers > "$RESULT_DIR/macos.txt"
/usr/sbin/sysctl -n hw.memsize > "$RESULT_DIR/memory-bytes.txt"
echo "检查完成。请把桌面上的结果文件夹发回当前对话：$RESULT_DIR"
echo "然后双击 AI Vision Forum.app，检查主窗口和测试字幕。"
read -r -p "按回车关闭窗口。" _
