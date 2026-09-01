from __future__ import annotations

import argparse
import csv
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Score private email-level classification.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument(
        "--prediction-file",
        type=Path,
        default=Path("reclassified.private.tsv"),
    )
    parser.add_argument(
        "--evidence",
        type=Path,
        default=Path("artifacts/real-qq-email-classifier-score.validation.json"),
    )
    return parser.parse_args()


def ratio(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def metrics(actual: dict[str, bool], predicted: dict[str, bool]) -> dict[str, object]:
    tp = sum(actual[key] and predicted[key] for key in actual)
    fp = sum(not actual[key] and predicted[key] for key in actual)
    tn = sum(not actual[key] and not predicted[key] for key in actual)
    fn = sum(actual[key] and not predicted[key] for key in actual)
    precision = ratio(tp, tp + fp)
    recall = ratio(tp, tp + fn)
    return {
        "tp": tp,
        "fp": fp,
        "tn": tn,
        "fn": fn,
        "precision": precision,
        "recall": recall,
        "f1": (
            2 * precision * recall / (precision + recall)
            if precision is not None and recall is not None and precision + recall
            else None
        ),
        "accuracy": ratio(tp + tn, tp + fp + tn + fn),
    }


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")

    prediction_path = args.prediction_file
    if not prediction_path.is_absolute():
        prediction_path = (capture_root / prediction_path).resolve()
    else:
        prediction_path = prediction_path.resolve()
    if prediction_path == capture_root or capture_root not in prediction_path.parents:
        raise ValueError("prediction file must remain in the private capture root")

    evidence = args.evidence
    if not evidence.is_absolute():
        evidence = (repo_root / evidence).resolve()
    artifacts_root = (repo_root / "artifacts").resolve()
    if evidence == artifacts_root or artifacts_root not in evidence.parents:
        raise ValueError("aggregate evidence must be under repository artifacts")

    with (capture_root / "email-ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        truth_rows = list(csv.DictReader(stream, delimiter="\t"))
    truth = {row["email_file"]: row["human_label"] for row in truth_rows}
    if len(truth) != len(truth_rows):
        raise ValueError("email ground truth contains duplicate email files")

    with prediction_path.open("r", encoding="utf-8", newline="") as stream:
        predicted_hashes = {
            row["sha256"]
            for row in csv.DictReader(stream, delimiter="\t")
            if row["predicted_invoice"].lower() == "true"
        }
    with (capture_root / "attachments.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        attachments = [
            row
            for row in csv.DictReader(stream, delimiter="\t")
            if row["layer"] == "expanded"
        ]
    hashes_by_email: dict[str, set[str]] = defaultdict(set)
    for row in attachments:
        hashes_by_email[row["email_file"]].add(row["sha256"])
    predicted = {
        email_file: bool(hashes_by_email[email_file] & predicted_hashes)
        for email_file in truth
    }

    strict_labels = {"invoice_attachment", "invoice_link_only", "invoice_notice_only"}
    related_labels = strict_labels | {"supporting_attachment"}
    direct_labels = {"invoice_attachment"}
    category_counts = Counter(truth.values())
    predicted_counts = Counter()
    missed_counts = Counter()
    for email_file, label in truth.items():
        predicted_counts[f"{label}:predicted"] += int(predicted[email_file])
        predicted_counts[f"{label}:not_predicted"] += int(not predicted[email_file])
        if label in related_labels and not predicted[email_file]:
            missed_counts[label] += 1

    report = {
        "verification": "real-qq-private-email-classifier-score-v1",
        "account": "879***187@qq.com",
        "range": "[2026-06-01, 2026-07-01)",
        "read_only_unchanged": True,
        "emails": len(truth),
        "ground_truth_categories": dict(sorted(category_counts.items())),
        "invoice_related": metrics(
            {key: label in related_labels for key, label in truth.items()}, predicted
        ),
        "strict_invoice": metrics(
            {key: label in strict_labels for key, label in truth.items()}, predicted
        ),
        "direct_invoice_attachment": metrics(
            {key: label in direct_labels for key, label in truth.items()}, predicted
        ),
        "prediction_by_category": dict(sorted(predicted_counts.items())),
        "missed_related_by_category": dict(sorted(missed_counts.items())),
        "link_only_supported": False,
        "notice_only_supported": False,
        "private_message_fields_in_evidence": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", "utf-8")

    print("verification=real-qq-private-email-classifier-score-v1")
    print(f"emails={len(truth)}")
    for scope in ("invoice_related", "strict_invoice", "direct_invoice_attachment"):
        score = report[scope]
        print(
            f"{scope}=tp:{score['tp']},fp:{score['fp']},tn:{score['tn']},fn:{score['fn']},"
            f"precision:{score['precision']:.6f},recall:{score['recall']:.6f},f1:{score['f1']:.6f}"
        )
    print("link_only_supported=false")
    print("notice_only_supported=false")
    print("private_message_text_logged=false")
    print(f"evidence={evidence}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
