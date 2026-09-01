from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path

import pdfplumber
from PIL import Image
from pypdf import PdfReader


LABELS = (
    "电子发票",
    "增值税",
    "普通发票",
    "专用发票",
    "发票号码",
    "开票日期",
    "价税合计",
    "小写",
    "金额",
    "税额",
    "购买方",
    "销售方",
)
SAFE_LINE_PHRASES = tuple(
    sorted(
        set(LABELS)
        | {
            "合计",
            "总金额",
            "含税金额",
            "不含税金额",
            "税前金额",
            "实付金额",
            "支付金额",
            "优惠金额",
            "发票日期",
            "开具日期",
            "交易日期",
            "消费日期",
            "服务日期",
            "行程日期",
            "校验码",
            "发票代码",
            "项目名称",
            "规格型号",
            "单位",
            "数量",
            "单价",
            "税率",
            "备注",
            "收款人",
            "复核人",
            "开票人",
            "统一社会信用代码",
            "纳税人识别号",
            "地址",
            "电话",
        },
        key=len,
        reverse=True,
    )
)
SAFE_LATIN_PHRASES = tuple(
    sorted(
        {
            "electronic invoice",
            "invoice number",
            "invoice date",
            "issue date",
            "total amount",
            "tax amount",
            "amount",
            "currency",
            "description",
            "invoice",
            "date",
            "buyer",
            "seller",
            "service",
            "trip",
            "ride",
        },
        key=len,
        reverse=True,
    )
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Diagnose aggregate structure of remaining private valid parse failures."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("--parse-result-overlay", type=Path, action="append", default=[])
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-remaining-valid-failures.validation.json"),
    )
    return parser.parse_args()


def pdf_font_profile(reader: PdfReader) -> dict[str, object]:
    subtypes: Counter[str] = Counter()
    encodings: Counter[str] = Counter()
    fonts = 0
    to_unicode = 0
    for page in reader.pages:
        resources = page.get("/Resources") or {}
        font_dictionary = resources.get("/Font") or {}
        try:
            font_dictionary = font_dictionary.get_object()
        except AttributeError:
            pass
        for font_reference in font_dictionary.values():
            try:
                font = font_reference.get_object()
            except AttributeError:
                font = font_reference
            fonts += 1
            subtypes[str(font.get("/Subtype", "none"))] += 1
            encoding = font.get("/Encoding", "none")
            if hasattr(encoding, "get_object"):
                encoding = encoding.get_object()
            if isinstance(encoding, dict):
                encoding = encoding.get("/BaseEncoding", "dictionary")
            encodings[str(encoding)] += 1
            if font.get("/ToUnicode") is not None:
                to_unicode += 1
    return {
        "font_resources": fonts,
        "font_subtypes": dict(sorted(subtypes.items())),
        "font_encodings": dict(sorted(encodings.items())),
        "fonts_with_to_unicode": to_unicode,
    }


def text_profile(text: str) -> dict[str, object]:
    compact = "".join(character for character in text if not character.isspace())
    return {
        "non_whitespace_chars": len(compact),
        "line_count": len([line for line in text.splitlines() if line.strip()]),
        "label_presence": {label: label in text for label in LABELS},
        "long_digit_pattern_count": len(re.findall(r"(?<!\d)\d{10,24}(?!\d)", text)),
        "date_pattern_count": len(
            re.findall(r"20\d{2}[-/.年]\d{1,2}[-/.月]\d{1,2}日?", text)
        ),
        "currency_amount_pattern_count": len(
            re.findall(r"[￥¥]\s*\d[\d,]*\.\d{2}", text)
        ),
        "decimal_pattern_count": len(re.findall(r"(?<!\d)\d[\d,]*\.\d{2}(?!\d)", text)),
        "han_character_count": len(re.findall(r"[\u3400-\u9fff]", text)),
    }


def redacted_line_shapes(text: str) -> list[str]:
    shapes = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        replacements: dict[str, str] = {}
        for index, phrase in enumerate(SAFE_LINE_PHRASES):
            marker = chr(0xE000 + index)
            if phrase in line:
                line = line.replace(phrase, marker)
                replacements[marker] = phrase
        marker_index = len(SAFE_LINE_PHRASES)
        for phrase in SAFE_LATIN_PHRASES:
            pattern = re.compile(re.escape(phrase), re.IGNORECASE)
            if pattern.search(line):
                marker = chr(0xE000 + marker_index)
                marker_index += 1
                original = pattern.search(line).group(0)
                line = pattern.sub(marker, line)
                replacements[marker] = original
        line = re.sub(r"[\u3400-\u9fff]+", "\uE100", line)
        line = re.sub(r"\d+(?:[.,:/-]\d+)*", "\uE101", line)
        line = re.sub(r"[A-Za-z]+", "\uE102", line)
        line = re.sub(r"\s+", " ", line)
        for marker, phrase in replacements.items():
            line = line.replace(marker, phrase)
        line = line.replace("\uE100", "<HAN>")
        line = line.replace("\uE101", "<NUM>")
        line = line.replace("\uE102", "<LATIN>")
        shapes.append(line)
    return shapes


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    evidence = args.evidence
    if not evidence.is_absolute():
        evidence = (repo_root / evidence).resolve()
    artifacts_root = (repo_root / "artifacts").resolve()
    if evidence == artifacts_root or artifacts_root not in evidence.parents:
        raise ValueError("aggregate evidence must be written under repository artifacts")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth = {}
        for row in csv.DictReader(stream, delimiter="\t"):
            truth.setdefault(row["sha256"], row)

    results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    by_sample = {result["sample"]: result for result in results}
    overlays = []
    for overlay_arg in args.parse_result_overlay:
        overlay = overlay_arg
        if not overlay.is_absolute():
            overlay = (parse_root / overlay).resolve(strict=True)
        else:
            overlay = overlay.resolve(strict=True)
        if overlay == parse_root or parse_root not in overlay.parents:
            raise ValueError("overlays must remain in the private parse root")
        for result in json.loads(overlay.read_text("utf-8")):
            if result["sample"] not in by_sample:
                raise ValueError("overlay contains unknown sample")
            by_sample[result["sample"]] = result
        overlays.append(overlay.name)

    samples_root = parse_root / "fixtures" / "samples"
    cases = []
    for sample, result in sorted(by_sample.items()):
        sample_path = samples_root / sample
        digest = hashlib.sha256(sample_path.read_bytes()).hexdigest()
        row = truth.get(digest)
        if row is None or row["human_label"] != "valid_invoice" or result.get("parsed") is not None:
            continue
        extension = row["extension"]
        case: dict[str, object] = {"extension": extension}
        if extension == "pdf":
            reader = PdfReader(sample_path, strict=False)
            pypdf_text = "\n".join(page.extract_text() or "" for page in reader.pages)
            with pdfplumber.open(sample_path) as document:
                plumber_text = "\n".join(page.extract_text() or "" for page in document.pages)
                plumber_images = sum(len(page.images) for page in document.pages)
            case.update(
                {
                    "pages": len(reader.pages),
                    "encrypted": reader.is_encrypted,
                    "attachments": sum(len(payloads) for payloads in reader.attachments.values()),
                    "page_images": plumber_images,
                    "metadata_present": reader.metadata is not None,
                    "xmp_metadata_present": reader.xmp_metadata is not None,
                    "acroform_present": bool(reader.trailer["/Root"].get("/AcroForm")),
                    "pypdf_text": text_profile(pypdf_text),
                    "pdfplumber_text": text_profile(plumber_text),
                    "pypdf_redacted_line_shapes": redacted_line_shapes(pypdf_text),
                    "fonts": pdf_font_profile(reader),
                }
            )
        elif extension in {"png", "jpg", "jpeg", "webp", "bmp"}:
            with Image.open(sample_path) as image:
                grayscale = image.convert("L")
                extrema = grayscale.getextrema()
                case.update(
                    {
                        "format": image.format,
                        "width": image.width,
                        "height": image.height,
                        "mode": image.mode,
                        "grayscale_min": extrema[0],
                        "grayscale_max": extrema[1],
                    }
                )
        cases.append(case)

    counts = Counter(case["extension"] for case in cases)
    report = {
        "verification": "real-qq-private-remaining-valid-failures-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "parse_result_overlays": overlays,
        "remaining_valid_failures": len(cases),
        "by_extension": dict(sorted(counts.items())),
        "cases": [dict(case_id=f"case-{index + 1}", **case) for index, case in enumerate(cases)],
        "private_names_hashes_and_field_values_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-remaining-valid-failures-v1")
    print(f"remaining_valid_failures={len(cases)}")
    print("by_extension=" + json.dumps(dict(sorted(counts.items())), ensure_ascii=False))
    print("private_values_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
