from __future__ import annotations

import argparse
import csv
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render prediction-blind private review sheets.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("--pdftoppm", required=True, type=Path)
    return parser.parse_args()


def render_pdf(source: Path, destination: Path, pdftoppm: Path) -> bool:
    prefix = destination.with_suffix("")
    completed = subprocess.run(
        [
            str(pdftoppm),
            "-f",
            "1",
            "-l",
            "1",
            "-scale-to",
            "1800",
            "-png",
            "-singlefile",
            str(source),
            str(prefix),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return completed.returncode == 0 and destination.is_file()


def render_image(source: Path, destination: Path) -> bool:
    try:
        with Image.open(source) as image:
            ImageOps.exif_transpose(image).convert("RGB").save(destination, "PNG")
        return True
    except Exception:
        return False


def make_placeholder(blind_id: str, destination: Path) -> None:
    image = Image.new("RGB", (1200, 800), "#f6f7f9")
    draw = ImageDraw.Draw(image)
    draw.rectangle((20, 20, 1180, 780), outline="#b91c1c", width=6)
    draw.text((60, 80), blind_id, fill="#111827", font=ImageFont.load_default())
    draw.text(
        (60, 150),
        "Independent renderer could not open this file",
        fill="#b91c1c",
        font=ImageFont.load_default(),
    )
    image.save(destination, "PNG")


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")
    pdftoppm = args.pdftoppm.resolve(strict=True)
    if pdftoppm.name.lower() != "pdftoppm.exe":
        raise ValueError("--pdftoppm must identify the bundled pdftoppm.exe")

    prelabel_path = capture_root / "ground-truth-prelabel.private.tsv"
    expanded_root = capture_root / "expanded-attachments"
    with prelabel_path.open("r", encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    review_rows = [
        row
        for row in rows
        if row["suggested_label"] in {"needs_visual_review", "corrupt_or_empty"}
    ]
    if not review_rows:
        raise ValueError("private prelabel contains no visual-review rows")

    review_root = capture_root / "ground-truth-review-private"
    pages_root = review_root / "pages"
    pages_root.mkdir(parents=True, exist_ok=False)
    rendered: list[tuple[str, Path, str]] = []
    for row in review_rows:
        blind_id = row["blind_id"]
        source = expanded_root / row["file"]
        destination = pages_root / f"{blind_id}.png"
        extension = row["extension"].lower()
        ok = (
            render_pdf(source, destination, pdftoppm)
            if extension == "pdf"
            else render_image(source, destination)
            if extension in {"jpg", "jpeg", "png", "webp", "bmp"}
            else False
        )
        if not ok:
            make_placeholder(blind_id, destination)
        rendered.append((blind_id, destination, row["suggested_label"]))

    tile_width = 900
    tile_height = 1120
    columns = 2
    rows_per_sheet = 2
    font = ImageFont.load_default()
    sheet_count = 0
    for offset in range(0, len(rendered), columns * rows_per_sheet):
        sheet_count += 1
        sheet = Image.new(
            "RGB",
            (tile_width * columns, tile_height * rows_per_sheet),
            "#e5e7eb",
        )
        for position, (blind_id, page_path, suggested_label) in enumerate(
            rendered[offset : offset + columns * rows_per_sheet]
        ):
            with Image.open(page_path) as page:
                page = page.convert("RGB")
                page.thumbnail((tile_width - 40, tile_height - 90), Image.Resampling.LANCZOS)
                tile = Image.new("RGB", (tile_width - 20, tile_height - 20), "white")
                x = (tile.width - page.width) // 2
                y = 58 + (tile.height - 70 - page.height) // 2
                tile.paste(page, (x, y))
                draw = ImageDraw.Draw(tile)
                draw.text((18, 16), blind_id, fill="#111827", font=font)
                draw.text((100, 16), suggested_label, fill="#4b5563", font=font)
                column = position % columns
                row_index = position // columns
                sheet.paste(tile, (column * tile_width + 10, row_index * tile_height + 10))
        sheet.save(review_root / f"review-sheet-{sheet_count:02}.png", "PNG")

    print("verification=prediction-blind-private-render-v1")
    print(f"review_files={len(rendered)}")
    print(f"review_sheets={sheet_count}")
    print("product_predictions_read=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
