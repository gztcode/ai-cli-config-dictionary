#!/bin/bash
# ============================================================
#  AI CLI 配置字典 · 桌面应用启动器
#
#  双击即可运行；在终端里也可执行：  ./启动预览.command
#  重新拉取最新 schema 后启动：      ./启动预览.command --refresh
# ============================================================

cd "$(dirname "$0")" || exit 1
export PATH="$HOME/.cargo/bin:$PATH"

APP="desktop/src-tauri/target/release/bundle/macos/AI配置字典.app"
BIN="desktop/src-tauri/target/release/cfg-explorer-desktop"

say()  { printf '%s\n' "$*"; }
pause(){ printf '\n按回车键关闭此窗口…'; read -r _; }

if ! command -v cargo >/dev/null 2>&1; then
  say "✗ 没找到 cargo。"
  say "  请先安装 Rust：https://rustup.rs"
  say "  （安装后重新双击本文件即可）"
  pause
  exit 1
fi

# ---- 首次构建 -------------------------------------------------------------
if [ ! -d "$APP" ]; then
  say "▶ 首次运行，正在构建桌面应用（约 2-5 分钟）…"
  ( cd desktop/src-tauri && npx --yes @tauri-apps/cli@2 build --bundles app ) \
    || { say "✗ 构建失败，请查看上方错误"; pause; exit 1; }
fi

# ---- 可选：刷新 schema ----------------------------------------------------
if [ "$1" = "--refresh" ] && [ -x "$BIN" ]; then
  say "▶ 正在拉取各家最新 schema…"
  "$BIN" --refresh >/dev/null 2>&1
  say "   （Codex 的 schema 地址当前返回 403，不影响其它工具）"
fi

# ---- 启动 ----------------------------------------------------------------
open "$APP"
say ""
say "✅ AI 配置字典 已启动"
say ""
say "   数据目录：~/Library/Application Support/cfg-explorer/   （注册表与翻译，可自行编辑）"
say "   缓存目录：~/Library/Caches/cfg-explorer/                （schema 缓存）"
say ""
say "   提示：应用内点「刷新 schema」可拉取各家最新配置定义。"
say ""
pause
