# tuneux 官方英文语言文件（en.txt）校验
# 用法：bash tools/check-i18n-keys.sh（在 code/ 仓库根执行）
# 校验：en.txt 的 key 集合 ⊇ 双端 zh_table 的 key 集合（防官方英文漏条目）
# 约定：commonx::i18n 的回退链是 primary → zh → key 本身，
#       en.txt 缺某 key 时该条回退中文（中英混合），本脚本把这种状态视为不完整。
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
fail=0

EN_FILE="$here/../locales/en.txt"

if [ ! -f "$EN_FILE" ]; then
  echo "❌ locales/en.txt 不存在"
  exit 1
fi

# 提取 en.txt 的 key 集合
EN_KEYS=$(grep -oE '^[a-z]+\.[a-z0-9_.]+' "$EN_FILE" | sort -u)

# 提取 base zh_table 的 key 集合（从 Rust 字符串字面量里）
BASE_KEYS=$(sed -n '/fn zh_table/,/^}/p' "$here/../crates/tuneux/src/config.rs" \
  | grep -oE '^[a-z]+\.[a-z0-9_.]+ = ' | sed 's/ = //' | sort -u)

# 提取 fx zh_table 的 key 集合
FX_KEYS=$(sed -n '/fn zh_table/,/^}/p' "$here/../crates/tuneux-fx/src/config.rs" \
  | grep -oE '^[a-z]+\.[a-z0-9_.]+ = ' | sed 's/ = //' | sort -u)

# 提取 max zh_table 的 key 集合（0.6.0 起三产品同链校验）
MAX_KEYS=$(sed -n '/fn zh_table/,/^}/p' "$here/../crates/tuneux-max/src/config.rs" \
  | grep -oE '^[a-z]+\.[a-z0-9_.]+ = ' | sed 's/ = //' | sort -u)

# 合并三端 key
ALL_KEYS=$(printf '%s\n%s\n%s\n' "$BASE_KEYS" "$FX_KEYS" "$MAX_KEYS" | sort -u)

# 检查 en.txt 是否覆盖全部 key
MISSING=$(comm -23 <(echo "$ALL_KEYS") <(echo "$EN_KEYS"))

if [ -n "$MISSING" ]; then
  echo "❌ locales/en.txt 缺少以下 key（官方英文不完整）："
  echo "$MISSING" | sed 's/^/  /'
  fail=1
else
  # 统计
  EN_COUNT=$(echo "$EN_KEYS" | wc -l | tr -d ' ')
  ALL_COUNT=$(echo "$ALL_KEYS" | wc -l | tr -d ' ')
  EXTRA_COUNT=$(comm -13 <(echo "$ALL_KEYS") <(echo "$EN_KEYS") | wc -l | tr -d ' ')
  echo "✓ i18n key 完整性：en.txt $EN_COUNT 条 ≥ 三端 zh_table 合并 $ALL_COUNT 条（en 冗余 $EXTRA_COUNT 条）"
fi

# —— 反向扫描：产品代码里 t("...") 的 key 必须落在该产品 zh_table ——
# 回退链 primary → zh → key 本身；zh 表缺 key 时中文模式会显示原始 key。
# 测试哨兵 key（故意缺、用于断言回退行为）豁免。
REV_SENTINELS='^missing\.key$|^nonexistent\.key$'
for spec in tuneux tuneux-fx tuneux-max; do
  table_keys=$(sed -n '/fn zh_table/,/^}/p' "$here/../crates/$spec/src/config.rs" \
    | grep -oE '^[a-z]+\.[a-z0-9_.]+ = ' | sed 's/ = //' | sort -u)
  used_keys=$(grep -rhoE '\.t\("[a-z][a-z0-9_.]*"\)' "$here/../crates/$spec/src" --include='*.rs' \
    | sed -E 's/.*\.t\("([^"]+)"\).*/\1/' | sort -u | grep -vE "$REV_SENTINELS")
  miss=$(comm -23 <(printf '%s\n' "$used_keys") <(printf '%s\n' "$table_keys"))
  if [ -n "$miss" ]; then
    echo "❌ $spec 代码引用但 zh_table 缺失的 key（中文会显示原始 key）："
    echo "$miss" | sed 's/^/  /'
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  exit 1
fi
