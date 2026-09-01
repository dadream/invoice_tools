from __future__ import annotations

import argparse
import csv
import hashlib
import sys
import textwrap
import zipfile
from pathlib import Path
from xml.etree import ElementTree

import pypdfium2 as pdfium
from PIL import Image, ImageDraw, ImageFont, ImageOps


FIELDS = ("invoice_number", "issue_date", "total_amount")
FIELD_LABELS = {
    "invoice_number": "发票号码",
    "issue_date": "开票日期",
    "total_amount": "价税合计",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render product-output-blind private core-field review pages."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("--font", required=True, type=Path)
    parser.add_argument(
        "--output-name", default="field-ground-truth-review-private-v1"
    )
    return parser.parse_args()


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1]


def add_header(
    image: Image.Image,
    blind_id: str,
    manual_fields: list[str],
    font_path: Path,
) -> Image.Image:
    image = image.convert("RGB")
    header_height = 120
    canvas = Image.new("RGB", (image.width, image.height + header_height), "white")
    canvas.paste(image, (0, header_height))
    draw = ImageDraw.Draw(canvas)
    title_font = ImageFont.truetype(str(font_path), 34)
    body_font = ImageFont.truetype(str(font_path), 25)
    draw.text((28, 16), blind_id, fill="#111827", font=title_font)
    labels = "、".join(FIELD_LABELS[field] for field in manual_fields)
    draw.text(
        (28, 66),
        f"仅人工核对：{labels}（不显示产品预测值）",
        fill="#991b1b",
        font=body_font,
    )
    return canvas


def render_pdf(path: Path) -> Image.Image:
    document = pdfium.PdfDocument(str(path))
    try:
        if len(document) == 0:
            raise ValueError("PDF has no pages")
        page = document[0]
        try:
            bitmap = page.render(scale=3.0)
            try:
                return bitmap.to_pil().convert("RGB")
            finally:
                bitmap.close()
        finally:
            page.close()
    finally:
        document.close()


def parse_boundary(raw: str) -> tuple[float, float, float, float] | None:
    try:
        values = [float(value) for value in raw.split()]
    except ValueError:
        return None
    return tuple(values) if len(values) == 4 else None


def render_ofd(path: Path, font_path: Path) -> Image.Image:
    boxes: list[tuple[float, float, float, float, str]] = []
    with zipfile.ZipFile(path) as archive:
        names = [
            name
            for name in archive.namelist()
            if name.lower().endswith("content.xml")
            and ("/pages/" in name.lower() or "/tpls/" in name.lower())
        ]
        for name in names:
            root = ElementTree.fromstring(archive.read(name))
            for element in root.iter():
                if local_name(str(element.tag)) != "TextObject":
                    continue
                boundary_raw = next(
                    (
                        value
                        for key, value in element.attrib.items()
                        if local_name(str(key)) == "Boundary"
                    ),
                    "",
                )
                boundary = parse_boundary(boundary_raw)
                text = "".join(
                    "".join(child.itertext())
                    for child in element.iter()
                    if local_name(str(child.tag)) == "TextCode"
                ).strip()
                if boundary and text:
                    boxes.append((*boundary, text))
    if not boxes:
        raise ValueError("OFD contains no positioned text")

    width_mm = max(210.0, max(x + width for x, _, width, _, _ in boxes) + 10.0)
    height_mm = max(140.0, max(y + height for _, y, _, height, _ in boxes) + 10.0)
    scale = min(9.0, 2400.0 / width_mm)
    image = Image.new(
        "RGB", (round(width_mm * scale), round(height_mm * scale)), "white"
    )
    draw = ImageDraw.Draw(image)
    font_cache: dict[int, ImageFont.FreeTypeFont] = {}
    for x, y, _, height, text in sorted(boxes, key=lambda item: (item[1], item[0])):
        font_size = max(13, min(42, round(height * scale * 0.78)))
        font = font_cache.setdefault(
            font_size, ImageFont.truetype(str(font_path), font_size)
        )
        draw.text((round(x * scale), round(y * scale)), text, fill="black", font=font)
    return image


def render_xml(path: Path, font_path: Path) -> Image.Image:
    root = ElementTree.parse(path).getroot()
    lines: list[str] = []
    for element in root.iter():
        value = (element.text or "").strip()
        if not value or list(element):
            continue
        label = local_name(str(element.tag))
        lines.extend(textwrap.wrap(f"{label}: {value}", width=72) or [""])
    font = ImageFont.truetype(str(font_path), 27)
    line_height = 42
    image = Image.new("RGB", (1800, max(1200, 80 + line_height * len(lines))), "white")
    draw = ImageDraw.Draw(image)
    y = 35
    for line in lines:
        draw.text((45, y), line, fill="#111827", font=font)
        y += line_height
    return image


def render_source(path: Path, extension: str, font_path: Path) -> Image.Image:
    if extension == "pdf":
        return render_pdf(path)
    if extension == "ofd":
        return render_ofd(path, font_path)
    if extension == "xml":
        return render_xml(path, font_path)
    if extension in {"png", "jpg", "jpeg", "webp", "bmp"}:
        with Image.open(path) as source:
            return ImageOps.exif_transpose(source).convert("RGB")
    raise ValueError(f"unsupported private review extension: {extension}")


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    font_path = args.font.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")
    if font_path.suffix.lower() not in {".ttf", ".otf"}:
        raise ValueError("--font must be a local TTF or OTF file")

    prelabel_path = capture_root / "field-ground-truth-prelabel.private.tsv"
    with prelabel_path.open("r", encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    review_rows = [
        row
        for row in rows
        if any(row[f"{field}_status"] == "needs_manual_review" for field in FIELDS)
    ]
    output_root = capture_root / args.output_name
    output_root.mkdir(parents=True, exist_ok=False)
    pages_root = output_root / "pages"
    pages_root.mkdir()
    expanded_root = capture_root / "expanded-attachments"

    index_rows: list[dict[str, str]] = []
    rendered = 0
    failures = 0
    for row in review_rows:
        source = expanded_root / row["file"]
        if hashlib.sha256(source.read_bytes()).hexdigest() != row["sha256"]:
            raise ValueError("private attachment hash changed")
        manual_fields = [
            field
            for field in FIELDS
            if row[f"{field}_status"] == "needs_manual_review"
        ]
        destination = pages_root / f"{row['blind_id']}.png"
        try:
            image = render_source(source, row["extension"].lower(), font_path)
            image = add_header(image, row["blind_id"], manual_fields, font_path)
            image.save(destination, "PNG", optimize=True)
            status = "rendered"
            rendered += 1
        except Exception:
            placeholder = Image.new("RGB", (1600, 1000), "white")
            draw = ImageDraw.Draw(placeholder)
            font = ImageFont.truetype(str(font_path), 32)
            draw.text((50, 50), row["blind_id"], fill="#111827", font=font)
            draw.text(
                (50, 120),
                "独立渲染失败，必须使用受控原件查看器人工核对",
                fill="#991b1b",
                font=font,
            )
            placeholder.save(destination, "PNG")
            status = "render_failed"
            failures += 1
        index_rows.append(
            {
                "blind_id": row["blind_id"],
                "sha256": row["sha256"],
                "extension": row["extension"],
                "review_page": destination.name,
                "manual_fields": ",".join(manual_fields),
                "render_status": status,
            }
        )

    with (output_root / "review-index.private.tsv").open(
        "w", encoding="utf-8", newline=""
    ) as stream:
        writer = csv.DictWriter(stream, fieldnames=list(index_rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(index_rows)

    print("verification=product-output-blind-private-field-render-v1")
    print(f"review_invoices={len(review_rows)}")
    print(f"rendered={rendered}")
    print(f"render_failures={failures}")
    print("product_parse_results_read=false")
    print("private_fields_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
