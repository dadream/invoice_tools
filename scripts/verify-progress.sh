#!/bin/bash
# 进度验证脚本 - 每次任务规划前运行

echo "=== 1. 测试状态 ==="
cargo test --workspace --no-fail-fast 2>&1 | grep -E "test result:|running"

echo -e "\n=== 2. 各 crate 的 CLI 功能 ==="
for crate in invoice-collect invoice-parse invoice-grouping; do
  echo "--- $crate ---"
  cargo run -p $crate -- --help 2>&1 | head -15 || echo "无 CLI"
done

echo -e "\n=== 3. 最近 20 次提交 ==="
git log --oneline -20

echo -e "\n=== 4. 各模块导出的公共 API ==="
for crate in crates/*/; do
  echo "--- $(basename $crate) ---"
  grep -h "pub fn\|pub struct\|pub enum" $crate/src/lib.rs 2>/dev/null | head -10
done

echo -e "\n=== 5. 最新实施报告 ==="
ls -lt docs/tasks/*.md docs/*final*.md 2>/dev/null | head -5
