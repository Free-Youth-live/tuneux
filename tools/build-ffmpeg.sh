#!/usr/bin/env bash
# =============================================================================
# tuneux · FFmpeg 合规构建脚本（唯一入口；CI 与本地共用同一逻辑）
#
# 职责：按 tools/ffmpeg-pin.toml 钉死的版本/哈希/配置串，从官方源码包
# 构建 LGPL 纯度的 ffmpeg / ffprobe，并产出可随发布包分发的目录：
#
#   <out>/ffmpeg/
#     bin/ffmpeg[.exe]           已 strip 的可执行
#     bin/ffprobe[.exe]
#     licenses/COPYING.LGPLv2.1  自源码包原样拷入（文本保留义务）
#     VERSIONS.txt               版本/来源/SHA-256/配置串/构建信息/源码要约
#
# 产物自检（失败即退出非零，绝不产出可疑目录）：
#   1. 源码包 SHA-256 逐位比对（本地包与下载包同一标准）
#   2. 配置串不得含 --enable-gpl / --enable-nonfree（与运行时许可闸门
#      同源判定——FFmpeg 9 的 -version 已不再输出许可段文字，配置串
#      是许可形态的唯一机器可读依据：未启用两项即 LGPL-2.1-or-later）
#
# 源码获取优先级（离线优先，网络只作兜底）：
#   --tarball <path> 或环境变量 FFMPEG_TARBALL > 素材区原件 >
#   pin 文件的官方 URL 下载
#
# 跨平台：本脚本只做通用流程；平台差异经环境变量注入——
#   FFMPEG_CC           C 编译器（默认 cc；交叉时如 "zig cc"）
#   FFMPEG_CROSS_OPTS   附加 configure 参数（如 --arch=aarch64 --target-os=linux
#                       或 --arch=x86_64 --target-os=darwin）
#   FFMPEG_STRIP        strip 工具（默认 strip；交叉时按平台指定）
#   FFMPEG_EXTRA_CFLAGS 附加 CFLAGS（交叉 sysroot 等）
#
# 用法：
#   bash tools/build-ffmpeg.sh [--tarball <tar.xz>] [--out <dir>] [--jobs N]
# 默认输出：仓库根上级的 ffmpeg/build/（素材区，构建产物不出素材区）
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PIN="$SCRIPT_DIR/ffmpeg-pin.toml"

# —— pin 文件解析（grep/sed 轻解析，不引外部依赖） ——
pin_get_str() { grep -E "^$1 =" "$PIN" | head -1 | sed -E 's/^[^=]+= *"([^"]*)".*/\1/'; }
FF_VERSION="$(pin_get_str version)"
FF_URL="$(pin_get_str url)"
FF_SHA256="$(pin_get_str sha256)"
[ -n "$FF_VERSION" ] && [ -n "$FF_URL" ] && [ -n "$FF_SHA256" ] || {
  echo "❌ ffmpeg-pin.toml 解析失败（version/url/sha256 缺失）"; exit 1; }
# configure_args：多行数组 → 空格分隔串
FF_CONFIGURE_ARGS="$(sed -n '/^configure_args = \[/,/\]/p' "$PIN" \
  | grep -oE '"[^"]+"' | tr -d '"' | tr '\n' ' ' | sed 's/ $//')"

# —— 参数 ——
TARBALL="${FFMPEG_TARBALL:-}"
OUT="$REPO_ROOT/../ffmpeg/build"
JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
while [ $# -gt 0 ]; do
  case "$1" in
    --tarball) TARBALL="$2"; shift 2 ;;
    --out)     OUT="$2"; shift 2 ;;
    --jobs)    JOBS="$2"; shift 2 ;;
    *) echo "未知参数：$1"; exit 1 ;;
  esac
done

# --out 目录必须先建：curl -o 不自建父目录（缺目录时报 curl(23)
# "Failure writing output to destination" 而非明确的目录错误），且第 2 步的
# work/ 与第 4 步的 ffmpeg/ 产出也都挂在它下面，故在取源码包之前统一创建。
# 非空校验：OUT 为空会让第 4 步的 rm -rf "$OUT/ffmpeg" 退化到根路径。
[ -n "$OUT" ] || { echo "❌ --out 不得为空"; exit 1; }
mkdir -p "$OUT"

CC_BIN="${FFMPEG_CC:-cc}"
STRIP_BIN="${FFMPEG_STRIP:-strip}"
FFMPEG_CROSS_OPTS="${FFMPEG_CROSS_OPTS:-}"
FFMPEG_EXTRA_CFLAGS="${FFMPEG_EXTRA_CFLAGS:-}"

# —— 第 1 步：取源码包并校验哈希 ——
MATERIALS="$REPO_ROOT/../ffmpeg"
if [ -z "$TARBALL" ]; then
  if [ -f "$MATERIALS/ffmpeg-$FF_VERSION.tar.xz" ]; then
    TARBALL="$MATERIALS/ffmpeg-$FF_VERSION.tar.xz"
    echo "· 使用素材区原件：$TARBALL"
  else
    TARBALL="$OUT/ffmpeg-$FF_VERSION.tar.xz"
    echo "· 下载官方源码包：$FF_URL"
    curl -fL --retry 3 -o "$TARBALL" "$FF_URL"
  fi
fi
echo "· SHA-256 校验（期望 ${FF_SHA256}）"
GOT="$(shasum -a 256 "$TARBALL" 2>/dev/null | awk '{print $1}' || sha256sum "$TARBALL" | awk '{print $1}')"
[ "$GOT" = "$FF_SHA256" ] || { echo "❌ 哈希不匹配：$GOT"; exit 1; }
echo "✓ 哈希一致"

# —— 第 2 步：解包（out-of-tree，源码树只读） ——
WORK="$OUT/work-$$"
BUILD="$WORK/build"
mkdir -p "$BUILD"
trap 'rm -rf "$WORK"' EXIT
tar -xJf "$TARBALL" -C "$WORK"
SRC="$WORK/ffmpeg-$FF_VERSION"
[ -d "$SRC" ] || { echo "❌ 解包目录异常"; exit 1; }

# —— 第 3 步：configure + make（LGPL 规格） ——
echo "· configure（$FF_CONFIGURE_ARGS ${FFMPEG_CROSS_OPTS}）"
( cd "$BUILD" && "$SRC/configure" \
    --cc="$CC_BIN" \
    $FF_CONFIGURE_ARGS \
    ${FFMPEG_CROSS_OPTS:-} \
    ${FFMPEG_EXTRA_CFLAGS:+--extra-cflags="$FFMPEG_EXTRA_CFLAGS"} \
    2>&1 | tail -3 )
echo "· make -j$JOBS"
# 成功时保持静默（FFmpeg 构建输出上万行，会淹没 CI 日志）；失败时留末 40 行。
# 交叉编译（zig cc / msys2 mingw64 / mac x86_64 交叉）是本脚本最易失败处，
# 全量丢弃输出会让 CI 只报退出码、无从判断病因，白搭一轮往返。
if ! ( cd "$BUILD" && make -j"$JOBS" ) > "$OUT/make.log" 2>&1; then
  echo "❌ make 失败，末 40 行："
  tail -40 "$OUT/make.log"
  exit 1
fi
for b in ffmpeg ffprobe; do
  [ -f "$BUILD/$b" ] || [ -f "$BUILD/$b.exe" ] || { echo "❌ $b 未产出"; exit 1; }
done
echo "✓ 构建完成"

# —— 第 4 步：组装产出目录 ——
DIST="$OUT/ffmpeg"
rm -rf "$DIST"
mkdir -p "$DIST/bin" "$DIST/licenses"
for b in ffmpeg ffprobe; do
  if [ -f "$BUILD/$b.exe" ]; then
    "$STRIP_BIN" "$BUILD/$b.exe" 2>/dev/null || true
    cp "$BUILD/$b.exe" "$DIST/bin/"
  else
    "$STRIP_BIN" "$BUILD/$b" 2>/dev/null || true
    cp "$BUILD/$b" "$DIST/bin/"
  fi
done
cp "$SRC/COPYING.LGPLv2.1" "$DIST/licenses/"

# —— 第 5 步：产物自检（许可纯度 + 禁项扫描） ——
FF_BIN="$DIST/bin/ffmpeg"
[ -f "$FF_BIN" ] || FF_BIN="$DIST/bin/ffmpeg.exe"
# ffprobe 同口径回退：Windows（msys2/mingw64）腿产出为 .exe，下方哈希登记
# 若硬编码无后缀路径会取不到文件，在 set -e 下中断整个构建。
FFPROBE_BIN="$DIST/bin/ffprobe"
[ -f "$FFPROBE_BIN" ] || FFPROBE_BIN="$DIST/bin/ffprobe.exe"
VERSION_OUT="$("$FF_BIN" -version 2>&1)"
echo "$VERSION_OUT" | head -1 | grep -q "ffmpeg version $FF_VERSION" \
  || { echo "❌ 版本行异常：$(echo "$VERSION_OUT" | head -1)"; exit 1; }
echo "$VERSION_OUT" | grep -qE -- "--enable-(gpl|nonfree)" \
  && { echo "❌ 配置串含禁项（gpl/nonfree），拒绝产出"; exit 1; } || true
echo "✓ 自检通过（版本 + 配置串两禁；与运行时闸门同源判定）"

# —— 第 6 步：生成 VERSIONS.txt（版本登记 + 源码要约） ——
CONFIG_LINE="$(echo "$VERSION_OUT" | grep '^configuration:' | head -1)"
BIN_SHA="$(shasum -a 256 "$FF_BIN" 2>/dev/null | awk '{print $1}' || sha256sum "$FF_BIN" | awk '{print $1}')"
PROBE_SHA="$(shasum -a 256 "$FFPROBE_BIN" 2>/dev/null | awk '{print $1}' || sha256sum "$FFPROBE_BIN" | awk '{print $1}')"
BUILD_DATE="$(date '+%Y-%m-%d %H:%M %Z')"
cat > "$DIST/VERSIONS.txt" <<EOT
FFmpeg ${FF_VERSION}（LGPL-2.1-or-later；官方源码未修改构建）
=====================================================================

来源        ：$FF_URL
源码包 SHA-256：$FF_SHA256
构建配置    ：$FF_CONFIGURE_ARGS ${FFMPEG_CROSS_OPTS:-}
实际配置串  ：$CONFIG_LINE
构建时间    ：$BUILD_DATE
二进制 SHA-256：
  ffmpeg  = $BIN_SHA
  ffprobe = $PROBE_SHA

许可文本随附：licenses/COPYING.LGPLv2.1（不可删改）

源码要约
---------------------------------------------------------------------
本软件分发的 FFmpeg 二进制由上述官方源码包（SHA-256 已登记）的
未修改构建产出。自本版本发布起三年内，向我们索取该完整对应
源码仅收介质工本费；索取渠道：support@tuneux.studio。
同哈希源码包亦随本软件各版本在 GitHub 与 Gitee 的 Release 页提供：

  https://github.com/Free-Youth-live/tuneux/releases
  https://gitee.com/Free_Youth_live/tuneux/releases

FFmpeg 是 FFmpeg 项目的开源产品，与本软件无从属关系；此处为
描述性使用，不构成项目背书。
EOT

echo "✓ 产出目录：$DIST"
du -sh "$DIST/bin"/* | sed 's/^/  /'
echo "完成。"
