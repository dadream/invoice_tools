from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Score private end-to-end parse coverage.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_capture_root", type=Path)
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-parse-coverage.validation.json"),
    )
    parser.add_argument(
        "--prediction-file",
        type=Path,
        help="Optional offline reclassification TSV keyed by sha256.",
    )
    parser.add_argument(
        "--parse-result-overlay",
        type=Path,
        action="append",
        default=[],
        help=(
            "Optional private parse-result JSON whose samples replace matching "
            "entries in parse-results.private.json (repeatable)."
        ),
    )
    return parser.parse_args()


def ratio(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private capture roots must remain outside the Git repository")

    evidence = args.evidence
    if not evidence.is_absolute():
        evidence = (repo_root / evidence).resolve()
    artifacts_root = (repo_root / "artifacts").resolve()
    if evidence == artifacts_root or artifacts_root not in evidence.parents:
        raise ValueError("aggregate evidence must be written under repository artifacts")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth_rows = list(csv.DictReader(stream, delimiter="\t"))
    prediction_source = "captured_product_prediction"
    if args.prediction_file is None:
        prediction_path = capture_root / "attachments.private.tsv"
        with prediction_path.open("r", encoding="utf-8", newline="") as stream:
            predictions = {
                row["sha256"]: row
                for row in csv.DictReader(stream, delimiter="\t")
                if row["layer"] == "expanded"
                and row["predicted_invoice"].lower() == "true"
            }
    else:
        prediction_path = args.prediction_file
        if not prediction_path.is_absolute():
            prediction_path = (capture_root / prediction_path).resolve()
        else:
            prediction_path = prediction_path.resolve()
        if prediction_path == capture_root or capture_root not in prediction_path.parents:
            raise ValueError("offline prediction file must remain in the private capture root")
        with prediction_path.open("r", encoding="utf-8", newline="") as stream:
            predictions = {
                row["sha256"]: row
                for row in csv.DictReader(stream, delimiter="\t")
                if row["predicted_invoice"].lower() == "true"
            }
        prediction_source = "offline_production_reclassification_after_fix"

    parse_results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    results_by_sample = {result["sample"]: result for result in parse_results}
    if len(results_by_sample) != len(parse_results):
        raise ValueError("base parse results contain duplicate sample names")
    applied_overlays: list[str] = []
    for overlay_arg in args.parse_result_overlay:
        overlay_path = overlay_arg
        if not overlay_path.is_absolute():
            overlay_path = (parse_root / overlay_path).resolve()
        else:
            overlay_path = overlay_path.resolve()
        if overlay_path == parse_root or parse_root not in overlay_path.parents:
            raise ValueError("parse-result overlays must remain in the private parse root")
        overlay_results = json.loads(overlay_path.read_text("utf-8"))
        overlay_samples: set[str] = set()
        for result in overlay_results:
            sample = result["sample"]
            if sample in overlay_samples:
                raise ValueError(f"parse-result overlay contains duplicate sample: {sample}")
            if sample not in results_by_sample:
                raise ValueError(f"parse-result overlay contains unknown sample: {sample}")
            overlay_samples.add(sample)
            results_by_sample[sample] = result
        applied_overlays.append(overlay_path.name)
    parse_results = list(results_by_sample.values())
    samples_root = parse_root / "fixtures" / "samples"
    parsed_by_hash: dict[str, dict[str, object]] = {}
    for result in parse_results:
        sample_path = samples_root / result["sample"]
        digest = hashlib.sha256(sample_path.read_bytes()).hexdigest()
        if digest in parsed_by_hash:
            raise ValueError("parse capture contains duplicate candidate content")
        parsed_by_hash[digest] = result
    if set(parsed_by_hash) != set(predictions):
        raise ValueError("parse capture and final product-positive hash sets differ")

    unique_truth: dict[str, dict[str, str]] = {}
    for row in truth_rows:
        unique_truth.setdefault(row["sha256"], row)
    rows = list(unique_truth.values())

    label_counts = Counter(row["human_label"] for row in rows)
    classified_counts = Counter()
    parsed_counts = Counter()
    parse_levels = Counter()
    parsed_by_extension = Counter()
    failed_by_extension = Counter()
    valid_actual_by_extension = Counter()
    valid_classified_by_extension = Counter()
    valid_parsed_by_extension = Counter()
    parsed_by_label_extension: dict[str, Counter[str]] = {}
    for row in rows:
        label = row["human_label"]
        digest = row["sha256"]
        extension = row["extension"]
        if label == "valid_invoice":
            valid_actual_by_extension[extension] += 1
        if digest not in predictions:
            continue
        classified_counts[label] += 1
        if label == "valid_invoice":
            valid_classified_by_extension[extension] += 1
        parse_result = parsed_by_hash[digest]
        parsed = parse_result.get("parsed")
        if parsed is None:
            failed_by_extension[extension] += 1
            continue
        parsed_counts[label] += 1
        parsed_by_label_extension.setdefault(label, Counter())[extension] += 1
        parse_levels[str(parsed["parse_level"])] += 1
        parsed_by_extension[extension] += 1
        if label == "valid_invoice":
            valid_parsed_by_extension[extension] += 1

    def coverage(labels: set[str]) -> dict[str, object]:
        actual = sum(label_counts[label] for label in labels)
        classified = sum(classified_counts[label] for label in labels)
        parsed = sum(parsed_counts[label] for label in labels)
        return {
            "actual": actual,
            "classified": classified,
            "classification_recall": ratio(classified, actual),
            "parsed": parsed,
            "parse_success_among_classified": ratio(parsed, classified),
            "end_to_end_parse_coverage": ratio(parsed, actual),
        }

    valid_rows = [row for row in rows if row["human_label"] == "valid_invoice"]
    ocr_required = sum(
        row["extension"] in {"jpg", "jpeg", "png", "webp", "bmp"}
        or (row["extension"] == "pdf" and int(row["text_chars"]) < 50)
        for row in valid_rows
    )
    direct_source = len(valid_rows) - ocr_required

    def final_decision(positive_labels: set[str]) -> dict[str, object]:
        actual_positive = sum(label_counts[label] for label in positive_labels)
        accepted_positive = sum(parsed_counts[label] for label in positive_labels)
        accepted_negative = sum(
            count for label, count in parsed_counts.items() if label not in positive_labels
        )
        actual_negative = len(rows) - actual_positive
        rejected_negative = actual_negative - accepted_negative
        precision = ratio(accepted_positive, accepted_positive + accepted_negative)
        recall = ratio(accepted_positive, actual_positive)
        return {
            "tp": accepted_positive,
            "fp": accepted_negative,
            "tn": rejected_negative,
            "fn": actual_positive - accepted_positive,
            "precision": precision,
            "recall": recall,
            "accuracy": ratio(accepted_positive + rejected_negative, len(rows)),
        }

    invalid_labels = {"corrupt_or_empty", "not_invoice"}
    invalid_actual = sum(label_counts[label] for label in invalid_labels)
    invalid_accepted = sum(parsed_counts[label] for label in invalid_labels)

    report = {
        "verification": "real-qq-private-parse-coverage-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "prediction_source": prediction_source,
        "parse_result_overlays": applied_overlays,
        "unique_logical_attachments": len(rows),
        "candidate_files_parsed": len(parsed_by_hash),
        "labels": dict(sorted(label_counts.items())),
        "valid_invoice": coverage({"valid_invoice"}),
        "reimbursable_material": coverage({"valid_invoice", "supporting_document"}),
        "parse_outcomes_by_label": {
            label: {
                "actual": label_counts[label],
                "classified": classified_counts[label],
                "parsed_and_accepted": parsed_counts[label],
                "rejected_before_parse": label_counts[label] - classified_counts[label],
                "rejected_by_parse": classified_counts[label] - parsed_counts[label],
            }
            for label in sorted(label_counts)
        },
        "accepted_by_label_and_extension": {
            label: dict(sorted(parsed_by_label_extension.get(label, Counter()).items()))
            for label in sorted(label_counts)
        },
        "strict_valid_invoice_final_decision": final_decision({"valid_invoice"}),
        "reimbursable_material_final_decision": final_decision(
            {"valid_invoice", "supporting_document"}
        ),
        "invalid_or_not_invoice_final_rejection": {
            "rejected": invalid_actual - invalid_accepted,
            "accepted": invalid_accepted,
            "total": invalid_actual,
            "rate": ratio(invalid_actual - invalid_accepted, invalid_actual),
        },
        "valid_invoice_source_need": {
            "direct_capable": direct_source,
            "ocr_required": ocr_required,
            "direct_capable_rate": ratio(direct_source, len(valid_rows)),
            "ocr_required_rate": ratio(ocr_required, len(valid_rows)),
        },
        "successful_parse_levels": dict(sorted(parse_levels.items())),
        "successful_by_extension": dict(sorted(parsed_by_extension.items())),
        "failed_by_extension": dict(sorted(failed_by_extension.items())),
        "valid_invoice_by_extension": {
            extension: {
                "actual": valid_actual_by_extension[extension],
                "classified": valid_classified_by_extension[extension],
                "parsed": valid_parsed_by_extension[extension],
                "end_to_end_parse_coverage": ratio(
                    valid_parsed_by_extension[extension],
                    valid_actual_by_extension[extension],
                ),
            }
            for extension in sorted(valid_actual_by_extension)
        },
        "field_accuracy_status": "pending_manual_core_field_ground_truth",
        "private_fields_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    valid = report["valid_invoice"]
    broad = report["reimbursable_material"]
    print("verification=real-qq-private-parse-coverage-v1")
    print(f"unique_logical_attachments={len(rows)}")
    print(f"valid_invoice_actual={valid['actual']}")
    print(f"valid_invoice_classified={valid['classified']}")
    print(f"valid_invoice_parsed={valid['parsed']}")
    print(f"valid_invoice_end_to_end_coverage={valid['end_to_end_parse_coverage']:.6f}")
    print(f"reimbursable_actual={broad['actual']}")
    print(f"reimbursable_parsed={broad['parsed']}")
    print(f"reimbursable_end_to_end_coverage={broad['end_to_end_parse_coverage']:.6f}")
    invalid = report["invalid_or_not_invoice_final_rejection"]
    print(f"invalid_final_rejected={invalid['rejected']}")
    print(f"invalid_final_accepted={invalid['accepted']}")
    print(f"invalid_final_rejection_rate={invalid['rate']:.6f}")
    print(f"valid_invoice_ocr_required={ocr_required}")
    print(f"valid_invoice_direct_capable={direct_source}")
    print("field_accuracy_status=pending_manual_core_field_ground_truth")
    print("private_fields_logged=false")
    print(f"evidence={evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
