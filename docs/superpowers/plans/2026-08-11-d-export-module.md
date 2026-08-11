# D 输出模块 Implementation Plan

**日期**: 2026-08-11
**任务**: D 输出模块（批次导出 PDF 台账 + Excel 明细表）
**前置**: S0.7 发票添加流程 + G1 校验去重（已完成）

## 目标

为批次提供两种导出格式：
1. **PDF 台账**：A4 排版，包含批次汇总信息 + 发票列表（表格形式，无缩略图）
2. **Excel 明细表**：包含所有发票字段的电子表格，可直接编辑或导入其他系统

## 技术选型

根据 Rust 生态和项目约束：

| 库 | 版本 | 用途 | 理由 |
|---|---|---|---|
| `rust_xlsxwriter` | 0.97.1 | Excel 生成 | 纯 Rust，API 清晰，支持样式/公式/冻结窗格 |
| `printpdf` | 0.12.5 | PDF 生成 | 纯 Rust，支持中文（需内嵌字体），轻量无 C 依赖 |

**不使用**：
- `lopdf`（低级 API，需手动计算坐标）
- 图片缩略图（S0.7 未存储发票图片路径，只存 `file_path` 指向原始文件；
  生成缩略图需重新渲染 PDF/OFD，复杂度高且体积大，留待后续优化）

## 已验证的现有能力

### 1. 数据源（invoice-store）

```rust
// crates/invoice-store/src/ledger_db.rs:148
pub fn get_batch(&self, id: i64) -> StoreResult<Batch>

// ledger_db.rs:280
pub fn list_invoices_by_batch(&self, batch_id: i64) -> StoreResult<Vec<ReportedInvoice>>
```

`Batch`（`models.rs:61`）字段：
- `id, name, month, status, total_amount, invoice_count, created_at, updated_at,
   submitted_at, approved_at, completed_at, rejected_at`

`ReportedInvoice`（`models.rs:147`）字段：
- `id, batch_id, invoice_number, issue_date, amount, tax_amount, buyer_name,
   seller_name, ticket_type, city, departure_time, checkin_date, file_path,
   verification_result, is_duplicate, duplicate_reason, created_at, updated_at`

### 2. 前端文件下载（Tauri v2）

Tauri 命令返回字节数组，前端用 `Blob` + `URL.createObjectURL` 触发下载：

```ts
const result = await invokeSafe<number[]>('export_batch_pdf', { batchId })
if (result.ok) {
  const blob = new Blob([new Uint8Array(result.value)], { type: 'application/pdf' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `批次-${batchName}.pdf`
  a.click()
  URL.revokeObjectURL(url)
}
```

同样适用于 Excel（MIME 类型改为 `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`）。

## Global Constraints

- **中文支持**：PDF 必须内嵌中文字体（开源字体如 Noto Sans SC），否则中文显示为空白
- **金额精度**：全程 `Decimal`，Excel 单元格格式设为文本避免科学计数法
- **文件大小**：内嵌字体会增加 PDF 体积（约 2-5 MB），属正常；Excel 纯文本体积小
- **错误处理**：导出失败返回 `AppError`，前端显示具体错误而非空白下载
- **无 GUI 验收**：WSL2 无 display server，生成文件写到 `/tmp` 人工检查内容
- **不存储导出文件**：直接返回字节流，不写 `~/.invoice-assistant/exports/`

## 任务拆分

两个任务顺序执行，每个任务一个 subagent。

---

## Task 1: Excel 明细表导出

**文件**: `src-tauri/src/commands/export.rs`（新建）、
`src-tauri/src/commands/mod.rs`、`src-tauri/src/main.rs`、
`src-tauri/Cargo.toml`

### Step 1: 添加依赖

`src-tauri/Cargo.toml`：
```toml
rust_xlsxwriter = "0.97"
```

### Step 2: 实现 `export_batch_excel` 命令

```rust
use rust_xlsxwriter::{Workbook, Worksheet, Format, Color};

#[tauri::command]
pub fn export_batch_excel(
    state: tauri::State<Mutex<AppState>>,
    batch_id: i64,
) -> AppResult<Vec<u8>>
{
    let db = state.lock().unwrap().ledger_db();
    let batch = db.get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let invoices = db.list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    // 样式
    let header_fmt = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xD9D9D9))
        .set_border(FormatBorder::Thin);
    let cell_fmt = Format::new()
        .set_border(FormatBorder::Thin);
    let amount_fmt = Format::new()  // 金额列设为文本格式
        .set_border(FormatBorder::Thin)
        .set_num_format("@");

    // 表头
    worksheet.write_with_format(0, 0, "发票号码", &header_fmt)?;
    worksheet.write_with_format(0, 1, "开票日期", &header_fmt)?;
    worksheet.write_with_format(0, 2, "金额", &header_fmt)?;
    worksheet.write_with_format(0, 3, "税额", &header_fmt)?;
    worksheet.write_with_format(0, 4, "购方名称", &header_fmt)?;
    worksheet.write_with_format(0, 5, "销方名称", &header_fmt)?;
    worksheet.write_with_format(0, 6, "票种", &header_fmt)?;
    worksheet.write_with_format(0, 7, "城市", &header_fmt)?;
    worksheet.write_with_format(0, 8, "出发时间", &header_fmt)?;
    worksheet.write_with_format(0, 9, "入住日期", &header_fmt)?;
    worksheet.write_with_format(0, 10, "签章状态", &header_fmt)?;
    worksheet.write_with_format(0, 11, "重复标记", &header_fmt)?;

    // 冻结首行
    worksheet.freeze_panes(1, 0)?;

    // 数据行
    for (idx, inv) in invoices.iter().enumerate() {
        let row = (idx + 1) as u32;
        worksheet.write_with_format(row, 0, &inv.invoice_number, &cell_fmt)?;
        worksheet.write_with_format(row, 1, inv.issue_date.format("%Y-%m-%d").to_string(), &cell_fmt)?;
        worksheet.write_with_format(row, 2, inv.amount.to_string(), &amount_fmt)?;
        worksheet.write_with_format(row, 3, inv.tax_amount.as_ref().map(|d| d.to_string()).unwrap_or_default(), &amount_fmt)?;
        worksheet.write_with_format(row, 4, inv.buyer_name.as_deref().unwrap_or(""), &cell_fmt)?;
        worksheet.write_with_format(row, 5, inv.seller_name.as_deref().unwrap_or(""), &cell_fmt)?;
        worksheet.write_with_format(row, 6, inv.ticket_type.to_str(), &cell_fmt)?;
        worksheet.write_with_format(row, 7, inv.city.as_deref().unwrap_or(""), &cell_fmt)?;
        worksheet.write_with_format(row, 8, inv.departure_time.map(|dt| dt.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(), &cell_fmt)?;
        worksheet.write_with_format(row, 9, inv.checkin_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), &cell_fmt)?;
        worksheet.write_with_format(row, 10, inv.verification_result.as_deref().unwrap_or(""), &cell_fmt)?;
        worksheet.write_with_format(row, 11, if inv.is_duplicate { "是" } else { "" }, &cell_fmt)?;
    }

    // 自适应列宽（粗略估算）
    worksheet.set_column_width(0, 22)?;  // 发票号码
    worksheet.set_column_width(1, 12)?;  // 日期
    worksheet.set_column_width(2, 10)?;  // 金额
    worksheet.set_column_width(4, 25)?;  // 购方
    worksheet.set_column_width(5, 25)?;  // 销方

    // 底部合计行
    let total_row = (invoices.len() + 1) as u32;
    worksheet.write_with_format(total_row, 0, "合计", &header_fmt)?;
    let total: Decimal = invoices.iter().map(|inv| inv.amount).sum();
    worksheet.write_with_format(total_row, 2, total.to_string(), &amount_fmt)?;

    // 序列化为字节
    let mut buf = Vec::new();
    workbook.save_to_buffer(&mut buf)
        .map_err(|e| AppError::internal(format!("Excel 生成失败: {}", e)))?;

    Ok(buf)
}
```

### Step 3: 注册命令

`commands/mod.rs` 加 `pub mod export;`，`main.rs` 的 `generate_handler!` 追加 `export::export_batch_excel`。

### Step 4: 测试

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn exports_excel_with_all_fields() {
        // 创建临时 DB，插入批次 + 2 张发票
        // 调用 export_batch_excel
        // 断言返回 Vec<u8> 非空，前 4 字节是 ZIP 魔数 PK\x03\x04
        // （xlsx 本质是 ZIP 压缩包）
    }
}
```

验证：
```bash
source scripts/tauri-env.sh
cargo test -p invoice-assistant export_batch_excel
# 手动验证：写文件到 /tmp/test.xlsx，用 LibreOffice 打开检查中文显示
```

---

## Task 2: PDF 台账导出 + 前端下载按钮

**文件**: `src-tauri/src/commands/export.rs`（扩展）、
`ui/src/routes/batches/BatchDetail.svelte`（改）、
`ui/src/lib/types.ts`（可选，加导出相关辅助函数）

### Step 1: 添加依赖与字体

`src-tauri/Cargo.toml`：
```toml
printpdf = "0.12"
```

下载开源中文字体（Noto Sans SC Regular）：
```bash
mkdir -p src-tauri/assets/fonts
curl -L -o src-tauri/assets/fonts/NotoSansSC-Regular.ttf \
  'https://github.com/notofonts/noto-cjk/raw/main/Sans/OTF/SimplifiedChinese/NotoSansSC-Regular.otf'
```

字体文件约 8 MB，内嵌到二进制会增加体积。替代方案：打包时放到 `resources/` 目录，
运行时从 `tauri::api::path::resource_dir()` 读取。Task 2 采用内嵌方案（简单但体积大）。

### Step 2: 实现 `export_batch_pdf` 命令

```rust
use printpdf::*;

const FONT_DATA: &[u8] = include_bytes!("../assets/fonts/NotoSansSC-Regular.ttf");

#[tauri::command]
pub fn export_batch_pdf(
    state: tauri::State<Mutex<AppState>>,
    batch_id: i64,
) -> AppResult<Vec<u8>>
{
    let db = state.lock().unwrap().ledger_db();
    let batch = db.get_batch(batch_id)?;
    let invoices = db.list_invoices_by_batch(batch_id)?;

    // A4 纸张 (210mm x 297mm)
    let (doc, page1, layer1) = PdfDocument::new(&batch.name, Mm(210.0), Mm(297.0), "Layer 1");
    let font = doc.add_external_font(FONT_DATA)
        .map_err(|e| AppError::internal(format!("字体加载失败: {:?}", e)))?;
    let current_layer = doc.get_page(page1).get_layer(layer1);

    // 标题
    current_layer.use_text(&batch.name, 18.0, Mm(20.0), Mm(270.0), &font);

    // 批次信息
    let info = format!(
        "批次月份: {}  状态: {}  发票数: {}  总金额: ¥{}",
        batch.month,
        batch.status.as_str(),
        batch.invoice_count,
        batch.total_amount
    );
    current_layer.use_text(&info, 10.0, Mm(20.0), Mm(260.0), &font);

    // 表格表头（简化版，不画边框线，只文本）
    let mut y = 250.0;
    current_layer.use_text("发票号码", 9.0, Mm(20.0), Mm(y), &font);
    current_layer.use_text("日期", 9.0, Mm(80.0), Mm(y), &font);
    current_layer.use_text("金额", 9.0, Mm(110.0), Mm(y), &font);
    current_layer.use_text("销方", 9.0, Mm(140.0), Mm(y), &font);

    y -= 10.0;

    // 表格数据（每行 10mm 高）
    for inv in invoices.iter() {
        if y < 30.0 {  // 接近底部，需新页
            let (page, layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            current_layer = doc.get_page(page).get_layer(layer);
            y = 270.0;
        }

        current_layer.use_text(&inv.invoice_number, 8.0, Mm(20.0), Mm(y), &font);
        current_layer.use_text(&inv.issue_date.format("%Y-%m-%d").to_string(), 8.0, Mm(80.0), Mm(y), &font);
        current_layer.use_text(&format!("¥{}", inv.amount), 8.0, Mm(110.0), Mm(y), &font);
        current_layer.use_text(inv.seller_name.as_deref().unwrap_or(""), 8.0, Mm(140.0), Mm(y), &font);

        y -= 7.0;
    }

    // 序列化
    let mut buf = Vec::new();
    doc.save(&mut buf)
        .map_err(|e| AppError::internal(format!("PDF 生成失败: {:?}", e)))?;

    Ok(buf)
}
```

注册命令同 Task 1。

### Step 3: 前端下载按钮

`BatchDetail.svelte` 在批次详情卡片底部新增两个按钮：

```svelte
<div class="export-actions">
  <button onclick={exportExcel} disabled={exporting}>
    {exporting ? '导出中...' : '📊 导出 Excel'}
  </button>
  <button onclick={exportPdf} disabled={exporting}>
    {exporting ? '生成中...' : '📄 导出 PDF'}
  </button>
</div>

<script>
let exporting = $state(false)

async function exportExcel() {
  exporting = true
  const result = await invokeSafe<number[]>('export_batch_excel', { batchId })
  exporting = false

  if (!result.ok) {
    alert(`导出失败: ${describeError(result.error)}`)
    return
  }

  const blob = new Blob([new Uint8Array(result.value)], {
    type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet'
  })
  downloadBlob(blob, `${batch.name}-明细表.xlsx`)
}

async function exportPdf() {
  exporting = true
  const result = await invokeSafe<number[]>('export_batch_pdf', { batchId })
  exporting = false

  if (!result.ok) {
    alert(`生成失败: ${describeError(result.error)}`)
    return
  }

  const blob = new Blob([new Uint8Array(result.value)], { type: 'application/pdf' })
  downloadBlob(blob, `${batch.name}-台账.pdf`)
}

function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.click()
  URL.revokeObjectURL(url)
}
</script>
```

CSS 沿用 S0.7 按钮风格，两按钮水平排列，间距 8px。

### Step 4: 测试

后端：
```rust
#[test]
fn exports_pdf_with_chinese() {
    // 类似 Excel 测试，断言前 4 字节是 PDF 魔数 %PDF
    // 手动验证：写到 /tmp/test.pdf，用 PDF 阅读器打开检查中文显示
}
```

前端：
```bash
cd ui && npm run check && npm run build
```

---

## 验收标准

- [ ] `export_batch_excel` 命令返回有效 xlsx 字节流
- [ ] Excel 包含 12 列（发票号~重复标记）+ 冻结首行 + 底部合计
- [ ] 金额列为文本格式，无科学计数法
- [ ] `export_batch_pdf` 命令返回有效 PDF 字节流
- [ ] PDF 中文正常显示（内嵌 Noto Sans SC 字体）
- [ ] PDF 包含批次信息 + 发票表格（发票号/日期/金额/销方）
- [ ] 前端两个导出按钮，调用成功触发浏览器下载
- [ ] 导出中按钮禁用，显示"导出中..."
- [ ] 导出失败显示错误提示（不触发下载）
- [ ] `cargo test -p invoice-assistant` 全绿
- [ ] `npm run check` 0 errors / 0 warnings

## 已知限制

- **无发票缩略图**：PDF 只有文字表格，不包含发票图片（S0.7 未存储图片，
  重新渲染 OFD/PDF 为图片复杂度高）
- **PDF 排版简化**：不绘制表格边框线，只文本对齐（printpdf 绘图 API 冗长）
- **字体内嵌增加体积**：Noto Sans SC ~8 MB，二进制体积从 ~10 MB 增至 ~18 MB
- **单页容量有限**：PDF 每页约 30 行发票，超出自动分页（A4 高度 297mm）
- **Excel 列宽固定**：粗略估算，未动态计算最大单元格宽度
- **不支持批量导出**：一次只能导出一个批次，多批次需前端循环调用

## 下一步

D 输出模块完成后，可开始 **H1 流水线串联**（采集 → 解析 → 归组 → 审核 → 导出），
打通端到端流程。或者先做 **G2 审核界面**（手动调整归组）。
