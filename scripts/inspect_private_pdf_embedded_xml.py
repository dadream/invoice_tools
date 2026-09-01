from __future__ import annotations

import argparse
import csv
import hashlib
import re
import sys
from collections import Counter
from pathlib import Path
from xml.etree import ElementTree

from pypdf import PdfReader


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Inspect aggregate schema tags in private PDF embedded XML files."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    return parser.parse_args()


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1]


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        valid_hashes = {
            row["sha256"]
            for row in csv.DictReader(stream, delimiter="\t")
            if row["human_label"] == "valid_invoice" and row["extension"] == "pdf"
        }
    with (capture_root / "reclassified.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        rail_hashes = {
            row["sha256"]
            for row in csv.DictReader(stream, delimiter="\t")
            if row["predicted_format"] == "pdf-rail"
        }

    results = __import__("json").loads(
        (parse_root / "parse-results.private.json").read_text("utf-8")
    )
    samples_root = parse_root / "fixtures" / "samples"
    tag_counts: Counter[str] = Counter()
    numeric_tag_counts: Counter[str] = Counter()
    date_tag_counts: Counter[str] = Counter()
    decimal_tag_counts: Counter[str] = Counter()
    attachment_extensions: Counter[str] = Counter()
    files = 0
    embedded_payloads = 0
    xml_payloads = 0
    for result in results:
        sample = samples_root / result["sample"]
        digest = hashlib.sha256(sample.read_bytes()).hexdigest()
        if (
            digest not in valid_hashes
            or digest not in rail_hashes
            or result.get("parsed") is not None
        ):
            continue
        files += 1
        reader = PdfReader(sample, strict=False)
        for filename, payloads in reader.attachments.items():
            attachment_extensions[Path(filename).suffix.lower() or "none"] += len(payloads)
            for payload in payloads:
                embedded_payloads += 1
                try:
                    root = ElementTree.fromstring(payload)
                except ElementTree.ParseError:
                    continue
                xml_payloads += 1
                for element in root.iter():
                    name = local_name(str(element.tag))
                    text = (element.text or "").strip()
                    tag_counts[name] += 1
                    if re.fullmatch(r"\d{8,24}", text):
                        numeric_tag_counts[name] += 1
                    if re.fullmatch(r"20\d{2}[-/.年]?\d{1,2}[-/.月]?\d{1,2}日?", text):
                        date_tag_counts[name] += 1
                    if re.fullmatch(r"[￥¥]?\d{1,12}[,.]\d{2}", text):
                        decimal_tag_counts[name] += 1

    semantic_names = re.compile(
        r"invoice|ticket|date|time|amount|fare|tax|buyer|seller|station|train|departure|arrival",
        re.IGNORECASE,
    )
    semantic_tags = Counter(
        {name: count for name, count in tag_counts.items() if semantic_names.search(name)}
    )

    print("verification=private-pdf-embedded-xml-schema-v1")
    print(f"failed_rail_pdfs={files}")
    print(f"embedded_payloads={embedded_payloads}")
    print(f"xml_payloads={xml_payloads}")
    print("attachment_extensions=" + repr(dict(sorted(attachment_extensions.items()))))
    print("numeric_tags=" + repr(dict(numeric_tag_counts.most_common(30))))
    print("date_tags=" + repr(dict(date_tag_counts.most_common(30))))
    print("decimal_tags=" + repr(dict(decimal_tag_counts.most_common(40))))
    print("semantic_tags=" + repr(dict(semantic_tags.most_common(80))))
    print("private_values_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
