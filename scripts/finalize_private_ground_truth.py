from __future__ import annotations

import argparse
import csv
import sys
from collections import Counter
from pathlib import Path


ALLOWED_LABELS = {
    "valid_invoice",
    "supporting_document",
    "not_invoice",
    "corrupt_or_empty",
    "unsupported",
    "uncertain",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Finalize a private, prediction-blind attachment ground truth."
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument(
        "--label",
        action="append",
        default=[],
        metavar="BLIND_ID=LABEL",
        help="Human label for one visual-review item; repeat for every item.",
    )
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
            raise ValueError(f"duplicate human label for {blind_id}")
        overrides[blind_id] = label

    source_path = capture_root / "ground-truth-prelabel.private.tsv"
    with source_path.open("r", encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    if not rows:
        raise ValueError("private prelabel is empty")

    known_ids = {row["blind_id"] for row in rows}
    unknown_ids = set(overrides) - known_ids
    if unknown_ids:
        raise ValueError(f"human labels reference unknown blind IDs: {sorted(unknown_ids)}")

    missing_visual: list[str] = []
    for row in rows:
        blind_id = row["blind_id"]
        suggested = row["suggested_label"]
        if suggested == "needs_visual_review":
            if blind_id not in overrides:
                missing_visual.append(blind_id)
                continue
            row["human_label"] = overrides[blind_id]
            row["human_notes"] = "visual_review"
        else:
            row["human_label"] = overrides.get(blind_id, suggested)
            row["human_notes"] = (
                "visual_override" if blind_id in overrides else "independent_structure_and_text"
            )
    if missing_visual:
        raise ValueError(f"missing human labels for visual-review IDs: {missing_visual}")

    output_path = capture_root / "ground-truth-final.private.tsv"
    with output_path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    counts = Counter(row["human_label"] for row in rows)
    print("verification=prediction-blind-private-ground-truth-v1")
    print(f"attachments={len(rows)}")
    for label in sorted(counts):
        print(f"label_{label}={counts[label]}")
    print("visual_review_complete=true")
    print("product_predictions_read=false")
    print("private_text_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
