from __future__ import annotations

import argparse
import csv
import sys
import textwrap
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

from prepare_private_email_ground_truth import visible_message_text


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render private blind email review sheets.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("--font", required=True, type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")
    font_path = args.font.resolve(strict=True)

    with (capture_root / "email-ground-truth-prelabel.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        rows = [
            row
            for row in csv.DictReader(stream, delimiter="\t")
            if row["requires_visual_review"] == "true"
        ]
    review_root = capture_root / "email-ground-truth-review-private"
    review_root.mkdir(parents=True, exist_ok=False)
    font = ImageFont.truetype(str(font_path), 28)
    header_font = ImageFont.truetype(str(font_path), 38)
    for row in rows:
        subject, body = visible_message_text(capture_root / "emails" / row["email_file"])
        text = f"主题：{subject}\n\n正文：{body[:5000]}"
        lines: list[str] = []
        for paragraph in text.splitlines() or [""]:
            lines.extend(textwrap.wrap(paragraph, width=62, replace_whitespace=False) or [""])
        image = Image.new("RGB", (1800, 2200), "white")
        draw = ImageDraw.Draw(image)
        draw.text((60, 45), row["blind_email_id"], fill="#111827", font=header_font)
        draw.text(
            (250, 55),
            row["suggestion_reason"],
            fill="#4b5563",
            font=font,
        )
        y = 130
        for line in lines:
            if y > 2140:
                draw.text((60, y - 10), "…（正文已截断）", fill="#b91c1c", font=font)
                break
            draw.text((60, y), line, fill="#111827", font=font)
            y += 42
        image.save(review_root / f"{row['blind_email_id']}.png", "PNG")

    print("verification=product-output-blind-private-email-render-v1")
    print(f"review_files={len(rows)}")
    print("product_predictions_read=false")
    print("private_message_text_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
