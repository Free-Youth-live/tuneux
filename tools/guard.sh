#!/usr/bin/env bash
# =============================================================================
# tuneux 分层守卫（治理落地）
#
# 把架构边界约定的依赖红线与「重复只降不升」从文档纪律升级为机器检查：
#   1. 依赖红线断言（失败关闭 + --edges all 连 dev/build 依赖一起抓）：
#      commonx 平级叶子 / mediax 不反依 commonx /
#      基础版依赖图永不含 pinx / corex·mediax·commonx·pinx 零 UI 零网络；
#   2. 重复棘轮：统计 tuneux 与 tuneux-fx 同名 .rs 的逐行相同行数，
#      与基线比对，回潮即 fail（基线文件与本脚本同目录）；
#   3. API 面断言 / i18n key 完整性（原有）；
#   4. 敏感词扫描：与 pre-push 钩子共享仓外词表源（tools-governance/，
#      物理隔离——词表含禁用词字面量，入仓即自违反），工作树全量逐文件
#      （006 盘点范围条款：不用扩展名白名单）；
#   5. 文件头 //! 全量检查（编码约定文件头层，crates 下全部 .rs）。
#
# 用法：bash tools/guard.sh（在 code/ 仓库根执行）
# 依赖：cargo（构建依赖树）+ python3（重复计数）。
# 接 pre-push 时，把本脚本内容并入 .git/hooks/pre-push，或直接调用
#   `bash "$(git rev-parse --show-toplevel)/tools/guard.sh"`。
# =============================================================================
set -uo pipefail

fail=0
here="$(cd "$(dirname "$0")" && pwd)"

# ---------------------------------------------------------------------------
# 1. 依赖红线断言（任一命中即 fail；失败关闭：cargo tree 本身跑不成也算 fail，
#    绝不静默放行；--edges all 连 dev/build 依赖一起抓）
# ---------------------------------------------------------------------------
assert_no_dep() {
  local crate="$1" pattern="$2" reason="$3"
  local tree rc hit
  tree="$(cargo tree -p "$crate" --edges all 2>/dev/null)"
  rc=$?
  if [ $rc -ne 0 ] || [ -z "$tree" ]; then
    echo "❌ 依赖断言无法执行：cargo tree -p $crate 失败（rc=$rc）——失败关闭，拒绝放行"
    fail=1
    return
  fi
  hit="$(printf '%s' "$tree" | grep -E "$pattern")"
  if [ -n "$hit" ]; then
    echo "❌ 依赖红线违反：[$crate] 不得依赖 $reason"
    echo "$hit" | head -5
    fail=1
  fi
}

assert_no_dep tuneux-commonx 'tuneux-(corex|mediax|pinx)' 'workspace 内部 crate（平级叶子，防循环）'
assert_no_dep tuneux-mediax   'tuneux-commonx'             '通用层（领域层不反依通用层）'
assert_no_dep tuneux          'tuneux-pinx'                '插件宿主（基础版依赖图永不含 pinx）'
assert_no_dep tuneux-max      'ratatui|crossterm'           'TUI 渲染栈（max 用 egui，永不依赖 ratatui）'
assert_no_dep tuneux-corex    'ratatui|crossterm|egui|reqwest|ureq|hyper|tungstenite|curl' '任何 UI 或网络依赖'
assert_no_dep tuneux-mediax   'ratatui|crossterm|egui|reqwest|ureq|hyper|tungstenite|curl' '任何 UI 或网络依赖'
assert_no_dep tuneux-commonx  'ratatui|crossterm|egui|image|reqwest|ureq|hyper|tungstenite|curl' '任何 UI 或网络依赖'
assert_no_dep tuneux-pinx     'ratatui|crossterm|egui|reqwest|ureq|hyper|tungstenite|curl' '任何 UI 或网络依赖（网络只能经宿主代发钩子）'

# ---------------------------------------------------------------------------
# 2. API 面断言：mediax / pinx 对 corex 的「仅允许名单」源码级检查
#    （依赖图只管 crate 粒度，此处管到「具体引用了哪些类型/常量」——
#      mediax 仅 probe_metadata / AudioParams；pinx 仅 EQ / 压缩槽位类型
#      与测试专用的 EQ_BANDS / spectrum）
# ---------------------------------------------------------------------------
api_out="$(python3 - "$here/.." <<'PY'
import os, sys, re
root = sys.argv[1]

API_ALLOW = {
    'crates/tuneux-mediax/src': {'probe_metadata', 'AudioParams'},
    'crates/tuneux-pinx/src': {'CompressorParams', 'EqParams', 'COMP_SLOTS', 'EQ_SLOTS', 'EQ_BANDS', 'spectrum'},
}

violations = []
for sub, allowed in API_ALLOW.items():
    base = os.path.join(root, sub)
    for dirpath, _, names in os.walk(base):
        for n in names:
            if not n.endswith('.rs'):
                continue
            path = os.path.join(dirpath, n)
            text = open(path, encoding='utf-8').read()
            idents = set()
            for m in re.finditer(r'use\s+tuneux_corex::\{([^}]+)\}', text):
                for part in m.group(1).split(','):
                    part = part.strip()
                    if part:
                        idents.add(part)
            for m in re.finditer(r'tuneux_corex::([A-Za-z_][A-Za-z0-9_]*)', text):
                idents.add(m.group(1))
            for i in sorted(idents - allowed):
                rel = os.path.relpath(path, root)
                violations.append(f'{rel}: tuneux_corex::{i}')

if violations:
    for v in violations:
        print(v)
    sys.exit(1)
PY
)"
api_rc=$?
if [ $api_rc -ne 0 ]; then
  echo "❌ API 面越界（对 corex 的引用超出白名单）："
  echo "$api_out"
  fail=1
else
  echo "✓ API 面断言：mediax/pinx 对 corex 引用均在白名单内"
fi

# ---------------------------------------------------------------------------
# 3. i18n key 完整性：en.txt ⊇ 双端 zh_table（防官方英文漏条目）
# ---------------------------------------------------------------------------
"$here/check-i18n-keys.sh" || fail=1

# ---------------------------------------------------------------------------
# 4. 敏感词扫描（工作树；词表源在仓外 tools-governance/，与钩子同源零漂移）
#    范围条款（编码约定盘点条款）：全量逐文件、不用扩展名白名单；有仓优先
#    git ls-files，无仓回退 find 排 target。THIRD-PARTY-LICENSES 除外
#    （许可全文原文，含核查类英文原词属合法引用）。
# ---------------------------------------------------------------------------
# 词表源：base64 编码入仓（编码态不含禁用词字面、无词边界，扫描道自洽；
# CI 只 checkout 仓库亦可用）。仓外 tools-governance/sensitive-re.sh 为人工
# 编辑母本，改后重编码：base64 -i ../tools-governance/sensitive-re.sh -o tools/sensitive-re.b64
eval "$(base64 -d < "$here/sensitive-re.b64")" || { echo "❌ 词表源解码失败（sensitive-re.b64 缺失/损坏）——失败关闭"; exit 1; }
# 解码后非空校验：防「source 静默失败 → 空词表 → 空扫描假绿」的 fail-open 类缺陷
if [ -z "${CONTENT_RE:-}" ] || [ -z "${MSG_RE:-}" ] || [ -z "${SENS_RE:-}" ]; then
  echo "❌ 词表源解码结果为空——失败关闭"; exit 1
fi
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  file_list=$(git ls-files)
else
  file_list=$(find . -path ./target -prune -o -type f -print | sed 's|^\./||')
fi
sens_fail=0
sens_hits=""
while IFS= read -r f; do
  [ -f "$f" ] || continue
  case "$f" in THIRD-PARTY-LICENSES*) continue ;; esac
  h=$(grep -IiE -e "$CONTENT_RE" -e "$SENS_RE" "$f" 2>/dev/null); rc=$?
  if [ $rc -ge 2 ]; then
    echo "❌ 敏感词扫描失败：$f（grep 错误码 $rc）——失败关闭"
    sens_fail=1; fail=1
  elif [ $rc -eq 0 ]; then
    sens_hits="${sens_hits}${f}: ${h}"$'\n'
  fi
done <<< "$file_list"
if [ -n "$sens_hits" ]; then
  echo "❌ 工作树命中敏感词/代号/敏感内容："; printf '%s' "$sens_hits" | head -5; fail=1
elif [ $sens_fail -eq 0 ]; then
  echo "✓ 敏感词扫描：工作树零命中"
fi

# ---------------------------------------------------------------------------
# 5. 文件头 //! 全量检查（编码约定文件头层：每个 .rs 都要有）
# ---------------------------------------------------------------------------
missing_headers=""
while IFS= read -r f; do
  head -1 "$f" | grep -q '^//!' || missing_headers="${missing_headers}${f}"$'\n'
done < <(find crates -name '*.rs' -not -path '*/target/*')
if [ -n "$missing_headers" ]; then
  echo "❌ 缺文件头 //! 的 .rs："; printf '%s' "$missing_headers" | head -5; fail=1
else
  echo "✓ 文件头：全部 .rs 具备 //!"
fi

# ---------------------------------------------------------------------------
# 6. 重复棘轮：双端同名 .rs 逐行相同行数，只降不升
# ---------------------------------------------------------------------------
baseline_file="$here/duplicate-baseline.txt"
baseline="$(cat "$baseline_file" 2>/dev/null || echo 0)"
now="$(python3 - "$here/.." <<'PY'
import os, sys, difflib
root = sys.argv[1]

def files(sub):
    out = {}
    base = os.path.join(root, sub)
    for dirpath, _, names in os.walk(base):
        for n in names:
            if n.endswith('.rs'):
                rel = os.path.relpath(os.path.join(dirpath, n), base)
                out[rel] = os.path.join(dirpath, n)
    return out

base = files('crates/tuneux/src')
fx = files('crates/tuneux-fx/src')
total = 0
for rel in sorted(set(base) & set(fx)):
    a = open(base[rel], encoding='utf-8').read().splitlines()
    b = open(fx[rel], encoding='utf-8').read().splitlines()
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    total += sum(block.size for block in sm.get_matching_blocks())
print(total)
PY
)"

if ! [[ "$now" =~ ^[0-9]+$ ]]; then
  echo "❌ 重复计数失败（python3 或路径异常）"
  fail=1
elif [ "$now" -gt "$baseline" ]; then
  echo "❌ 双端重复回潮：当前 $now 行 > 基线 $baseline 行（新增重复请先更新基线或先收口）"
  fail=1
else
  echo "✓ 重复棘轮：当前 $now 行 ≤ 基线 $baseline 行"
fi

# ---------------------------------------------------------------------------
# 6.5 codec 方向断言：codec 层不得反向依赖 engine 层
#     （T53 内部分层后，方向由 guard 机器守护；codec 只做字节解析，
#      不得引用线程 / cpal / DSP 等 engine 侧类型）
CODEC_ENGINE_REFS=$(grep -rn \
  -e "use crate::audio::engine" \
  -e "use super::super::engine" \
  -e "crate::audio::engine::" \
  "$here/../crates/tuneux-corex/src/audio/codec/" \
  --include="*.rs" 2>/dev/null || true)
if [ -n "$CODEC_ENGINE_REFS" ]; then
  echo "❌ codec 层反向依赖 engine（T53 分层违反）："
  echo "$CODEC_ENGINE_REFS" | head -5
  fail=1
fi

# ---------------------------------------------------------------------------
# 7. 发布管线纯度：交付构建不得启用诊断 feature
#    （站长口径：max 仅应用程序形态交付。旧 UI 的 smoke 无头诊断机制已随
#      UI 重写删除；default = [] 保留为交付纯度基线，新增 feature 必须默认关）
# ---------------------------------------------------------------------------
wf="$here/../.github/workflows/release.yml"
if [ -f "$wf" ] && grep -n -- '--features smoke\|--all-features' "$wf" >/dev/null 2>&1; then
  echo "❌ release.yml 含 smoke/all-features 构建参数（交付管线纯度违反）"
  fail=1
else
  echo "✓ 发布管线：交付构建不启用 smoke 诊断 feature"
fi
if ! grep -q '^default = \[\]' "$here/../crates/tuneux-max/Cargo.toml"; then
  echo "❌ tuneux-max 的 features default 非空（交付二进制将编入诊断代码）"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "分层守卫未通过（$fail 项违规）"
  exit 1
fi
echo "分层守卫通过：依赖红线 + API 面断言 + 重复棘轮"
exit 0
