#!/bin/bash
# Prepare test images for OCR validation
# Converts PDFs and identifies existing JPG samples

set -e

SAMPLES_DIR="/home/holo/work-tools/fixtures/samples"
OUTPUT_DIR="/home/holo/work-tools/fixtures/ocr-test-images"

mkdir -p "$OUTPUT_DIR"

echo "=== Existing JPG samples ==="
find "$SAMPLES_DIR" -name "*.jpg" | head -10

echo ""
echo "=== Available PDF samples ==="
find "$SAMPLES_DIR" -name "*.pdf" | head -10

echo ""
echo "=== Available OFD samples ==="
find "$SAMPLES_DIR" -name "*.ofd" | head -10

echo ""
echo "Test images will be prepared in: $OUTPUT_DIR"
