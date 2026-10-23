#!/bin/bash
# ============================================================
#  构建 macOS .app（桌面应用，Tauri 2）
#  用法： ./打包.app.sh
#
#  产物：desktop/src-tauri/target/release/bundle/macos/AI配置字典.app
#  这是一个完整的独立应用（有原生窗口与菜单栏），不是网页。
# ============================================================
set -e
cd "$(dirname "$0")"

export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  echo "✗ 没找到 cargo，请先安装 Rust：https://rustup.rs"
  exit 1
fi

echo "▶ 1/3 构建核心库"
cargo build --release

echo "▶ 2/3 构建桌面应用"
( cd desktop/src-tauri && cargo build --release )

echo "▶ 3/3 打包 .app"
( cd desktop/src-tauri && npx --yes @tauri-apps/cli@2 build --bundles app )

APP="desktop/src-tauri/target/release/bundle/macos/AI配置字典.app"
echo ""
echo "✅ 已生成：$(pwd)/${APP}"
du -sh "$APP"
echo ""
echo "   双击即可运行。也可以把整个 ${APP} 复制到别处（它自带运行时依赖）。"
echo "   跨平台：Windows 用 --bundles msi，Linux 用 --bundles deb / appimage。"
