# 合成测试夹具清单与隐私规则

> 状态：当前工作树只允许清单登记、哈希固定且明确标注的合成夹具。

机器可读清单为 `fixtures/inventory.json`，由 `scripts/scan-private-fixtures.ps1 -SelfTest` 校验路径、文件类型、大小、SHA-256、合成标记和清单覆盖率。任何未登记文件、重解析点、哈希漂移或 `fixtures/test-images`/`fixtures/samples` 回流都会使 Windows 质量门禁失败。

## 当前夹具层级

| 层级 | 路径 | 用途 |
|---|---|---|
| 结构化 | `fixtures/synthetic/vat-invoice.xml` | XML 核心字段黄金样本 |
| 文本 PDF | `fixtures/synthetic/vat-invoice-text.pdf` | 文本层解析、字段定位与核心字段黄金样本 |
| 结构化 OFD | `fixtures/synthetic/vat-invoice.ofd` | OFD L0 路由与嵌入 XML 核心字段黄金样本 |
| 邮件 | `fixtures/synthetic/vat-invoice.eml` | EML 附件发现与稳定化 |
| 图片 | `fixtures/synthetic/ocr-vat-invoice.png` | 离线图片 OCR |
| 扫描 PDF | `fixtures/synthetic/ocr-vat-invoice-scanned.pdf` | Windows PDF 渲染与离线 OCR |
| 畸形输入 | `fixtures/synthetic/malformed.pdf`、`fixtures/synthetic/malformed.ofd` | 明确错误类型与不崩溃回归 |
| 重复输入 | `fixtures/synthetic/duplicate-a.xml`、`fixtures/synthetic/duplicate-b.xml` | 跨邮件内容去重黄金样本 |
| 资源限制 | `fixtures/synthetic/expanded-over-limit.zip` | ZIP 展开后大小上限回归 |
| 预期结果 | `fixtures/synthetic/expected-errors.json` | 畸形、超限和重复用例的机器可读预期 |
| 业务 DTO | `src-tauri/tests/fixtures/parsed_invoices.json` | 存储、去重、归组和导出集成测试 |

所有地址使用 `example.invalid`，组织名称使用“示例/演示”，号码与金额只用于固定断言，不对应真实用户、邮件或票据。

## 生成与验证

- `scripts/generate-synthetic-fixtures.py` 确定性生成文本 PDF、OFD、畸形文件、重复文件、超限 ZIP 和预期错误；连续两次生成必须保持字节与 SHA-256 不变。
- `crates/invoice-parse/tests/synthetic_file_fixtures.rs` 校验 PDF/OFD 全部黄金字段及畸形输入的明确错误和不崩溃行为。
- `crates/invoice-collect/tests/synthetic_file_fixtures.rs` 校验跨邮件内容去重、ZIP 展开上限及畸形 ZIP 拒绝。
- 文本 PDF 还须通过 `pdfinfo`、`pdfplumber`、页面渲染和人工视觉检查，确保字体嵌入、中文可读且无裁切或重叠。

## 范围边界

- M1.4 的分层合成夹具已闭环；未知格式和加密输入的路由矩阵属于 M4.1，未通过对应参数化测试前不得据此宣称格式路由完整。
- 真实样本只允许在仓库外受控目录执行；报告只能留下掩码统计和不可逆哈希。
