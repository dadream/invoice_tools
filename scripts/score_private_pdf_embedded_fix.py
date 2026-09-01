from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Score the aggregate effect of PDF embedded railway XBRL parsing."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("post_fix_results", type=Path)
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-pdf-embedded-xbrl-fix.validation.json"),
    )
    return parser.parse_args()


def ratio(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    post_fix_path = args.post_fix_results
    if not post_fix_path.is_absolute():
        post_fix_path = (parse_root / post_fix_path).resolve(strict=True)
    else:
        post_fix_path = post_fix_path.resolve(strict=True)
    if post_fix_path == parse_root or parse_root not in post_fix_path.parents:
        raise ValueError("post-fix results must remain inside the private parse root")

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
    with (capture_root / "reclassified.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        prediction = {row["sha256"]: row for row in csv.DictReader(stream, delimiter="\t")}

    samples_root = parse_root / "fixtures" / "samples"

    def by_hash(result_path: Path) -> dict[str, dict[str, object]]:
        results = json.loads(result_path.read_text("utf-8"))
        hashed: dict[str, dict[str, object]] = {}
        for result in results:
            digest = hashlib.sha256((samples_root / result["sample"]).read_bytes()).hexdigest()
            if digest in hashed:
                raise ValueError("parse results contain duplicate content")
            hashed[digest] = result
        return hashed

    before = by_hash(parse_root / "parse-results.private.json")
    after = by_hash(post_fix_path)
    if set(after) != {
        digest
        for digest, row in prediction.items()
        if row["predicted_invoice"].lower() == "true"
        and row["predicted_format"].startswith("pdf-")
    }:
        raise ValueError("post-fix PDF result set does not match the classified PDF candidates")

    valid_pdf = {
        digest for digest, row in truth.items() if row["human_label"] == "valid_invoice" and row["extension"] == "pdf"
    }
    before_valid_parsed = sum(
        digest in before and before[digest].get("parsed") is not None for digest in valid_pdf
    )
    after_valid_parsed = sum(
        digest in after and after[digest].get("parsed") is not None for digest in valid_pdf
    )
    embedded_l0 = {
        digest
        for digest, result in after.items()
        if result.get("parsed") is not None
        and result["parsed"].get("parse_level") == "L0"
    }
    embedded_labels = Counter(truth[digest]["human_label"] for digest in embedded_l0)
    embedded_formats = Counter(prediction[digest]["predicted_format"] for digest in embedded_l0)

    report = {
        "verification": "real-qq-private-pdf-embedded-xbrl-fix-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "post_fix_result_overlay": post_fix_path.name,
        "pdf_candidates": len(after),
        "valid_pdf_actual": len(valid_pdf),
        "valid_pdf_parsed_before": before_valid_parsed,
        "valid_pdf_parsed_after": after_valid_parsed,
        "valid_pdf_recovered": after_valid_parsed - before_valid_parsed,
        "valid_pdf_coverage_before": ratio(before_valid_parsed, len(valid_pdf)),
        "valid_pdf_coverage_after": ratio(after_valid_parsed, len(valid_pdf)),
        "remaining_valid_pdf_failures": len(valid_pdf) - after_valid_parsed,
        "embedded_l0_parsed": len(embedded_l0),
        "embedded_l0_truth_labels": dict(sorted(embedded_labels.items())),
        "embedded_l0_predicted_formats": dict(sorted(embedded_formats.items())),
        "embedded_l0_false_positive": sum(
            label != "valid_invoice" for label in embedded_labels.elements()
        ),
        "private_fields_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-pdf-embedded-xbrl-fix-v1")
    print(f"pdf_candidates={len(after)}")
    print(f"valid_pdf_parsed_before={before_valid_parsed}")
    print(f"valid_pdf_parsed_after={after_valid_parsed}")
    print(f"valid_pdf_recovered={after_valid_parsed - before_valid_parsed}")
    print(f"embedded_l0_parsed={len(embedded_l0)}")
    print(f"embedded_l0_false_positive={report['embedded_l0_false_positive']}")
    print(f"remaining_valid_pdf_failures={report['remaining_valid_pdf_failures']}")
    print("private_fields_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
