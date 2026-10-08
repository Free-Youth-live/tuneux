#!/usr/bin/env bash
# =============================================================================
# tuneux-max · macOS .app bundle 打包脚本
#
# 生成可双击启动的 tuneux-max.app（裸二进制双击不了且 Gatekeeper 摩擦大）。
# 结构：
#   tuneux-max.app/Contents/
#     Info.plist
#     MacOS/tuneux-max          二进制
#     MacOS/locales/<lang>.txt  语言表（build_i18n 按 exe 同目录查找）
#     Resources/                手册 + 第三方许可（用户右键包内容可见）
#
# 配置落点：portable_path 优先 exe 同目录（bundle 内 MacOS/）；不可写时
# 自动回退系统配置目录——/Applications 权限不足的用户不受影响。
#
# 用法（macOS）：bash tools/package-macos-app.sh <二进制路径> <版本号> <输出目录>
# 首版不签名：首次打开需右键→打开或系统设置放行（随包 README 有三平台教学）。
# =============================================================================
set -euo pipefail

BIN="$1"
VER="$2"
OUT="$3"

APP="$OUT/tuneux-max.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS/locales" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/tuneux-max"
chmod +x "$APP/Contents/MacOS/tuneux-max"

# 语言表随 bundle（exe 同目录口径）
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cp -R "$REPO_ROOT/locales/." "$APP/Contents/MacOS/locales/"

# 可选字体随 bundle（exe 同目录 fonts/，运行时扫描进字体菜单）
if ls "$REPO_ROOT/crates/tuneux-max/assets/fonts/"*.ttf >/dev/null 2>&1; then
  mkdir -p "$APP/Contents/MacOS/fonts"
  cp "$REPO_ROOT/crates/tuneux-max/assets/fonts/"*.ttf "$APP/Contents/MacOS/fonts/"
  cp "$REPO_ROOT/crates/tuneux-max/assets/fonts/"LICENSE*.txt "$APP/Contents/Resources/" 2>/dev/null || true
fi

# 用户可读文档进 Resources
cp "$REPO_ROOT/使用手册-tuneux-max.txt" "$APP/Contents/Resources/"

# 应用图标（ICNS；Finder/Dock 显示源。缺图则跳过，保持默认图标）
if [ -f "$REPO_ROOT/assets/AppIcon.icns" ]; then
  cp "$REPO_ROOT/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
else
  echo "  ⚠ 未找到 assets/AppIcon.icns，本次打包无自定义图标"
fi

# 插件目录（第一方 wasm/manifest/sig 随包分发）
if [ -d "$REPO_ROOT/plugins" ]; then
  mkdir -p "$APP/Contents/MacOS/plugins"
  cp "$REPO_ROOT/plugins/"* "$APP/Contents/MacOS/plugins/"
  echo "  插件已拷入 $(ls "$APP/Contents/MacOS/plugins/" | wc -l | tr -d ' ') 份"
fi
cp "$REPO_ROOT/THIRD-PARTY-LICENSES.txt" "$APP/Contents/Resources/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>zh_CN</string>
  <key>CFBundleExecutable</key>
  <string>tuneux-max</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>tech.jishu.tuneux-max</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>tuneux-max</string>
  <key>CFBundleDisplayName</key>
  <string>tuneux-max</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>__VERSION__</string>
  <key>CFBundleVersion</key>
  <string>__VERSION__</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.13</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
PLIST

# 版本注入（mac sed 语法；本脚本仅 mac 运行）
sed -i '' "s/__VERSION__/$VER/g" "$APP/Contents/Info.plist"

# sha 自校验：bundle 内二进制必须与源构建逐字节一致（删源前的最后对账）
SRC_SHA=$(shasum -a 256 "$BIN" | awk '{print $1}')
DST_SHA=$(shasum -a 256 "$APP/Contents/MacOS/tuneux-max" | awk '{print $1}')
if [ "$SRC_SHA" != "$DST_SHA" ]; then
  echo "❌ bundle 内二进制与源构建 sha 不一致（${SRC_SHA:0:12} vs ${DST_SHA:0:12}）"
  exit 1
fi
echo "✓ sha 一致：${SRC_SHA:0:12}"

# 交付目录纯度（2026-09-28 站长口径）：打包完成后删除源裸二进制——
# 交付目录只留 .app，max 在盘上不存在任何可被终端直接启动的裸文件。
# （cargo 下次构建会重生中间物，属构建缓存而非交付面。）
rm -f "$BIN"

echo "✓ .app 已生成：${APP}（源裸二进制已按交付纯度口径删除）"
