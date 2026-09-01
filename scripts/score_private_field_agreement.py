from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from collections import Counter
from decimal import Decimal, InvalidOperation
from pathlib import Path


FIELDS = ("invoice_number", "issue_date", "total_amount")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Score product fields against private independent high-confidence prelabels."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("--parse-result-overlay", type=Path, action="append", default=[])
    parser.add_argument(
        "--truth-file",
        default="field-ground-truth-prelabel.private.tsv",
        help="private TSV under capture_root",
    )
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-field-agreement.validation.json"),
    )
    return parser.parse_args()


def normalized(field: str, value: object) -> str:
    if value is None:
        return ""
    text = str(value).strip()
    if field == "invoice_number":
        return "".join(character for character in text if character.isdigit())
    if field == "issue_date":
        return text[:10]
    if field == "total_amount":
        try:
            return f"{Decimal(text.replace(',', '')):.2f}"
        except InvalidOperation:
            return text
    return text


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

    evidence = args.evidence
    if not evidence.is_absolute():
        evidence = (repo_root / evidence).resolve()
    artifacts_root = (repo_root / "artifacts").resolve()
    if evidence == artifacts_root or artifacts_root not in evidence.parents:
        raise ValueError("aggregate evidence must be written under repository artifacts")

    truth_path = (capture_root / args.truth_file).resolve(strict=True)
    if truth_path == capture_root or capture_root not in truth_path.parents:
        raise ValueError("private truth file must remain under capture_root")
    with truth_path.open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        prelabels = {row["sha256"]: row for row in csv.DictReader(stream, delimiter="\t")}
    final_truth = truth_path.name == "field-ground-truth-final.private.tsv"
    eligible_statuses = (
        {"high_confidence", "manual_visual_confirmed"}
        if final_truth
        else {"high_confidence"}
    )

    results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    by_sample = {result["sample"]: result for result in results}
    overlays: list[str] = []
    for overlay_arg in args.parse_result_overlay:
        overlay_path = overlay_arg
        if not overlay_path.is_absolute():
            overlay_path = (parse_root / overlay_path).resolve()
        else:
            overlay_path = overlay_path.resolve()
        if overlay_path == parse_root or parse_root not in overlay_path.parents:
            raise ValueError("parse-result overlays must remain in the private parse root")
        for result in json.loads(overlay_path.read_text("utf-8")):
            if result["sample"] not in by_sample:
                raise ValueError("parse-result overlay contains an unknown sample")
            by_sample[result["sample"]] = result
        overlays.append(overlay_path.name)

    samples_root = parse_root / "fixtures" / "samples"
    parsed_by_hash: dict[str, dict[str, object]] = {}
    for sample, result in by_sample.items():
        digest = hashlib.sha256((samples_root / sample).read_bytes()).hexdigest()
        parsed_by_hash[digest] = result

    field_counts = {
        field: Counter(
            {
                "eligible": 0,
                "parse_succeeded": 0,
                "matched": 0,
                "missing_in_parsed": 0,
                "mismatched": 0,
                "parse_failed": 0,
            }
        )
        for field in FIELDS
    }
    by_extension: dict[str, Counter[str]] = {}
    eligible_invoices: set[str] = set()
    full_match_invoices: set[str] = set()
    for digest, truth in prelabels.items():
        result = parsed_by_hash.get(digest)
        parsed = result.get("parsed") if result else None
        invoice_has_eligible = False
        invoice_all_match = True
        for field in FIELDS:
            if truth[f"{field}_status"] not in eligible_statuses:
                continue
            invoice_has_eligible = True
            field_counts[field]["eligible"] += 1
            extension_counts = by_extension.setdefault(
                truth["extension"], Counter({"eligible": 0, "matched": 0})
            )
            extension_counts["eligible"] += 1
            expected = normalized(field, truth[field])
            if parsed is None:
                field_counts[field]["parse_failed"] += 1
                invoice_all_match = False
                continue
            field_counts[field]["parse_succeeded"] += 1
            actual = normalized(field, parsed.get(field))
            if not actual:
                field_counts[field]["missing_in_parsed"] += 1
                invoice_all_match = False
            elif actual == expected:
                field_counts[field]["matched"] += 1
                extension_counts["matched"] += 1
            else:
                field_counts[field]["mismatched"] += 1
                invoice_all_match = False
        if invoice_has_eligible:
            eligible_invoices.add(digest)
            if invoice_all_match:
                full_match_invoices.add(digest)

    total_eligible = sum(counts["eligible"] for counts in field_counts.values())
    total_matched = sum(counts["matched"] for counts in field_counts.values())
    report = {
        "verification": "real-qq-private-independent-field-agreement-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "scope": (
            "complete_independent_plus_manual_visual_confirmed_ground_truth"
            if final_truth
            else "high_confidence_product-output-blind_prelabels_only"
        ),
        "private_truth_file": truth_path.name,
        "private_truth_sha256": hashlib.sha256(truth_path.read_bytes()).hexdigest(),
        "parse_result_overlays": overlays,
        "fields": {
            field: {
                **dict(counts),
                "agreement_among_parsed": ratio(
                    counts["matched"], counts["parse_succeeded"]
                ),
                "end_to_end_correct_coverage": ratio(
                    counts["matched"], counts["eligible"]
                ),
            }
            for field, counts in field_counts.items()
        },
        "aggregate": {
            "eligible": total_eligible,
            "parse_succeeded": sum(
                counts["parse_succeeded"] for counts in field_counts.values()
            ),
            "matched": total_matched,
            "agreement_among_parsed": ratio(
                total_matched,
                sum(counts["parse_succeeded"] for counts in field_counts.values()),
            ),
            "end_to_end_correct_coverage": ratio(total_matched, total_eligible),
        },
        "invoices": {
            "with_at_least_one_eligible_field": len(eligible_invoices),
            "all_eligible_fields_matched": len(full_match_invoices),
            "all_eligible_fields_match_rate": ratio(
                len(full_match_invoices), len(eligible_invoices)
            ),
        },
        "by_extension": {
            extension: {
                **dict(counts),
                "agreement": ratio(counts["matched"], counts["eligible"]),
            }
            for extension, counts in sorted(by_extension.items())
        },
        "manual_ground_truth_status": "complete" if final_truth else "pending_for_ambiguous_or_missing_prelabels",
        "private_fields_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-independent-field-agreement-v1")
    print(f"eligible_field_slots={total_eligible}")
    print(f"matched_field_slots={total_matched}")
    parsed_eligible = sum(
        counts["parse_succeeded"] for counts in field_counts.values()
    )
    print(f"parsed_eligible_field_slots={parsed_eligible}")
    print(f"agreement_among_parsed={ratio(total_matched, parsed_eligible):.6f}")
    print(f"end_to_end_correct_coverage={ratio(total_matched, total_eligible):.6f}")
    print(f"eligible_invoices={len(eligible_invoices)}")
    print(f"fully_matched_invoices={len(full_match_invoices)}")
    print(f"manual_ground_truth_status={'complete' if final_truth else 'pending_for_ambiguous_or_missing_prelabels'}")
    print("private_fields_logged=false")
    print(f"evidence={evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
