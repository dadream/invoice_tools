from __future__ import annotations

import argparse
import csv
import hashlib
import re
import sys
import zipfile
from dataclasses import dataclass
from pathlib import Path
from xml.etree import ElementTree

import pdfplumber
from PIL import Image
from pypdf import PdfReader


VALID_PATTERNS = (
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"电子发票",
        r"增值税.{0,12}发票",
        r"数电.{0,8}发票",
        r"发票号码",
        r"价税合计",
        r"购买方.{0,12}销售方",
        r"铁路电子客票",
        r"航空运输电子客票行程单",
        r"电子客票行程单",
        r"报销凭证",
        r"invoice\s*(number|no\.)",
    )
)
VALID_PATTERNS = tuple(VALID_PATTERNS)

SUPPORTING_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"费用明细",
        r"行程明细",
        r"订单明细",
        r"结算单",
        r"对账单",
        r"行程确认单",
        r"transaction\s+details",
    )
)


@dataclass
class Inspection:
    structural_valid: bool
    text_chars: int
    suggested_label: str
    reason: str


def compact_text(value: str) -> str:
    return re.sub(r"\s+", "", value or "")


def classify_text(text: str, structural_valid: bool, extension: str) -> Inspection:
    compact = compact_text(text)
    if not structural_valid:
        return Inspection(False, len(compact), "corrupt_or_empty", "independent_open_failed")
    if any(pattern.search(compact) for pattern in VALID_PATTERNS):
        return Inspection(True, len(compact), "valid_invoice", "strong_invoice_terms")
    if any(pattern.search(compact) for pattern in SUPPORTING_PATTERNS):
        return Inspection(True, len(compact), "supporting_document", "supporting_terms")
    if extension in {"jpg", "jpeg", "png", "webp", "bmp"} or len(compact) < 50:
        return Inspection(True, len(compact), "needs_visual_review", "insufficient_independent_text")
    return Inspection(True, len(compact), "needs_visual_review", "no_decisive_invoice_terms")


def inspect_pdf(path: Path) -> Inspection:
    text_parts: list[str] = []
    try:
        with pdfplumber.open(path) as pdf:
            if not pdf.pages:
                return Inspection(False, 0, "corrupt_or_empty", "pdf_has_no_pages")
            for page in pdf.pages:
                text_parts.append(page.extract_text() or "")
    except Exception:
        try:
            reader = PdfReader(path, strict=False)
            if not reader.pages:
                return Inspection(False, 0, "corrupt_or_empty", "pdf_has_no_pages")
            for page in reader.pages:
                try:
                    text_parts.append(page.extract_text() or "")
                except Exception:
                    text_parts.append("")
        except Exception:
            return Inspection(False, 0, "corrupt_or_empty", "independent_pdf_open_failed")
    return classify_text("\n".join(text_parts), True, "pdf")


def inspect_ofd(path: Path) -> Inspection:
    text_parts: list[str] = []
    try:
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
            if sum(1 for name in names if name.lower() == "ofd.xml") != 1:
                return Inspection(False, 0, "corrupt_or_empty", "ofd_root_missing")
            for name in names:
                if not name.lower().endswith(".xml"):
                    continue
                payload = archive.read(name)
                root = ElementTree.fromstring(payload)
                text_parts.extend(value for value in root.itertext() if value)
    except Exception:
        return Inspection(False, 0, "corrupt_or_empty", "independent_ofd_open_failed")
    return classify_text("\n".join(text_parts), True, "ofd")


def inspect_xml(path: Path) -> Inspection:
    try:
        root = ElementTree.parse(path).getroot()
        text = "\n".join(value for value in root.itertext() if value)
    except Exception:
        return Inspection(False, 0, "corrupt_or_empty", "independent_xml_open_failed")
    return classify_text(text, True, "xml")


def inspect_image(path: Path, extension: str) -> Inspection:
    try:
        with Image.open(path) as image:
            image.verify()
    except Exception:
        return Inspection(False, 0, "corrupt_or_empty", "independent_image_open_failed")
    return classify_text("", True, extension)


def inspect_file(path: Path) -> Inspection:
    extension = path.suffix.lower().lstrip(".")
    if path.stat().st_size == 0:
        return Inspection(False, 0, "corrupt_or_empty", "zero_bytes")
    if extension == "pdf":
        return inspect_pdf(path)
    if extension == "ofd":
        return inspect_ofd(path)
    if extension == "xml":
        return inspect_xml(path)
    if extension in {"jpg", "jpeg", "png", "webp", "bmp"}:
        return inspect_image(path, extension)
    return Inspection(True, 0, "needs_visual_review", "unsupported_for_independent_text")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a prediction-blind private ground-truth prelabel inventory."
    )
    parser.add_argument("capture_root", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")
    if capture_root.is_symlink():
        raise ValueError("private capture root must not be a symlink")

    expanded_root = capture_root / "expanded-attachments"
    emails_root = capture_root / "emails"
    if not expanded_root.is_dir() or not emails_root.is_dir():
        raise ValueError("capture root does not contain the expected private directories")

    candidates: list[tuple[str, Path]] = []
    for path in expanded_root.iterdir():
        if not path.is_file() or path.is_symlink():
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        candidates.append((digest, path))
    candidates.sort(key=lambda item: (item[0], item[1].name))
    if not candidates:
        raise ValueError("private capture contains no expanded attachments")

    rows: list[dict[str, str | int | bool]] = []
    counts: dict[str, int] = {}
    email_pattern = re.compile(r"^email-(\d{3})-")
    for index, (digest, path) in enumerate(candidates, start=1):
        match = email_pattern.match(path.name)
        email_file = ""
        if match:
            matching_emails = sorted(emails_root.glob(f"email-{match.group(1)}-*.eml"))
            if len(matching_emails) == 1:
                email_file = matching_emails[0].name
        inspection = inspect_file(path)
        counts[inspection.suggested_label] = counts.get(inspection.suggested_label, 0) + 1
        rows.append(
            {
                "blind_id": f"B{index:03}",
                "file": path.name,
                "email_file": email_file,
                "extension": path.suffix.lower().lstrip("."),
                "byte_len": path.stat().st_size,
                "sha256": digest,
                "structural_valid": str(inspection.structural_valid).lower(),
                "text_chars": inspection.text_chars,
                "suggested_label": inspection.suggested_label,
                "suggestion_reason": inspection.reason,
                "human_label": "",
                "human_notes": "",
            }
        )

    output_path = capture_root / "ground-truth-prelabel.private.tsv"
    with output_path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    print("verification=prediction-blind-private-prelabel-v1")
    print(f"attachments={len(rows)}")
    for label in sorted(counts):
        print(f"suggested_{label}={counts[label]}")
    print("product_predictions_read=false")
    print("private_text_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
