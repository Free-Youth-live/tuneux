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
# 另在输出目录（.app 同级）放一份《使用手册-tuneux-max.txt》
# （2026-10-20 起生效）：同级一份开箱可见，Resources 一份随 .app
# 单独流转不丢；两份同源同版，版本号在脚本内统一注入（源文件里的
# __VERSION__ 占位符不动仓内原件，只改拷贝）。
#
# 配置落点：portable_path 优先 exe 同目录（bundle 内 MacOS/）；不可写时
# 自动回退系统配置目录——/Applications 权限不足的用户不受影响。
#
# 用法（macOS）：bash tools/package-macos-app.sh <二进制> <版本号> <输出目录> [ffmpeg目录]
#   ffmpeg 目录默认素材区产出 ../ffmpeg/build/ffmpeg；存在即并入
#   Contents/Resources/ffmpeg/ 并执行四步强制校验（见脚本内注释），
#   缺失则打印醒目警告继续（本地开发形态，发布链必须提供）。
# 首版不签名：首次打开需右键→打开或系统设置放行（随包 README 有三平台教学）。
# =============================================================================
set -euo pipefail

BIN="$1"
VER="$2"
OUT="$3"
FFMPEG_DIR_ARG="${4:-}"

APP="$OUT/tuneux-max.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS/locales" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/tuneux-max"
chmod +x "$APP/Contents/MacOS/tuneux-max"

# 语言表随 bundle（exe 同目录口径）
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FFMPEG_DIR="${FFMPEG_DIR_ARG:-$REPO_ROOT/../ffmpeg/build/ffmpeg}"
cp -R "$REPO_ROOT/locales/." "$APP/Contents/MacOS/locales/"

# 内嵌字体（Sarasa Mono SC，OFL）许可文本随包——二进制嵌入即分发，
# OFL §1/§3 的文本保留义务落地；原 fonts/ 可选字体段已随字体收口移除。
cp "$REPO_ROOT/crates/tuneux-max/assets/LICENSE-SarasaMonoSC-OFL.txt" "$APP/Contents/Resources/"
# 用户自投字体目录（默认空，仅为 fonts/ 扫描机制保留；存在则随包）
if ls "$REPO_ROOT/crates/tuneux-max/assets/fonts/"*.ttf >/dev/null 2>&1; then
  mkdir -p "$APP/Contents/MacOS/fonts"
  cp "$REPO_ROOT/crates/tuneux-max/assets/fonts/"*.ttf "$APP/Contents/MacOS/fonts/"
fi

# 用户可读文档进 Resources + .app 同级各一份；版本号在此统一注入
#（原 release.yml 的 sed 步骤退役为无害冗余，保留亦可）。
sed_cmd() { sed -i '' "s/__VERSION__/$VER/g" "$1"; }
cp "$REPO_ROOT/使用手册-tuneux-max.txt" "$APP/Contents/Resources/使用手册-tuneux-max.txt"
sed_cmd "$APP/Contents/Resources/使用手册-tuneux-max.txt"
cp "$APP/Contents/Resources/使用手册-tuneux-max.txt" "$OUT/使用手册-tuneux-max.txt"

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

# —— FFmpeg 组件并入（四步强制；目录存在才执行，缺失则警告继续）——
# ① 真拷贝（-L 解引用，杜绝符号链接混入发布包）
# ② bin 二进制 SHA-256 对 VERSIONS.txt 登记值逐位校验
# ③ 闸门：版本行 + 配置串两禁扫描（与运行时闸门同源判定）
# ④ licenses/COPYING.LGPLv2.1 与 VERSIONS.txt 完整性（存在且非空）
if [ -d "$FFMPEG_DIR" ] && [ -f "$FFMPEG_DIR/bin/ffmpeg" ]; then
  rm -rf "$APP/Contents/Resources/ffmpeg"
  mkdir -p "$APP/Contents/Resources"
  cp -R -L "$FFMPEG_DIR" "$APP/Contents/Resources/ffmpeg"
  # download-artifact v4 不保留可执行位（run 18 实证 exit 126）：拷入后
  # 显式补齐——下方闸门自检要执行它，随包分发的二进制更必须带执行位
  chmod +x "$APP/Contents/Resources/ffmpeg/bin/ffmpeg" \
           "$APP/Contents/Resources/ffmpeg/bin/ffprobe"
  FFV="$APP/Contents/Resources/ffmpeg/bin/ffmpeg"
  WANT_F="$(grep -E '^  ffmpeg  = ' "$FFMPEG_DIR/VERSIONS.txt" | awk '{print $3}')"
  WANT_P="$(grep -E '^  ffprobe = ' "$FFMPEG_DIR/VERSIONS.txt" | awk '{print $3}')"
  GET_F="$(shasum -a 256 "$FFV" | awk '{print $1}')"
  GET_P="$(shasum -a 256 "$APP/Contents/Resources/ffmpeg/bin/ffprobe" | awk '{print $1}')"
  [ "$GET_F" = "$WANT_F" ] || { echo "❌ ffmpeg 哈希与 VERSIONS.txt 不一致"; exit 1; }
  [ "$GET_P" = "$WANT_P" ] || { echo "❌ ffprobe 哈希与 VERSIONS.txt 不一致"; exit 1; }
  VOUT="$("$FFV" -version 2>&1)"
  echo "$VOUT" | head -1 | grep -q "ffmpeg version 9\." \
    || { echo "❌ ffmpeg 版本行异常"; exit 1; }
  echo "$VOUT" | grep -qE -- "--enable-(gpl|nonfree)" \
    && { echo "❌ ffmpeg 配置串含禁项（gpl/nonfree），中止打包"; exit 1; } || true
  [ -s "$APP/Contents/Resources/ffmpeg/licenses/COPYING.LGPLv2.1" ] \
    || { echo "❌ COPYING.LGPLv2.1 缺失或为空"; exit 1; }
  [ -s "$APP/Contents/Resources/ffmpeg/VERSIONS.txt" ] \
    || { echo "❌ VERSIONS.txt 缺失或为空"; exit 1; }
  echo "✓ FFmpeg 四步校验通过（拷贝/哈希/闸门/完整性）"
else
  echo "⚠⚠ 未找到 ffmpeg 目录（$FFMPEG_DIR）——本包不含 FFmpeg 组件，"
  echo "   长尾格式将依赖系统安装版本；发布链构建必须提供该目录！"
fi

# sha 自校验：bundle 内二进制必须与源构建逐字节一致（删源前的最后对账）
SRC_SHA=$(shasum -a 256 "$BIN" | awk '{print $1}')
DST_SHA=$(shasum -a 256 "$APP/Contents/MacOS/tuneux-max" | awk '{print $1}')
if [ "$SRC_SHA" != "$DST_SHA" ]; then
  echo "❌ bundle 内二进制与源构建 sha 不一致（${SRC_SHA:0:12} vs ${DST_SHA:0:12}）"
  exit 1
fi
echo "✓ sha 一致：${SRC_SHA:0:12}"

# 交付目录纯度（产品口径）：打包完成后删除源裸二进制——
# 交付目录只留 .app，max 在盘上不存在任何可被终端直接启动的裸文件。
# （cargo 下次构建会重生中间物，属构建缓存而非交付面。）
rm -f "$BIN"

echo "✓ .app 已生成：${APP}（源裸二进制已按交付纯度口径删除）"
echo "✓ 使用手册双份：${OUT}/使用手册-tuneux-max.txt（同级）+ Resources/（bundle 内）"
