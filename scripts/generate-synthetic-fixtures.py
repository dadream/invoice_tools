#!/usr/bin/env python3
"""Generate deterministic, privacy-safe binary fixtures for release validation."""

from __future__ import annotations

import io
import json
import zipfile
from pathlib import Path

from reportlab.lib.pagesizes import A4
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "fixtures" / "synthetic"
FONT = ROOT / "src-tauri" / "assets" / "fonts" / "SourceHanSansCN-VF.ttf"
FIXED_ZIP_TIME = (1980, 1, 1, 0, 0, 0)

INVOICE_NUMBER = "26112000000000000001"
ISSUE_DATE = "2026-06-18"
TOTAL_AMOUNT = "1200.00"
TAX_AMOUNT = "67.92"
TAX_RATE = "0.06"
BUYER = "北京示例科技有限公司"
SELLER = "上海演示商贸有限公司"


def write_bytes(name: str, data: bytes) -> None:
    path = OUTPUT / name
    if path.exists() and path.read_bytes() == data:
        return
    path.write_bytes(data)


def zip_bytes(entries: list[tuple[str, bytes]]) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, data in entries:
            info = zipfile.ZipInfo(name, FIXED_ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data, compresslevel=9)
    return buffer.getvalue()


def invoice_xml(marker: str) -> bytes:
    return (
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        "<Invoice>\n"
        f"  <SyntheticMarker>synthetic-only {marker}</SyntheticMarker>\n"
        f"  <InvoiceNumber>{INVOICE_NUMBER}</InvoiceNumber>\n"
        f"  <IssueDate>{ISSUE_DATE}</IssueDate>\n"
        f"  <TotalAmount>{TOTAL_AMOUNT}</TotalAmount>\n"
        f"  <TaxAmount>{TAX_AMOUNT}</TaxAmount>\n"
        f"  <TaxRate>{TAX_RATE}</TaxRate>\n"
        f"  <BuyerName>{BUYER}</BuyerName>\n"
        f"  <SellerName>{SELLER}</SellerName>\n"
        "</Invoice>\n"
    ).encode("utf-8")


def make_text_pdf() -> bytes:
    if not FONT.is_file():
        raise FileNotFoundError(f"bundled font is missing: {FONT}")
    pdfmetrics.registerFont(TTFont("SyntheticSourceHan", str(FONT)))
    buffer = io.BytesIO()
    document = canvas.Canvas(
        buffer,
        pagesize=A4,
        pageCompression=1,
        invariant=1,
    )
    document.setTitle("Synthetic VAT Invoice Fixture")
    document.setAuthor("synthetic-only")
    width, height = A4
    document.setStrokeColorRGB(0.15, 0.25, 0.42)
    document.setLineWidth(1.2)
    document.rect(40, 70, width - 80, height - 120)
    document.setFont("SyntheticSourceHan", 20)
    document.drawCentredString(width / 2, height - 95, "增值税电子普通发票（合成测试）")
    document.setFont("SyntheticSourceHan", 9)
    document.drawString(58, height - 120, "synthetic-only · 不对应任何真实个人、公司或票据")
    document.line(52, height - 132, width - 52, height - 132)

    rows = [
        ("发票号码", INVOICE_NUMBER),
        ("开票日期", "2026年06月18日"),
        ("购买方名称", BUYER),
        ("销售方名称", SELLER),
        ("税率", "6%"),
        ("税额", f"¥{TAX_AMOUNT}"),
        ("价税合计", f"¥{TOTAL_AMOUNT}"),
    ]
    y = height - 170
    for label, value in rows:
        document.setFont("SyntheticSourceHan", 11)
        document.drawString(65, y, label)
        document.drawString(175, y, value)
        document.setStrokeColorRGB(0.78, 0.82, 0.88)
        document.line(60, y - 8, width - 60, y - 8)
        y -= 48

    document.setFont("SyntheticSourceHan", 9)
    document.setFillColorRGB(0.35, 0.39, 0.46)
    document.drawString(58, 88, "用途：文本层 PDF 解析、字段定位与发布门禁黄金样本")
    document.showPage()
    document.save()
    return buffer.getvalue()


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)

    write_bytes("vat-invoice-text.pdf", make_text_pdf())
    write_bytes(
        "vat-invoice.ofd",
        zip_bytes(
            [
                (
                    "OFD.xml",
                    b'<?xml version="1.0" encoding="UTF-8"?>\n'
                    b'<OFD SyntheticMarker="synthetic-only"/>\n',
                ),
                ("Doc_0/Attachments/invoice.xml", invoice_xml("ofd-embedded-xml")),
            ]
        ),
    )
    write_bytes(
        "malformed.pdf",
        b"%PDF-1.7\n% synthetic-only deliberately truncated PDF fixture\n"
        b"1 0 obj\n<< /Type /Catalog >>\n",
    )
    write_bytes(
        "malformed.ofd",
        b"synthetic-only deliberately invalid OFD/ZIP fixture\n",
    )

    duplicate = invoice_xml("duplicate-content-golden")
    write_bytes("duplicate-a.xml", duplicate)
    write_bytes("duplicate-b.xml", duplicate)

    oversized_payload = b"<!-- synthetic-only expanded ZIP resource-limit fixture -->\n"
    oversized_payload += b"0" * (26 * 1024 * 1024 - len(oversized_payload))
    write_bytes(
        "expanded-over-limit.zip",
        zip_bytes([("synthetic-only-oversized.xml", oversized_payload)]),
    )

    expected_errors = {
        "syntheticMarker": "synthetic-only",
        "cases": [
            {
                "path": "malformed.pdf",
                "component": "invoice-parse",
                "expected": "MalformedFormat",
            },
            {
                "path": "malformed.ofd",
                "component": "invoice-parse",
                "expected": "MalformedFormat",
            },
            {
                "path": "expanded-over-limit.zip",
                "component": "invoice-collect",
                "expected": "rejected-empty-result",
            },
            {
                "path": "duplicate-b.xml",
                "component": "invoice-collect",
                "expected": "duplicate-content",
                "sameAs": "duplicate-a.xml",
            },
        ],
    }
    write_bytes(
        "expected-errors.json",
        (json.dumps(expected_errors, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
    )


if __name__ == "__main__":
    main()
