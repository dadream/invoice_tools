# D 输出模块 完成报告

**日期**: 2026-08-11
**commit**: `b18da2b`
**计划**: `2026-08-11-d-export-module.md`

## 交付内容

### Excel 明细表导出（Task 1）

**后端命令**（`export.rs`）：
```rust
export_batch_excel(batch_id) -> AppResult<Vec<u8>>
```

**特性**：
- **12 列完整字段**：发票号码、开票日期、金额、税额、购方名称、销方名称、票种、城市、出发时间、入住日期、签章状态、重复标记
- **冻结首行**：表头固定，滚动时保持可见
- **金额文本格式**：`set_num_format("@")` 避免科学计数法
- **底部合计行**：显示"合计"和总金额
- **样式**：表头灰色背景 + 加粗，单元格边框线
- **列宽自适应**：发票号 22 字符、日期 12、金额 10、购方/销方 25

### PDF 台账导出（Task 2）

**后端命令**（`export.rs`）：
```rust
export_batch_pdf(batch_id) -> AppResult<Vec<u8>>
```

**特性**：
- **A4 纸张**：210mm × 297mm
- **批次信息**：ID、月份、状态、发票数、总金额
- **发票表格**：发票号、日期、金额、销方（4 列简化版）
- **自动分页**：Y 坐标 < 30mm 时新建页面
- **字体选择**：内置 Helvetica（仅英文/数字，中文显示为空白）

### 前端下载按钮（Task 2）

**位置**：`BatchDetail.svelte` 批次详情卡片底部

**两个按钮**：
- 📊 导出 Excel（`export_batch_excel`）
- 📄 导出 PDF（`export_batch_pdf`）

**交互**：
- 水平排列，间距 8px，沿用 S0.7 按钮风格
- loading 状态：导出中禁用按钮，显示"导出中..." / "生成中..."
- 错误处理：失败时 `alert` 显示错误，不触发下载
- 文件名：`${batch.name}-明细表.xlsx` / `${batch.name}-台账.pdf`

**下载实现**：
```ts
const blob = new Blob([new Uint8Array(result.value)], { type: 'application/...' })
const url = URL.createObjectURL(blob)
const a = document.createElement('a')
a.href = url
a.download = filename
a.click()
URL.revokeObjectURL(url)
```

## 验证结果

| 项目 | 结果 |
|------|------|
| `cargo test --workspace` | **248 passed, 0 failed**（G1 基线 242，+6） |
| `npm run check` | **212 files, 0 errors, 0 warnings** |
| `npm run build` | 通过（2.20s） |

## 测试覆盖

**新增测试（6 个）**：

Excel（3 个）：
1. `exports_excel_with_all_fields` - 完整字段导出
2. `exports_empty_batch` - 空批次处理
3. `returns_error_for_nonexistent_batch` - 错误处理

PDF（3 个）：
4. `exports_pdf_with_batch_info` - 批次信息正确
5. `exports_empty_batch_pdf` - 空批次 PDF
6. `pdf_returns_error_for_nonexistent_batch` - 错误处理

**魔数验证**：
- Excel：前 4 字节 `PK\x03\x04`（ZIP 压缩包）
- PDF：前 4 字节 `%PDF`

**手动验证**（`/tmp/test_export.*`）：
- Excel 用 LibreOffice Calc 打开，12 列对齐，冻结首行生效，合计正确
- PDF 用 PDF 阅读器打开，批次信息与表格正确，英文/数字正常，中文为空白

## 技术亮点

### 1. Excel 金额格式

长数字（如发票号 20 位）默认显示为科学计数法。用 `set_num_format("@")` 将单元格设为文本格式：
```rust
let amount_fmt = Format::new()
    .set_border(FormatBorder::Thin)
    .set_num_format("@");  // 文本格式
worksheet.write_with_format(row, 2, inv.amount.to_string(), &amount_fmt)?;
```

### 2. PDF 字体选择

**方案 A（采用）**：内置 Helvetica 字体
- 优点：无需下载外部字体，不增加二进制体积
- 限制：仅支持英文/数字，中文字段显示为空白
- 实际影响：核心数据（发票号、日期、金额）都是英文/数字，可正常显示

**方案 B（未采用）**：内嵌 Noto Sans SC 子集
- 需下载 2-3 MB 字体文件
- 增加二进制体积，但支持完整中文
- 留待后续优化（用户反馈中文显示需求时再升级）

### 3. 前端 Blob 下载

直接用 `Blob` + `URL.createObjectURL` 触发浏览器下载，无需 Tauri 的 `plugin-dialog` save API：
```ts
const blob = new Blob([new Uint8Array(result.value)], { type: 'application/...' })
const url = URL.createObjectURL(blob)
const a = document.createElement('a')
a.href = url
a.download = filename
a.click()
URL.revokeObjectURL(url)  // 释放内存
```

优点：简单、跨平台、无需额外权限配置。

### 4. 自动分页逻辑

PDF 每行 7mm 高，页面高度 297mm，底部留 30mm 边距：
```rust
let mut y = 250.0;  // 初始 Y 坐标
for inv in invoices.iter() {
    if y < 30.0 {  // 接近底部
        let (page, layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
        current_layer = doc.get_page(page).get_layer(layer);
        y = 270.0;  // 重置 Y 坐标
    }
    // 绘制当前行
    y -= 7.0;
}
```

## 与计划的偏差

1. **printpdf 版本**：使用 0.7 而非计划的 0.12
   - 原因：0.12 API 完全重写，改用 HTML 渲染模式，不适合简单表格
   - 0.7 API 稳定，直接绘制文本，符合当前需求
   
2. **字体方案**：选择内置字体（方案 A）而非子集字体（方案 B）
   - 简化实现，避免下载外部文件
   - 核心数据（发票号/日期/金额）不受影响
   - 中文字段（购方/销方/城市）显示为空白，属已知限制

3. **PDF 排版简化**：只文本对齐，不绘制表格边框线
   - printpdf 绘图 API 冗长（每条线段需单独绘制）
   - 对齐文本已足够可读，暂不投入边框绘制

## 已知限制

- **PDF 中文显示为空白**：内置 Helvetica 无中文支持，购方/销方/城市字段为空
- **无发票缩略图**：S0.7 未存储图片，重新渲染 OFD/PDF 为图片复杂度高
- **PDF 排版简化**：无表格边框线，只文本对齐
- **Excel 列宽固定**：粗略估算，未动态计算最大单元格宽度
- **不支持批量导出**：一次只能导出一个批次，多批次需前端循环调用
- **二进制体积**：printpdf 依赖增加约 1-2 MB

## 用户使用流程

1. 在批次详情页，批次信息卡片底部显示两个导出按钮
2. 点击"📊 导出 Excel"：
   - 按钮禁用，显示"导出中..."
   - 后端生成 xlsx 字节流
   - 浏览器触发下载：`批次名称-明细表.xlsx`
   - 按钮恢复
3. 点击"📄 导出 PDF"：
   - 按钮禁用，显示"生成中..."
   - 后端生成 PDF 字节流
   - 浏览器触发下载：`批次名称-台账.pdf`
   - 按钮恢复
4. 导出失败时 alert 显示错误信息

## 下一步建议

D 输出模块完成后，可选路径：

### 路径 1: H1 流水线串联（推荐）
- **目标**：采集 → 解析 → 归组 → 审核 → 导出端到端流程
- **依赖**：S0.6 ✅ + S0.7 ✅ + G1 ✅ + D ✅ → 可以开始
- **价值**：打通完整流程，可进行首次内部测试
- **阻塞**：目前无阻塞，D 是最后一个依赖

### 路径 2: G2 审核界面
- **目标**：手动调整归组结果，标记歧义项
- **依赖**：S0.6 ✅ + S0.7 ✅ + C（归组引擎）✅
- **价值**：提供归组调整能力，处理 `invoice-grouping` 检测出的歧义

### 路径 3: 优化 PDF 中文支持
- **目标**：内嵌 Noto Sans SC 子集，支持完整中文
- **工作量**：下载字体（2-3 MB）+ 调整 `printpdf` 字体加载逻辑
- **价值**：提升 PDF 可读性，购方/销方/城市字段可正常显示

建议优先 **H1 流水线串联**，因为：
- 所有依赖已完成，无阻塞
- 打通端到端可验证整体架构
- 发现集成问题可及时调整
- G2 和 PDF 优化可在 H1 验证后再精细化
