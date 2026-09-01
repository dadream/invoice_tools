from __future__ import annotations

import argparse
import csv
import json
import math
import sys
from collections import Counter
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Score private attachment ground truth.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-classifier-score.validation.json"),
    )
    parser.add_argument(
        "--prediction-file",
        type=Path,
        help="Optional offline reclassification TSV keyed by sha256.",
    )
    return parser.parse_args()


def safe_ratio(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def wilson(successes: int, total: int, z: float = 1.959963984540054) -> list[float] | None:
    if total == 0:
        return None
    proportion = successes / total
    denominator = 1 + z * z / total
    centre = proportion + z * z / (2 * total)
    margin = z * math.sqrt((proportion * (1 - proportion) + z * z / (4 * total)) / total)
    return [(centre - margin) / denominator, (centre + margin) / denominator]


def binary_metrics(rows: list[dict[str, str]], positive_labels: set[str]) -> dict[str, object]:
    tp = fp = tn = fn = 0
    for row in rows:
        actual = row["human_label"] in positive_labels
        predicted = row["predicted_invoice"].lower() == "true"
        if actual and predicted:
            tp += 1
        elif actual:
            fn += 1
        elif predicted:
            fp += 1
        else:
            tn += 1
    precision = safe_ratio(tp, tp + fp)
    recall = safe_ratio(tp, tp + fn)
    f1 = (
        2 * precision * recall / (precision + recall)
        if precision is not None and recall is not None and precision + recall
        else None
    )
    return {
        "tp": tp,
        "fp": fp,
        "tn": tn,
        "fn": fn,
        "precision": precision,
        "precision_wilson95": wilson(tp, tp + fp),
        "recall": recall,
        "recall_wilson95": wilson(tp, tp + fn),
        "f1": f1,
        "accuracy": safe_ratio(tp + tn, len(rows)),
    }


def score(rows: list[dict[str, str]]) -> dict[str, object]:
    label_prediction = Counter(
        (row["human_label"], row["predicted_invoice"].lower()) for row in rows
    )
    invalid = [
        row for row in rows if row["human_label"] in {"not_invoice", "corrupt_or_empty"}
    ]
    invalid_rejected = sum(
        row["predicted_invoice"].lower() == "false" for row in invalid
    )
    return {
        "total": len(rows),
        "labels": dict(sorted(Counter(row["human_label"] for row in rows).items())),
        "strict_valid_invoice": binary_metrics(rows, {"valid_invoice"}),
        "reimbursable_material": binary_metrics(
            rows, {"valid_invoice", "supporting_document"}
        ),
        "invalid_or_not_invoice_rejection": {
            "rejected": invalid_rejected,
            "total": len(invalid),
            "rate": safe_ratio(invalid_rejected, len(invalid)),
            "wilson95": wilson(invalid_rejected, len(invalid)),
        },
        "label_prediction_counts": {
            f"{label}|predicted_{prediction}": count
            for (label, prediction), count in sorted(label_prediction.items())
        },
    }


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")

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
    joined: list[dict[str, str]] = []
    prediction_source = "captured_product_prediction"
    if args.prediction_file is None:
        with (capture_root / "attachments.private.tsv").open(
            "r", encoding="utf-8", newline=""
        ) as stream:
            prediction_rows = [
                row
                for row in csv.DictReader(stream, delimiter="\t")
                if row["layer"] == "expanded"
            ]
        predictions = {row["file"]: row for row in prediction_rows}
        if len(predictions) != len(prediction_rows):
            raise ValueError("product prediction inventory contains duplicate file identifiers")
        for truth in truth_rows:
            prediction = predictions.get(truth["file"])
            if prediction is None:
                raise ValueError("ground truth does not match the private prediction inventory")
            joined.append({**truth, "predicted_invoice": prediction["predicted_invoice"]})
        if len(joined) != len(predictions):
            raise ValueError("private prediction inventory contains unlabelled attachments")
    else:
        prediction_path = args.prediction_file
        if not prediction_path.is_absolute():
            prediction_path = (capture_root / prediction_path).resolve()
        else:
            prediction_path = prediction_path.resolve()
        if prediction_path == capture_root or capture_root not in prediction_path.parents:
            raise ValueError("offline prediction file must remain in the private capture root")
        with prediction_path.open("r", encoding="utf-8", newline="") as stream:
            prediction_rows = list(csv.DictReader(stream, delimiter="\t"))
        predictions_by_hash: dict[str, str] = {}
        for row in prediction_rows:
            existing = predictions_by_hash.setdefault(row["sha256"], row["predicted_invoice"])
            if existing != row["predicted_invoice"]:
                raise ValueError("same attachment content received inconsistent predictions")
        for truth in truth_rows:
            predicted = predictions_by_hash.get(truth["sha256"])
            if predicted is None:
                raise ValueError("ground truth does not match the offline prediction inventory")
            joined.append({**truth, "predicted_invoice": predicted})
        prediction_source = "offline_production_reclassification_after_fix"

    unique_by_hash: dict[str, dict[str, str]] = {}
    for row in joined:
        unique_by_hash.setdefault(row["sha256"], row)
    unique_rows = list(unique_by_hash.values())

    report = {
        "verification": "real-qq-private-classifier-score-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "label_source": "prediction_blind_independent_structure_text_and_visual_review",
        "prediction_source": prediction_source,
        "all_logical_attachments": score(joined),
        "unique_content": score(unique_rows),
        "duplicates_removed_for_unique_score": len(joined) - len(unique_rows),
        "private_fields_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    strict = report["all_logical_attachments"]["strict_valid_invoice"]
    broad = report["all_logical_attachments"]["reimbursable_material"]
    rejection = report["all_logical_attachments"]["invalid_or_not_invoice_rejection"]
    print("verification=real-qq-private-classifier-score-v1")
    print(f"logical_attachments={len(joined)}")
    print(f"unique_content={len(unique_rows)}")
    print(f"strict_precision={strict['precision']:.6f}")
    print(f"strict_recall={strict['recall']:.6f}")
    print(f"strict_f1={strict['f1']:.6f}")
    print(f"reimbursable_precision={broad['precision']:.6f}")
    print(f"reimbursable_recall={broad['recall']:.6f}")
    print(f"reimbursable_f1={broad['f1']:.6f}")
    print(f"invalid_rejection_rate={rejection['rate']:.6f}")
    print("private_fields_logged=false")
    print(f"evidence={evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
