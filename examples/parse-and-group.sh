#!/bin/bash
# 端到端测试：字段提取 → 归组引擎

set -e

export PATH="$HOME/.cargo/bin:$PATH"

echo "=== 1. 运行字段提取器单元测试 ==="
cargo test -p invoice-parse field_extractor --quiet

echo ""
echo "=== 2. 测试 XML 解析器集成 ==="
cargo test -p invoice-parse xml::tests --quiet

echo ""
echo "=== 3. 测试 PDF 解析器集成 ==="
cargo test -p invoice-parse pdf::tests --quiet

echo ""
echo "=== 4. 运行归组引擎测试 ==="
cargo test -p invoice-grouping --test synthetic --quiet

echo ""
echo "✅ 端到端流程验证完成"
