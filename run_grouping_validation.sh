#!/bin/bash
set -e

cd /home/holo/work-tools
source scripts/tauri-env.sh

echo "Running grouping validation test..."
cargo test -p invoice-assistant test_grouping_validation --release -- --nocapture 2>&1 | tee /tmp/grouping_validation_output.txt

echo ""
echo "Extracting statistics..."

# Extract key metrics from output
INPUT_COUNT=$(grep "输入发票数：" /tmp/grouping_validation_output.txt | sed 's/.*：//')
GROUP_COUNT=$(grep "归组数量：" /tmp/grouping_validation_output.txt | sed 's/.*：//')
AVG_SIZE=$(grep "平均组大小：" /tmp/grouping_validation_output.txt | sed 's/.*：//')
MAX_SPAN=$(grep "最大时间跨度：" /tmp/grouping_validation_output.txt | sed 's/.*：//' | sed 's/ 天//')

echo "{"
echo "  \"input_invoice_count\": ${INPUT_COUNT:-0},"
echo "  \"group_count\": ${GROUP_COUNT:-0},"
echo "  \"avg_group_size\": ${AVG_SIZE:-0},"
echo "  \"max_time_span_days\": ${MAX_SPAN:-0}"
echo "}"
