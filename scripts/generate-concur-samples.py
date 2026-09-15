"""Build the deliberately synthetic Concur acceptance kit (no real invoices)."""
import hashlib
import json
from pathlib import Path
from reportlab.pdfgen import canvas
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from pypdf import PdfReader

root = Path(__file__).resolve().parent.parent / "sidecars/concur-browser/samples"
root.mkdir(parents=True, exist_ok=True)
pdfmetrics.registerFont(TTFont("STSong-Light", str(Path(__file__).resolve().parent.parent / "src-tauri/assets/fonts/SourceHanSansCN-VF.ttf")))
documents = [
    ("local-invoice.pdf", "本地交通费用 - 发票测试样例", "示例城市交通服务商", "2026-06-02", "1.11"),
    ("rail-invoice.pdf", "铁路费用 - 发票测试样例", "示例铁路服务商", "2026-06-15", "2.22"),
    ("itinerary.pdf", "铁路费用 - 配套行程单样例", "北京南 → 上海虹桥（虚构行程）", "2026-06-15", "2.22"),
    ("hotel-invoice.pdf", "住宿费用 - 发票测试样例", "示例酒店", "2026-06-16", "3.33"),
    ("water-bill.pdf", "住宿费用 - 配套水单样例", "示例酒店 / 入住 06-15，离店 06-16", "2026-06-16", "3.33"),
    ("details.pdf", "住宿费用 - 配套明细样例", "示例房费一晚（仅验证附件追加）", "2026-06-16", "3.33"),
]
for filename, title, vendor, date, amount in documents:
    path = root / filename
    pdf = canvas.Canvas(str(path), pagesize=(595, 842), invariant=1)
    pdf.setTitle(title)
    pdf.setFillColorRGB(.07, .42, .32)
    pdf.rect(0, 716, 595, 126, fill=1, stroke=0)
    pdf.setFillColorRGB(1, 1, 1)
    pdf.setFont("STSong-Light", 24)
    pdf.drawString(40, 785, "测试样例 · 请勿报销")
    pdf.setFont("Helvetica", 12)
    pdf.drawString(40, 753, "SYNTHETIC TEST DATA - DO NOT SUBMIT")
    pdf.setFillColorRGB(.1, .15, .18)
    pdf.setFont("STSong-Light", 20)
    pdf.drawString(40, 666, title)
    pdf.setFont("STSong-Light", 14)
    for y, text in [(612, "用途：验证 Concur 草稿、费用表单和附件逐份追加"),
                    (562, f"示例交易方：{vendor}"), (520, f"实际日期：{date}"),
                    (478, f"样例金额：CNY {amount}"), (416, "说明：配套材料不另行计入费用，不是真实税务发票。"),
                    (374, "所有名称、行程、金额均为虚构测试内容。"),
                    (300, "完成后请在 Concur 检查每份文件仍存在，再删除测试草稿。")]:
        pdf.drawString(40, y, text)
    pdf.setFont("Helvetica", 10)
    pdf.drawString(40, 70, filename)
    pdf.drawRightString(555, 70, "TEST KIT / 1")
    pdf.save()
    reader = PdfReader(path)
    assert len(reader.pages) == 1
    assert "SYNTHETIC TEST DATA" in reader.pages[0].extract_text()

def doc(number, name, role):
    return {"id": number, "role": role, "path": name, "name": name,
            "sha256": hashlib.sha256((root / name).read_bytes()).hexdigest()}

expenses = [
    (1, 1, "样例本地费用", "local_month", "city_transport", "2026-06-02", "示例城市交通服务商", "北京", "北京", "1.11", [doc(1, "local-invoice.pdf", "main_invoice")]),
    (2, 2, "样例上海出差", "business_trip", "rail", "2026-06-15", "示例铁路服务商", "北京", "北京", "2.22", [doc(2, "rail-invoice.pdf", "main_invoice"), doc(3, "itinerary.pdf", "itinerary")]),
    (3, 2, "样例上海出差", "business_trip", "hotel", "2026-06-16", "示例酒店", "上海", "上海", "3.33", [doc(4, "hotel-invoice.pdf", "main_invoice"), doc(5, "water-bill.pdf", "supporting"), doc(6, "details.pdf", "detail")]),
]
snapshot = {"batch_id": 0, "snapshot_id": 0, "fingerprint": "synthetic-concur-kit-v1", "name": "Concur功能验证", "expenses": []}
for number, group, title, kind, category, date, vendor, city, province, amount, docs in expenses:
    snapshot["expenses"].append({"id": number, "group_id": group, "group_title": title, "group_kind": kind,
        "fields": {"category_code": category, "transaction_date": date, "description": "虚构测试费用-请勿提交报销", "counterparty_name": vendor,
                   "city_name": city, "province_name": province, "gross_amount": amount, "currency_code": "CNY", "payment_method": "", "tax_amount": ""},
        "documents": docs})
(root / "sample.json").write_text(json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"Validated {len(documents)} synthetic PDFs and 3 sample expenses (CNY 6.66)")
