from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

import pdfplumber
from pypdf import PdfReader


RAW_MARKERS = {
    "metadata": b"/Metadata",
    "embedded_files": b"/EmbeddedFiles",
    "associated_files": b"/AF",
    "xbrl": b"xbrl",
    "invoice_number_tag": b"InvoiceNumber",
    "rail_number_tag": b"ElectronicInvoiceRailwayETicketNumber",
    "date_of_issue_tag": b"DateOfIssue",
    "tax_inclusive_total_tag": b"TaxInclusiveTotalAmount",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Diagnose private valid-PDF parse failures.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-pdf-failure-families.validation.json"),
    )
    return parser.parse_args()


def safe_count_text(extractor) -> tuple[bool, int]:
    try:
        text = extractor()
        return True, len(re.sub(r"\s+", "", text or ""))
    except Exception:
        return False, 0


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
        raise ValueError("aggregate evidence must be under repository artifacts")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth_rows = list(csv.DictReader(stream, delimiter="\t"))
    valid_by_hash: dict[str, dict[str, str]] = {}
    for row in truth_rows:
        if row["human_label"] == "valid_invoice":
            valid_by_hash.setdefault(row["sha256"], row)

    with (capture_root / "reclassified.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        predictions = {
            row["sha256"]: row for row in csv.DictReader(stream, delimiter="\t")
        }
    results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    samples_root = parse_root / "fixtures" / "samples"

    rows: list[dict[str, object]] = []
    for result in results:
        sample_path = samples_root / result["sample"]
        digest = hashlib.sha256(sample_path.read_bytes()).hexdigest()
        truth = valid_by_hash.get(digest)
        if truth is None or truth["extension"] != "pdf":
            continue
        raw = sample_path.read_bytes()
        state = "parsed" if result.get("parsed") is not None else "failed"
        predicted_format = predictions[digest]["predicted_format"]

        page_count = 0
        encrypted = False
        pypdf_ok = False
        pypdf_chars = 0
        annotation_count = 0
        image_count = 0
        try:
            reader = PdfReader(sample_path, strict=False)
            encrypted = bool(reader.is_encrypted)
            page_count = len(reader.pages)
            pypdf_ok, pypdf_chars = safe_count_text(
                lambda: "\n".join(page.extract_text() or "" for page in reader.pages)
            )
            for page in reader.pages:
                annotations = page.get("/Annots")
                annotation_count += len(annotations) if annotations else 0
                try:
                    image_count += len(page.images)
                except Exception:
                    pass
        except Exception:
            pass

        pdfplumber_ok = False
        pdfplumber_chars = 0
        try:
            with pdfplumber.open(sample_path) as pdf:
                pdfplumber_ok, pdfplumber_chars = safe_count_text(
                    lambda: "\n".join(page.extract_text() or "" for page in pdf.pages)
                )
        except Exception:
            pass

        row: dict[str, object] = {
            "blind_id": truth["blind_id"],
            "sha256": digest,
            "state": state,
            "predicted_format": predicted_format,
            "byte_len": len(raw),
            "page_count": page_count,
            "encrypted": encrypted,
            "pypdf_ok": pypdf_ok,
            "pypdf_chars": pypdf_chars,
            "pdfplumber_ok": pdfplumber_ok,
            "pdfplumber_chars": pdfplumber_chars,
            "annotation_count": annotation_count,
            "image_count": image_count,
        }
        for name, marker in RAW_MARKERS.items():
            row[f"raw_{name}"] = marker.lower() in raw.lower()
        rows.append(row)

    private_output = capture_root / "pdf-failure-diagnostics.private.tsv"
    with private_output.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    groups: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        key = f"{row['predicted_format']}:{row['state']}"
        group = groups[key]
        group["files"] += 1
        group["pypdf_open"] += int(bool(row["pypdf_ok"]))
        group["pdfplumber_open"] += int(bool(row["pdfplumber_ok"]))
        group["has_annotations"] += int(int(row["annotation_count"]) > 0)
        group["has_images"] += int(int(row["image_count"]) > 0)
        for name in RAW_MARKERS:
            group[f"raw_{name}"] += int(bool(row[f"raw_{name}"]))

    report = {
        "verification": "real-qq-private-pdf-failure-families-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "valid_pdf_files": len(rows),
        "valid_pdf_parsed": sum(row["state"] == "parsed" for row in rows),
        "valid_pdf_failed": sum(row["state"] == "failed" for row in rows),
        "groups": {key: dict(value) for key, value in sorted(groups.items())},
        "private_identifiers_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-pdf-failure-families-v1")
    print(f"valid_pdf_files={report['valid_pdf_files']}")
    print(f"valid_pdf_parsed={report['valid_pdf_parsed']}")
    print(f"valid_pdf_failed={report['valid_pdf_failed']}")
    for key, counts in sorted(groups.items()):
        print(f"group={key} counts={json.dumps(dict(counts), sort_keys=True)}")
    print("private_identifiers_logged=false")
    print(f"evidence={evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
