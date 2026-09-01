from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from decimal import Decimal, InvalidOperation
from pathlib import Path


FIELDS = ("invoice_number", "issue_date", "total_amount")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Finalize private core-field ground truth after product-output-blind "
            "prelabeling and explicit visual confirmation of all remaining fields."
        )
    )
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("parse_root", type=Path)
    parser.add_argument("parse_result_overlay", type=Path)
    parser.add_argument(
        "--confirmation-file",
        type=Path,
        default=Path("field-ground-truth-manual-confirmations.private.tsv"),
    )
    parser.add_argument(
        "--blind-review-name", default="field-ground-truth-review-private-v2"
    )
    parser.add_argument(
        "--comparison-review-name", default="field-prediction-comparison-private-v1"
    )
    parser.add_argument(
        "--output-name", default="field-ground-truth-final.private.tsv"
    )
    return parser.parse_args()


def normalize(field: str, value: object) -> str:
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
            return ""
    return text


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def resolve_private_file(root: Path, value: Path) -> Path:
    path = value.resolve(strict=True) if value.is_absolute() else (root / value).resolve(strict=True)
    if path == root or root not in path.parents:
        raise ValueError("private input file must remain inside its private root")
    return path


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    parse_root = args.parse_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    for root in (capture_root, parse_root):
        if root == repo_root or repo_root in root.parents:
            raise ValueError("private roots must remain outside the Git repository")

    overlay_path = resolve_private_file(parse_root, args.parse_result_overlay)
    confirmation_path = resolve_private_file(capture_root, args.confirmation_file)
    output_path = capture_root / args.output_name
    if output_path.exists():
        raise FileExistsError("final private ground truth already exists")

    with (capture_root / "field-ground-truth-prelabel.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        rows = list(csv.DictReader(stream, delimiter="\t"))
    if len(rows) != 84:
        raise ValueError("expected exactly 84 validated invoice rows")

    with confirmation_path.open("r", encoding="utf-8", newline="") as stream:
        confirmation_rows = list(csv.DictReader(stream, delimiter="\t"))
    confirmations: dict[str, set[str]] = {}
    for row in confirmation_rows:
        if row["review_status"] != "confirmed_against_source":
            raise ValueError("every manual confirmation must be confirmed against source")
        fields = {field for field in row["confirmed_fields"].split(",") if field}
        if not fields or not fields <= set(FIELDS):
            raise ValueError("manual confirmation contains invalid fields")
        if row["blind_id"] in confirmations:
            raise ValueError("duplicate manual confirmation")
        confirmations[row["blind_id"]] = fields

    results = json.loads((parse_root / "parse-results.private.json").read_text("utf-8"))
    by_sample = {result["sample"]: result for result in results}
    for result in json.loads(overlay_path.read_text("utf-8")):
        if result["sample"] not in by_sample:
            raise ValueError("parse overlay contains an unknown sample")
        by_sample[result["sample"]] = result
    samples_root = parse_root / "fixtures" / "samples"
    parsed_by_hash: dict[str, dict[str, object]] = {}
    for sample, result in by_sample.items():
        digest = sha256_file(samples_root / sample)
        if digest in parsed_by_hash:
            raise ValueError("parse inputs contain duplicate content")
        parsed_by_hash[digest] = result

    final_rows: list[dict[str, str]] = []
    automated_slots = 0
    manual_slots = 0
    expected_confirmation_ids: set[str] = set()
    for row in rows:
        blind_id = row["blind_id"]
        manual_fields = {
            field
            for field in FIELDS
            if row[f"{field}_status"] == "needs_manual_review"
        }
        if manual_fields:
            expected_confirmation_ids.add(blind_id)
            if confirmations.get(blind_id) != manual_fields:
                raise ValueError("manual confirmation fields do not match prelabel review scope")
        elif blind_id in confirmations:
            raise ValueError("confirmation provided for a row without manual fields")

        parsed_result = parsed_by_hash.get(row["sha256"])
        parsed = parsed_result.get("parsed") if parsed_result else None
        blind_page = capture_root / args.blind_review_name / "pages" / f"{blind_id}.png"
        comparison_page = (
            capture_root
            / args.comparison_review_name
            / "pages"
            / f"{blind_id}.png"
        )
        output_row = dict(row)
        output_row["blind_review_page_sha256"] = ""
        output_row["comparison_review_page_sha256"] = ""
        if manual_fields:
            if not blind_page.is_file() or not comparison_page.is_file():
                raise ValueError("manual review evidence page is missing")
            output_row["blind_review_page_sha256"] = sha256_file(blind_page)
            output_row["comparison_review_page_sha256"] = sha256_file(comparison_page)

        for field in FIELDS:
            status_key = f"{field}_status"
            methods_key = f"{field}_methods"
            if row[status_key] == "high_confidence":
                if not normalize(field, row[field]):
                    raise ValueError("high-confidence prelabel is empty or invalid")
                output_row[field] = normalize(field, row[field])
                automated_slots += 1
                continue
            if row[status_key] != "needs_manual_review" or field not in manual_fields:
                raise ValueError("unexpected prelabel field status")
            if parsed is None:
                raise ValueError("visually confirmed field has no current product parse")
            value = normalize(field, parsed.get(field))
            if not value:
                raise ValueError("visually confirmed product field is empty or invalid")
            output_row[field] = value
            output_row[status_key] = "manual_visual_confirmed"
            output_row[methods_key] = (
                "product_output_blind_source_reading+second_stage_exact_visual_comparison"
            )
            manual_slots += 1
        final_rows.append(output_row)

    if set(confirmations) != expected_confirmation_ids:
        raise ValueError("confirmation row set does not match manual review row set")
    if automated_slots + manual_slots != 252 or manual_slots != 45:
        raise ValueError("final field-slot counts do not match the approved review scope")

    fieldnames = list(final_rows[0])
    with output_path.open("x", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames, delimiter="\t")
        writer.writeheader()
        writer.writerows(final_rows)

    metadata = {
        "verification": "private-core-field-ground-truth-finalization-v1",
        "invoice_rows": len(final_rows),
        "field_slots": automated_slots + manual_slots,
        "product_output_blind_high_confidence_slots": automated_slots,
        "manual_visual_confirmed_slots": manual_slots,
        "manual_review_invoices": len(expected_confirmation_ids),
        "prelabel_sha256": sha256_file(
            capture_root / "field-ground-truth-prelabel.private.tsv"
        ),
        "confirmation_sha256": sha256_file(confirmation_path),
        "parse_overlay_sha256": sha256_file(overlay_path),
        "final_truth_sha256": sha256_file(output_path),
        "private_values_logged": False,
    }
    metadata_path = capture_root / "field-ground-truth-finalization.private.json"
    metadata_path.write_text(
        json.dumps(metadata, ensure_ascii=False, indent=2) + "\n", "utf-8"
    )

    print("verification=private-core-field-ground-truth-finalization-v1")
    print(f"invoice_rows={len(final_rows)}")
    print(f"field_slots={automated_slots + manual_slots}")
    print(f"product_output_blind_high_confidence_slots={automated_slots}")
    print(f"manual_visual_confirmed_slots={manual_slots}")
    print(f"manual_review_invoices={len(expected_confirmation_ids)}")
    print(f"final_truth_sha256={metadata['final_truth_sha256']}")
    print("private_values_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
