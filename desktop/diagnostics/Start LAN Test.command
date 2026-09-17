#!/bin/bash
set -euo pipefail
KIT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESOURCE_DIR="$KIT_DIR/AI Vision Forum.app/Contents/Resources"
RESULT_DIR="$HOME/Desktop/Forum-F01-LAN-$(date +%Y%m%d-%H%M%S)"
echo "请输入这台 Mac 的局域网 IPv4（系统设置 → 网络 → 当前连接的详细信息）。"
read -r -p "例如 192.168.1.20: " FORUM_PROBE_IP
echo "测试会显示合成双语句子；20 分钟自动结束，也可按 Control-C 停止。"
echo "证书和结果位置：$RESULT_DIR"
"$RESOURCE_DIR/meeting-worker/python/bin/python3.12" -I -B \
  "$RESOURCE_DIR/diagnostics/f01_lan_probe.py" \
  --bind "$FORUM_PROBE_IP" --output "$RESULT_DIR"
echo "服务已停止。请保留 report.json，并按说明移除测试证书。"
read -r -p "按回车关闭窗口。" _
