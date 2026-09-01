from __future__ import annotations

import argparse
import csv
import hashlib
import os
import re
import shutil
import subprocess
import sys
import zipfile
from dataclasses import dataclass
from datetime import date
from decimal import Decimal, InvalidOperation
from pathlib import Path
from xml.etree import ElementTree

import pdfplumber
from pypdf import PdfReader


NUMBER_TAGS = {
    "fphm",
    "invoicenumber",
    "invoiceno",
    "invoicenum",
    "electronicinvoicerailwayeticketnumber",
}
DATE_TAGS = {
    "kprq",
    "issuedate",
    "issuetime",
    "dateofissue",
    "invoicedate",
    "billingdate",
}
AMOUNT_TAGS = {
    "fare",
    "jshj",
    "totalamount",
    "amountwithtax",
    "amountinfigures",
    "invoiceamount",
    "totalamountwithtax",
    "totalamountincludingtax",
    "invoicetotalamount",
    "totaltaxincludedamount",
    "totaltax-includedamount",
    "taxinclusivetotalamount",
}
AUTHORITATIVE_AMOUNT_TAGS = {
    "fare",
    "jshj",
    "totalamount",
    "amountwithtax",
    "amountinfigures",
    "invoiceamount",
    "totalamountwithtax",
    "totalamountincludingtax",
    "invoicetotalamount",
    "totaltax-includedamount",
    "taxinclusivetotalamount",
}

NUMBER_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE | re.DOTALL)
    for pattern in (
        r"(?:发票号码|发票号|票据号码)\s*[:：]?\s*([0-9][0-9\s-]{7,28})",
        r"(?:电子客票号|客票号)\s*[:：]?\s*([0-9][0-9\s-]{7,28})",
        r"(?:invoice|ticket)\s*(?:number|no\.?|nbr)\s*[:：]?\s*([0-9][0-9\s-]{7,28})",
    )
)
DATE_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE | re.DOTALL)
    for pattern in (
        r"(?:开票日期|填开日期|开具日期|开具时间)\s*[:：]?\s*"
        r"(20\d{2}[年./-]\s*\d{1,2}[月./-]\s*\d{1,2}日?)",
        r"(?:开票日期|填开日期|开具日期|开具时间)\s*[:：]?\s*(20\d{6})",
        r"(?:issue\s*date|invoice\s*date)\s*[:：]?\s*"
        r"(20\d{2}[./-]\d{1,2}[./-]\d{1,2})",
    )
)
AMOUNT_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE | re.DOTALL)
    for pattern in (
        r"(?:小写)\s*[:：]?\s*[¥￥]?\s*([0-9][0-9,]*\.\d{2})",
        r"[（(]\s*小写\s*[）)]\s*[:：]?\s*[¥￥]?\s*([0-9][0-9,]*\.\d{2})",
        r"(?:价税合计)[\s\S]{0,80}?(?:小写\s*[）)]?)?\s*[:：]?\s*[¥￥]?\s*"
        r"([0-9][0-9,]*\.\d{2})",
        r"(?:价税合计)\s*(?:\([^)]{0,12}\)|（[^）]{0,12}）|[:：]|\s){0,6}"
        r"[¥￥]\s*([0-9][0-9,]*\.\d{2})",
        r"(?:票价|合计金额|总额)\s*[:：]?\s*[¥￥]?\s*([0-9][0-9,]*\.\d{2})",
        r"(?:应付金额|实付金额|含税金额)\s*[:：]?\s*[¥￥]?\s*"
        r"([0-9][0-9,]*\.\d{2})",
        r"(?:total\s*amount|invoice\s*total)\s*[:：]?\s*[A-Z¥￥]*\s*"
        r"([0-9][0-9,]*\.\d{2})",
    )
)


@dataclass(frozen=True)
class FieldCandidate:
    value: str
    method: str


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1].lower()


def normalize_number(raw: str) -> str | None:
    digits = re.sub(r"\D", "", raw)
    return digits if 8 <= len(digits) <= 24 else None


def normalize_date(raw: str) -> str | None:
    match = re.match(
        r"^\s*(20\d{2})\D?(\d{1,2})\D?(\d{1,2})(?:\D|$)", raw
    )
    if not match:
        return None
    try:
        return date(int(match.group(1)), int(match.group(2)), int(match.group(3))).isoformat()
    except ValueError:
        return None


def normalize_amount(raw: str) -> str | None:
    cleaned = raw.replace(",", "").replace("¥", "").replace("￥", "").strip()
    try:
        value = Decimal(cleaned)
    except InvalidOperation:
        return None
    if value < 0 or value > Decimal("999999999.99"):
        return None
    return f"{value:.2f}"


def unique_matches(
    text: str,
    patterns: tuple[re.Pattern[str], ...],
    normalizer,
) -> set[str]:
    values: set[str] = set()
    for pattern in patterns:
        for match in pattern.finditer(text):
            value = normalizer(match.group(1))
            if value is not None:
                values.add(value)
    return values


def candidates_from_text(text: str, method: str) -> dict[str, list[FieldCandidate]]:
    return {
        "invoice_number": [
            FieldCandidate(value, method)
            for value in sorted(unique_matches(text, NUMBER_PATTERNS, normalize_number))
        ],
        "issue_date": [
            FieldCandidate(value, method)
            for value in sorted(unique_matches(text, DATE_PATTERNS, normalize_date))
        ],
        "total_amount": [
            FieldCandidate(value, method)
            for value in sorted(unique_matches(text, AMOUNT_PATTERNS, normalize_amount))
        ],
    }


def merge_candidates(
    destination: dict[str, list[FieldCandidate]],
    source: dict[str, list[FieldCandidate]],
) -> None:
    for field, values in source.items():
        destination[field].extend(values)


def find_pdftotext() -> str | None:
    candidates = [
        shutil.which("pdftotext.exe"),
        shutil.which("pdftotext"),
        os.environ.get("PDFTOTEXT_EXE"),
        str(Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Git" / "mingw64" / "bin" / "pdftotext.exe"),
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return str(Path(candidate).resolve())
    return None


def structured_xml_candidates(payload: bytes, method: str) -> dict[str, list[FieldCandidate]]:
    candidates = {field: [] for field in ("invoice_number", "issue_date", "total_amount")}
    try:
        root = ElementTree.fromstring(payload)
    except ElementTree.ParseError:
        return candidates
    for element in root.iter():
        raw = (element.text or "").strip()
        if not raw:
            continue
        tag = local_name(element.tag)
        if tag in NUMBER_TAGS:
            value = normalize_number(raw)
            if value:
                candidates["invoice_number"].append(FieldCandidate(value, method))
        elif tag in DATE_TAGS:
            value = normalize_date(raw)
            if value:
                candidates["issue_date"].append(FieldCandidate(value, method))
        elif tag in AMOUNT_TAGS:
            value = normalize_amount(raw)
            if value:
                amount_method = (
                    f"{method}_authoritative"
                    if tag in AUTHORITATIVE_AMOUNT_TAGS
                    else method
                )
                candidates["total_amount"].append(
                    FieldCandidate(value, amount_method)
                )
    return candidates


def inspect_xml(path: Path) -> dict[str, list[FieldCandidate]]:
    payload = path.read_bytes()
    result = structured_xml_candidates(payload, "structured_xml_tag")
    try:
        root = ElementTree.fromstring(payload)
        text = "\n".join(value for value in root.itertext() if value)
        merge_candidates(result, candidates_from_text(text, "xml_visible_text"))
    except ElementTree.ParseError:
        pass
    return result


def inspect_ofd(path: Path) -> dict[str, list[FieldCandidate]]:
    result = {field: [] for field in ("invoice_number", "issue_date", "total_amount")}
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            if not name.lower().endswith(".xml"):
                continue
            payload = archive.read(name)
            merge_candidates(result, structured_xml_candidates(payload, "ofd_structured_xml_tag"))
            try:
                root = ElementTree.fromstring(payload)
            except ElementTree.ParseError:
                continue
            if name.lower().endswith("content.xml"):
                text = "\n".join(
                    "".join(element.itertext())
                    for element in root.iter()
                    if local_name(str(element.tag)) == "textcode"
                )
            else:
                text = "\n".join(value for value in root.itertext() if value)
            merge_candidates(result, candidates_from_text(text, "ofd_xml_visible_text"))
    return result


def inspect_pdf(path: Path) -> dict[str, list[FieldCandidate]]:
    result = {field: [] for field in ("invoice_number", "issue_date", "total_amount")}
    try:
        with pdfplumber.open(path) as pdf:
            text = "\n".join(page.extract_text() or "" for page in pdf.pages)
        merge_candidates(result, candidates_from_text(text, "pdfplumber_text"))
    except Exception:
        pass
    try:
        reader = PdfReader(path, strict=False)
        text = "\n".join(page.extract_text() or "" for page in reader.pages)
        merge_candidates(result, candidates_from_text(text, "pypdf_text"))
        embedded_payloads = 0
        embedded_bytes = 0
        for payloads in reader.attachments.values():
            for payload in payloads:
                embedded_payloads += 1
                embedded_bytes += len(payload)
                if embedded_payloads > 32 or embedded_bytes > 4 * 1024 * 1024:
                    raise ValueError("PDF embedded attachment budget exceeded")
                if len(payload) <= 1024 * 1024:
                    merge_candidates(
                        result,
                        structured_xml_candidates(payload, "pdf_embedded_structured_xml_tag"),
                    )
    except Exception:
        pass
    pdftotext = find_pdftotext()
    if pdftotext:
        try:
            completed = subprocess.run(
                [pdftotext, "-enc", "UTF-8", "-nopgbrk", str(path), "-"],
                capture_output=True,
                check=False,
                timeout=30,
            )
            if completed.returncode == 0 and len(completed.stdout) <= 8 * 1024 * 1024:
                text = completed.stdout.decode("utf-8", errors="replace")
                merge_candidates(result, candidates_from_text(text, "poppler_pdftotext"))
        except (OSError, subprocess.SubprocessError):
            pass
    return result


def resolve_field(candidates: list[FieldCandidate], extension: str) -> tuple[str, str, str]:
    by_value: dict[str, set[str]] = {}
    for candidate in candidates:
        by_value.setdefault(candidate.value, set()).add(candidate.method)
    authoritative_by_value = {
        value: methods
        for value, methods in by_value.items()
        if any("authoritative" in method for method in methods)
    }
    if len(authoritative_by_value) == 1:
        value, methods = next(iter(authoritative_by_value.items()))
        return value, "high_confidence", "+".join(sorted(methods))
    if len(authoritative_by_value) > 1:
        return "", "needs_manual_review", "ambiguous"
    structured_by_value = {
        value: methods
        for value, methods in by_value.items()
        if any("structured" in method for method in methods)
    }
    if len(structured_by_value) == 1:
        value, methods = next(iter(structured_by_value.items()))
        return value, "high_confidence", "+".join(sorted(methods))
    if len(structured_by_value) > 1:
        return "", "needs_manual_review", "none" if not by_value else "ambiguous"
    if extension == "pdf":
        consensus_by_value = {
            value: methods
            for value, methods in by_value.items()
            if len(
                {
                    "pdfplumber_text",
                    "pypdf_text",
                    "poppler_pdftotext",
                }.intersection(methods)
            )
            >= 2
        }
        if len(consensus_by_value) == 1:
            value, methods = next(iter(consensus_by_value.items()))
            return value, "high_confidence", "+".join(sorted(methods))
        if len(consensus_by_value) > 1:
            return "", "needs_manual_review", "ambiguous"
    if len(by_value) != 1:
        return "", "needs_manual_review", "none" if not by_value else "ambiguous"
    value, methods = next(iter(by_value.items()))
    independent_pdf_methods = {
        "pdfplumber_text",
        "pypdf_text",
        "poppler_pdftotext",
    }.intersection(methods)
    if extension == "pdf" and len(independent_pdf_methods) >= 2:
        return value, "high_confidence", "+".join(sorted(methods))
    return value, "needs_manual_review", "+".join(sorted(methods))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build product-output-blind private core-field prelabels."
    )
    parser.add_argument("capture_root", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth_rows = list(csv.DictReader(stream, delimiter="\t"))
    unique_valid: dict[str, dict[str, str]] = {}
    for row in truth_rows:
        if row["human_label"] == "valid_invoice":
            unique_valid.setdefault(row["sha256"], row)

    expanded_root = capture_root / "expanded-attachments"
    rows: list[dict[str, str]] = []
    status_counts = {"high_confidence": 0, "needs_manual_review": 0}
    for digest, truth in sorted(unique_valid.items()):
        path = expanded_root / truth["file"]
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError("private attachment hash changed")
        extension = truth["extension"].lower()
        if extension == "xml":
            candidates = inspect_xml(path)
        elif extension == "ofd":
            candidates = inspect_ofd(path)
        elif extension == "pdf":
            candidates = inspect_pdf(path)
        else:
            candidates = {field: [] for field in ("invoice_number", "issue_date", "total_amount")}

        row = {
            "blind_id": truth["blind_id"],
            "file": truth["file"],
            "extension": extension,
            "sha256": digest,
        }
        for field in ("invoice_number", "issue_date", "total_amount"):
            value, status, methods = resolve_field(candidates[field], extension)
            row[field] = value
            row[f"{field}_status"] = status
            row[f"{field}_methods"] = methods
            status_counts[status] += 1
        rows.append(row)

    output = capture_root / "field-ground-truth-prelabel.private.tsv"
    with output.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    print("verification=product-output-blind-private-field-prelabel-v1")
    print(f"unique_valid_invoices={len(rows)}")
    print(f"field_slots={len(rows) * 3}")
    print(f"high_confidence_slots={status_counts['high_confidence']}")
    print(f"manual_review_slots={status_counts['needs_manual_review']}")
    print(
        "poppler_pdftotext_available="
        + str(bool(find_pdftotext())).lower()
    )
    print("product_parse_results_read=false")
    print("private_fields_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
