#!/usr/bin/env bash
# =============================================================================
# tuneux · 钩子安装器
#
# 把入库的钩子源码（tools/hooks/pre-push）安装到 .git/hooks/pre-push
# （字节一致 + 可执行位）。批 0（git init 后）与任何钩子修订后各跑一次。
#
# 用法：bash tools/hooks/install.sh（在 code/ 仓库根执行）
# =============================================================================
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

if [ ! -d "$root/.git" ]; then
  echo "❌ $root 不是 git 仓库（批 0 收口后再安装）"
  exit 1
fi

mkdir -p "$root/.git/hooks"
cp "$here/pre-push" "$root/.git/hooks/pre-push"
chmod +x "$root/.git/hooks/pre-push"

# 字节一致校验（防手改 .git/hooks 副本造成源码与实装漂移）
if cmp -s "$here/pre-push" "$root/.git/hooks/pre-push"; then
  echo "✓ pre-push 已安装（与 tools/hooks/pre-push 字节一致）"
else
  echo "❌ 安装后字节不一致，请检查"; exit 1
fi

# 语法自检
bash -n "$root/.git/hooks/pre-push" && echo "✓ bash -n 语法通过"
