from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


FIELDS = ("invoice_number", "issue_date", "total_amount")
FIELD_LABELS = {
    "invoice_number": "发票号码",
    "issue_date": "开票日期",
    "total_amount": "价税合计",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Render a second-stage private comparison of independently reviewed "
            "source pages and current product field predictions."
        )
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("--parse-result-overlay", type=Path, action="append", default=[])
    parser.add_argument("--font", required=True, type=Path)
    parser.add_argument(
        "--blind-review-name", default="field-ground-truth-review-private-v2"
    )
    parser.add_argument(
        "--output-name", default="field-prediction-comparison-private-v1"
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    font_path = args.font.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    with (capture_root / "field-ground-truth-prelabel.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    manual_rows = [
        row
        for row in rows
        if any(row[f"{field}_status"] == "needs_manual_review" for field in FIELDS)
    ]

    results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    by_sample = {result["sample"]: result for result in results}
    for overlay_arg in args.parse_result_overlay:
        overlay_path = overlay_arg
        if not overlay_path.is_absolute():
            overlay_path = (parse_root / overlay_path).resolve()
        else:
            overlay_path = overlay_path.resolve()
        if overlay_path == parse_root or parse_root not in overlay_path.parents:
            raise ValueError("parse-result overlays must remain in the private parse root")
        for result in json.loads(overlay_path.read_text("utf-8")):
            if result["sample"] not in by_sample:
                raise ValueError("parse-result overlay contains an unknown sample")
            by_sample[result["sample"]] = result

    samples_root = parse_root / "fixtures" / "samples"
    parsed_by_hash: dict[str, dict[str, object]] = {}
    for sample, result in by_sample.items():
        digest = hashlib.sha256((samples_root / sample).read_bytes()).hexdigest()
        parsed_by_hash[digest] = result

    blind_pages = capture_root / args.blind_review_name / "pages"
    output_root = capture_root / args.output_name
    output_root.mkdir(parents=True, exist_ok=False)
    pages_root = output_root / "pages"
    pages_root.mkdir()
    title_font = ImageFont.truetype(str(font_path), 28)
    value_font = ImageFont.truetype(str(font_path), 27)

    rendered = 0
    missing_parse = 0
    index_rows: list[dict[str, str]] = []
    for row in manual_rows:
        result = parsed_by_hash.get(row["sha256"])
        parsed = result.get("parsed") if result else None
        manual_fields = [
            field
            for field in FIELDS
            if row[f"{field}_status"] == "needs_manual_review"
        ]
        source_page = blind_pages / f"{row['blind_id']}.png"
        with Image.open(source_page) as source:
            source = source.convert("RGB")
        comparison_height = 70 + 52 * len(manual_fields)
        canvas = Image.new(
            "RGB", (source.width, source.height + comparison_height), "#fff7ed"
        )
        canvas.paste(source, (0, comparison_height))
        draw = ImageDraw.Draw(canvas)
        draw.text(
            (28, 14),
            "第二阶段：将下列产品预测与下方原件的人工读取结果逐项对照",
            fill="#9a3412",
            font=title_font,
        )
        if parsed is None:
            missing_parse += 1
        for index, field in enumerate(manual_fields):
            value = "<解析失败或字段缺失>" if parsed is None else str(parsed.get(field) or "<字段缺失>")
            draw.text(
                (55, 62 + index * 52),
                f"{FIELD_LABELS[field]}：{value}",
                fill="#111827",
                font=value_font,
            )
        destination = pages_root / source_page.name
        canvas.save(destination, "PNG", optimize=True)
        rendered += 1
        index_rows.append(
            {
                "blind_id": row["blind_id"],
                "sha256": row["sha256"],
                "manual_fields": ",".join(manual_fields),
                "comparison_page": destination.name,
                "parse_available": str(parsed is not None).lower(),
            }
        )

    with (output_root / "comparison-index.private.tsv").open(
        "w", encoding="utf-8", newline=""
    ) as stream:
        writer = csv.DictWriter(stream, fieldnames=list(index_rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(index_rows)

    print("verification=private-independent-field-prediction-comparison-v1")
    print(f"comparison_invoices={len(manual_rows)}")
    print(f"rendered={rendered}")
    print(f"missing_parse={missing_parse}")
    print("comparison_values_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
