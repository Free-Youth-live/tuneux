#!/usr/bin/env bash
# =============================================================================
# tuneux-max · Linux GUI 应用打包脚本
#
# 与 macOS .app 同一口径（交付纯度）：max 只以「桌面应用」形态交付——
# .desktop 入口（Terminal=false）+ hicolor 图标 + 安装器；归档根目录
# 不放裸二进制，不存在「终端直接 ./tuneux-max 启动」的交付面。
#
# 结构（glibc 2.17 基线产物，见 release.yml zigbuild .2.17）：
#   tuneux-max-<VER>-linux/
#     tuneux-max.desktop      菜单入口（Terminal=false，双击/菜单启动）
#     bin/tuneux-max          二进制（藏于 bin/ 子目录，非根级裸文件）
#     icons/hicolor/...       全尺寸图标（assets/hicolor 生成物）
#     locales/                语言表
#     install.sh              桌面集成安装器（~/.local 三件套）
#     使用手册 / 第三方许可
#
# 用法（Linux）：bash tools/package-linux-app.sh <二进制路径> <版本号> <输出目录>
# 打包完成按纯度口径删除源裸二进制（同 macOS 脚本）。
# =============================================================================
set -euo pipefail

BIN="$1"
VER="$2"
OUT="$3"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
APP="$OUT/tuneux-max-$VER-linux"

rm -rf "$APP"
mkdir -p "$APP/bin" "$APP/locales"

cp "$BIN" "$APP/bin/tuneux-max"
chmod +x "$APP/bin/tuneux-max"

# 语言表随包（exe 同目录口径：运行时向上找 locales/）
cp -R "$REPO_ROOT/locales/." "$APP/locales/"

# hicolor 图标树（assets/hicolor 由图标源图生成；缺图则警告降级）
if [ -d "$REPO_ROOT/assets/hicolor" ]; then
  cp -R "$REPO_ROOT/assets/hicolor" "$APP/icons"
else
  echo "  ⚠ 未找到 assets/hicolor，本包无图标（.desktop 仍可按名回退系统图标）"
fi

# 用户可读文档
cp "$REPO_ROOT/使用手册-tuneux-max.txt" "$APP/"
cp "$REPO_ROOT/THIRD-PARTY-LICENSES.txt" "$APP/"

# .desktop（模板占位符注入：__EXEC__ → 安装器落 ~/.local/bin 后的命令名；
# __ICON__ → hicolor 图标名。Terminal=false 是 GUI 纪律的机器断言）
if [ ! -f "$REPO_ROOT/tools/tuneux-max.desktop" ]; then
  echo "❌ 缺少 tools/tuneux-max.desktop 模板"
  exit 1
fi
sed -e "s|__EXEC__|tuneux-max|g" -e "s|__ICON__|tuneux-max|g" \
  "$REPO_ROOT/tools/tuneux-max.desktop" > "$APP/tuneux-max.desktop"

# 桌面集成安装器：二进制 → ~/.local/bin（PATH）；.desktop + 图标 →
# ~/.local/share。写入后 update-desktop-database（存在才跑，失败不阻断）。
cat > "$APP/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# tuneux-max 桌面集成（用户级，免 sudo）：三件套装入 ~/.local
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons"
cp "$HERE/bin/tuneux-max" "$HOME/.local/bin/"
cp "$HERE/tuneux-max.desktop" "$HOME/.local/share/applications/"
if [ -d "$HERE/icons/hicolor" ]; then
  cp -R "$HERE/icons/hicolor/." "$HOME/.local/share/icons/"
fi
command -v update-desktop-database >/dev/null 2>&1 && \
  update-desktop-database "$HOME/.local/share/applications" || true
echo "✓ 已安装：菜单搜索 tuneux-max，或 ~/.local/bin/tuneux-max"
INSTALL
chmod +x "$APP/install.sh"

# sha 自校验：包内二进制必须与源构建逐字节一致（删源前的最后对账）
SRC_SHA=$(shasum -a 256 "$BIN" | awk '{print $1}')
DST_SHA=$(shasum -a 256 "$APP/bin/tuneux-max" | awk '{print $1}')
if [ "$SRC_SHA" != "$DST_SHA" ]; then
  echo "❌ 包内二进制与源构建 sha 不一致（${SRC_SHA:0:12} vs ${DST_SHA:0:12}）"
  exit 1
fi
echo "✓ sha 一致：${SRC_SHA:0:12}"

# 交付目录纯度（与 macOS 同口径）：打包完成后删除源裸二进制。
rm -f "$BIN"

echo "✓ Linux GUI 应用包已生成：${APP}（源裸二进制已按纯度口径删除）"
