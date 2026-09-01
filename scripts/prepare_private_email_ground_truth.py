from __future__ import annotations

import argparse
import csv
import hashlib
import html
import re
import sys
from collections import Counter, defaultdict
from email import policy
from email.parser import BytesParser
from pathlib import Path


INVOICE_TERMS = re.compile(r"发票|invoice", re.IGNORECASE)
LINK_TERMS = re.compile(r"https?://|下载|查看|获取|二维码|小程序", re.IGNORECASE)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build product-output-blind private email-level prelabels."
    )
    parser.add_argument("capture_root", type=Path)
    return parser.parse_args()


def visible_message_text(path: Path) -> tuple[str, str]:
    message = BytesParser(policy=policy.default).parsebytes(path.read_bytes())
    subject = str(message.get("subject", ""))
    parts: list[str] = []
    for part in message.walk():
        if part.is_multipart() or part.get_filename():
            continue
        if part.get_content_disposition() == "attachment":
            continue
        content_type = part.get_content_type()
        if content_type not in {"text/plain", "text/html"}:
            continue
        try:
            content = str(part.get_content())
        except Exception:
            continue
        if content_type == "text/html":
            content = re.sub(r"(?is)<(script|style).*?>.*?</\1>", " ", content)
            content = re.sub(r"(?s)<[^>]+>", " ", content)
            content = html.unescape(content)
        parts.append(content)
    compact = re.sub(r"\s+", " ", "\n".join(parts)).strip()
    return subject, compact


def main() -> int:
    args = parse_args()
    capture_root = args.capture_root.resolve(strict=True)
    repo_root = Path(__file__).resolve().parent.parent
    if capture_root == repo_root or repo_root in capture_root.parents:
        raise ValueError("private capture root must remain outside the Git repository")

    with (capture_root / "ground-truth-final.private.tsv").open(
        "r", encoding="utf-8", newline=""
    ) as stream:
        attachment_truth = list(csv.DictReader(stream, delimiter="\t"))
    labels_by_email: dict[str, list[str]] = defaultdict(list)
    for row in attachment_truth:
        if row["email_file"]:
            labels_by_email[row["email_file"]].append(row["human_label"])

    emails_root = capture_root / "emails"
    email_paths = sorted(path for path in emails_root.iterdir() if path.suffix.lower() == ".eml")
    if not email_paths:
        raise ValueError("private capture contains no email files")

    rows: list[dict[str, str | int]] = []
    counts: Counter[str] = Counter()
    for index, email_path in enumerate(email_paths, start=1):
        labels = labels_by_email[email_path.name]
        label_counts = Counter(labels)
        subject, body = visible_message_text(email_path)
        visible = f"{subject}\n{body}"
        has_invoice_terms = bool(INVOICE_TERMS.search(visible))
        has_link_terms = bool(LINK_TERMS.search(visible))

        if label_counts["valid_invoice"]:
            suggested = "invoice_attachment"
            reason = "contains_blind_labeled_valid_invoice"
            review = "false"
        elif label_counts["supporting_document"]:
            suggested = "supporting_attachment"
            reason = "contains_blind_labeled_supporting_document"
            review = "false"
        else:
            suggested = "needs_visual_review"
            review = "true"
            if has_invoice_terms and has_link_terms:
                reason = "invoice_and_link_terms_without_valid_attachment"
            elif has_invoice_terms:
                reason = "invoice_terms_without_valid_attachment"
            else:
                reason = "no_invoice_terms_or_valid_attachment"

        counts[suggested] += 1
        rows.append(
            {
                "blind_email_id": f"E{index:03}",
                "email_file": email_path.name,
                "sha256": hashlib.sha256(email_path.read_bytes()).hexdigest(),
                "logical_attachments": len(labels),
                "valid_invoice_attachments": label_counts["valid_invoice"],
                "supporting_attachments": label_counts["supporting_document"],
                "invalid_or_corrupt_attachments": label_counts["not_invoice"]
                + label_counts["corrupt_or_empty"],
                "has_invoice_terms": str(has_invoice_terms).lower(),
                "has_link_terms": str(has_link_terms).lower(),
                "suggested_label": suggested,
                "suggestion_reason": reason,
                "requires_visual_review": review,
                "human_label": "",
                "human_notes": "",
            }
        )

    output = capture_root / "email-ground-truth-prelabel.private.tsv"
    with output.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    print("verification=product-output-blind-private-email-prelabel-v1")
    print(f"emails={len(rows)}")
    for label in sorted(counts):
        print(f"suggested_{label}={counts[label]}")
    print("product_predictions_read=false")
    print("private_message_text_logged=false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
