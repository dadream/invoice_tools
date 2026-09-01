from __future__ import annotations

import argparse
import csv
import sys
from collections import Counter
from pathlib import Path


ALLOWED_LABELS = {
    "invoice_attachment",
    "supporting_attachment",
    "invoice_link_only",
    "invoice_notice_only",
    "not_invoice",
    "uncertain",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Finalize private blind email ground truth.")
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("--label", action="append", default=[], metavar="EMAIL_ID=LABEL")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")

    overrides: dict[str, str] = {}
    for value in args.label:
        blind_id, separator, label = value.partition("=")
        if not separator or not blind_id or label not in ALLOWED_LABELS:
            raise ValueError(f"invalid --label value: {value}")
        if blind_id in overrides:
            raise ValueError(f"duplicate human label: {blind_id}")
        overrides[blind_id] = label

    with (capture_root / "email-ground-truth-prelabel.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    known = {row["blind_email_id"] for row in rows}
    if set(overrides) - known:
        raise ValueError("human labels reference unknown email IDs")

    missing: list[str] = []
    for row in rows:
        blind_id = row["blind_email_id"]
        if row["requires_visual_review"] == "true":
            if blind_id not in overrides:
                missing.append(blind_id)
                continue
            row["human_label"] = overrides[blind_id]
            row["human_notes"] = "prediction_blind_visual_review"
        else:
            row["human_label"] = overrides.get(blind_id, row["suggested_label"])
            row["human_notes"] = (
                "visual_override" if blind_id in overrides else "attachment_ground_truth_derived"
            )
    if missing:
        raise ValueError(f"missing visual labels: {missing}")

    with (capture_root / "email-ground-truth-final.private.tsv").open(
        "w", encoding="utf-8", newline=""
    ) as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    counts = Counter(row["human_label"] for row in rows)
    print("verification=product-output-blind-private-email-ground-truth-v1")
    print(f"emails={len(rows)}")
    for label in sorted(counts):
        print(f"label_{label}={counts[label]}")
    print("visual_review_complete=true")
    print("product_predictions_read=false")
    print("private_message_text_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
