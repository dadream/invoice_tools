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
        description="Compare two private parse overlays and emit aggregate-only evidence."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("before_results", type=Path)
    parser.add_argument("after_results", type=Path)
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-pdf-layout-fixes.validation.json"),
    )
    return parser.parse_args()


def resolve_private_file(root: Path, value: Path) -> Path:
    path = value.resolve(strict=True) if value.is_absolute() else (root / value).resolve(strict=True)
    if path == root or root not in path.parents:
        raise ValueError("parse result files must remain inside the private parse root")
    return path


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    before_path = resolve_private_file(parse_root, args.before_results)
    after_path = resolve_private_file(parse_root, args.after_results)
    evidence = args.evidence
    if not evidence.is_absolute():
        evidence = (repo_root / evidence).resolve()
    artifacts_root = (repo_root / "artifacts").resolve()
    if evidence == artifacts_root or artifacts_root not in evidence.parents:
        raise ValueError("aggregate evidence must be written under repository artifacts")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth: dict[str, dict[str, str]] = {}
        for row in csv.DictReader(stream, delimiter="\t"):
            truth.setdefault(row["sha256"], row)

    samples_root = parse_root / "fixtures" / "samples"

    def load(path: Path) -> dict[str, dict[str, object]]:
        mapped: dict[str, dict[str, object]] = {}
        for result in json.loads(path.read_text("utf-8")):
            digest = hashlib.sha256((samples_root / result["sample"]).read_bytes()).hexdigest()
            if digest in mapped:
                raise ValueError("parse results contain duplicate content")
            mapped[digest] = result
        return mapped

    before = load(before_path)
    after = load(after_path)
    if set(before) != set(after):
        raise ValueError("before and after result sets differ")

    before_success = {digest for digest, result in before.items() if result.get("parsed") is not None}
    after_success = {digest for digest, result in after.items() if result.get("parsed") is not None}
    newly_parsed = after_success - before_success
    newly_failed = before_success - after_success
    stable_success = before_success & after_success
    changed_stable_results = sum(
        before[digest]["parsed"] != after[digest]["parsed"] for digest in stable_success
    )
    changed_field_counts: Counter[str] = Counter()
    for digest in stable_success:
        before_parsed = before[digest]["parsed"]
        after_parsed = after[digest]["parsed"]
        for field in set(before_parsed) | set(after_parsed):
            if before_parsed.get(field) != after_parsed.get(field):
                changed_field_counts[field] += 1
    new_truth_labels = Counter(
        truth.get(digest, {}).get("human_label", "missing_truth") for digest in newly_parsed
    )
    valid_pdf = {
        digest
        for digest, row in truth.items()
        if row["human_label"] == "valid_invoice" and row["extension"] == "pdf"
    }
    level_counts = Counter(
        result["parsed"]["parse_level"]
        for result in after.values()
        if result.get("parsed") is not None
    )

    report = {
        "verification": "real-qq-private-parse-overlay-diff-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "before_result_overlay": before_path.name,
        "after_result_overlay": after_path.name,
        "candidate_files": len(after),
        "parse_success_before": len(before_success),
        "parse_success_after": len(after_success),
        "newly_parsed": len(newly_parsed),
        "newly_failed": len(newly_failed),
        "changed_existing_success_results": changed_stable_results,
        "changed_field_counts": dict(sorted(changed_field_counts.items())),
        "newly_parsed_truth_labels": dict(sorted(new_truth_labels.items())),
        "valid_pdf_actual": len(valid_pdf),
        "valid_pdf_parsed_before": len(valid_pdf & before_success),
        "valid_pdf_parsed_after": len(valid_pdf & after_success),
        "valid_pdf_remaining": len(valid_pdf - after_success),
        "after_parse_levels": dict(sorted(level_counts.items())),
        "private_names_hashes_and_field_values_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-parse-overlay-diff-v1")
    for field in (
        "candidate_files",
        "parse_success_before",
        "parse_success_after",
        "newly_parsed",
        "newly_failed",
        "changed_existing_success_results",
        "valid_pdf_parsed_before",
        "valid_pdf_parsed_after",
        "valid_pdf_remaining",
    ):
        print(f"{field}={report[field]}")
    print(f"changed_field_counts={json.dumps(report['changed_field_counts'], sort_keys=True)}")
    print("private_fields_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
