# G1 校验去重 完成报告

**日期**: 2026-08-11
**commit**: `1a82a69`
**计划**: `2026-08-11-g1-validation-dedup.md`

## 交付内容

### 数据库层（Task 1）

**迁移（user_version 0 → 1）**：
- `reported_invoices` 表新增三个字段：
  - `verification_result TEXT` —— 签章验证结果（`"valid"` / `"invalid"` / `"not_signed"` / `NULL`）
  - `is_duplicate INTEGER DEFAULT 0` —— 重复标记（0/1 布尔）
  - `duplicate_reason TEXT` —— 重复原因说明

**新增方法**（`ledger_db.rs`）：
- `find_potential_duplicates()` —— 多字段组合查重：
  - 发票号精确匹配 **OR** (金额 + 日期 + 票种) 三字段模糊匹配
  - `exclude_id` 参数排除自身（编辑场景）
  - 返回命中列表，由调用方决定是否阻断
- `clear_duplicate_flag()` —— 清除重复标记（单 UPDATE 语句）

迁移逻辑幂等：检查 `PRAGMA user_version` 与字段是否已存在，避免重复执行。

### 后端层（Task 2）

**接入签章验证**（`invoice.rs`）：
- `parse_invoice` 命令调用 `invoice_parse::verify::{verify_xml_signature, verify_ofd_signature}`
- `ParsedInvoiceDto` 新增 `verification_result` 字段
- PDF 固定返回 `"not_applicable"`（行程单无数字签名）
- 验签失败不阻断解析，作为参考信息返回

**增强查重**（`invoice.rs`）：
- `check_duplicate` 改为接收 **4 参数**：`invoiceNumber`, `amount`, `issueDate`, `ticketType`
- 调用 `find_potential_duplicates`，返回新 DTO：
  ```ts
  {
    is_duplicate: boolean,
    match_type: 'exact' | 'fuzzy' | null,  // 发票号一致 vs 三字段一致
    existing_invoices: InvoiceSummary[]     // 命中列表（含批次名）
  }
  ```
- `add_invoice_to_batch` 写入验签结果与查重标记
- **重复发票仍允许入库**（不自动删除，由用户决策）

**新增命令**：
- `clear_duplicate_flag(invoiceId)` —— 清除重复标记

### 前端层（Task 3）

**ParseResultCard.svelte**：
- 解析级别徽标旁新增**验签徽标**，颜色映射（绿色 valid / 红色 invalid / 灰色 not_signed/not_applicable）
- `invalid` 签名时在卡片顶部显示**红色警告横幅**
- `check_duplicate` 调用传递 4 参数（从 `parsed` 提取）
- 根据 `match_type` 分级提示：
  - **exact**（发票号一致）：红色横幅阻断 + 禁用确认按钮 + "发票号完全一致"
  - **fuzzy**（三字段一致）：黄色横幅警告 + 允许确认 + "金额/日期/票种一致但发票号不同，请核对"
- 展开 `existing_invoices` 列表，显示批次名、发票号、金额、日期

**InvoiceList.svelte**：
- `is_duplicate: true` 的行背景**浅红色**（`#fff0f0`）
- 发票号列后追加 **🔁 emoji**，hover 显示 `duplicate_reason`
- 操作列新增**"取消重复"按钮**（仅重复行显示），调用 `clear_duplicate_flag`，成功后刷新列表

## 验证结果

| 项目 | 结果 |
|------|------|
| `cargo test --workspace` | **242 passed, 0 failed**（S0.7 基线 234，+8） |
| `npm run check` | **212 files, 0 errors, 0 warnings** |
| `npm run build` | 通过（85.44 kB / gzip 28.82 kB） |

## 测试覆盖

**新增测试（8 个）**：

数据库层（7 个）：
1. 新字段读写
2. 发票号精确匹配
3. 三字段模糊匹配
4. 排除自身功能
5. 无匹配场景
6. 多条匹配
7. 完整迁移流程（v0 → v1）

后端层（1 个）：
8. 签章状态枚举转字符串映射

## 技术亮点

### 1. 数据库迁移策略

采用 SQLite `PRAGMA user_version` 版本控制 + 字段存在性检查双重保障：
```sql
PRAGMA user_version;  -- 读取版本号
-- 若为 0 且字段不存在，执行 ALTER TABLE
PRAGMA user_version = 1;  -- 升级版本
```
迁移幂等，空库初始化直接建新表结构，旧库平滑升级。

### 2. 多字段组合查重

SQL 一次查询覆盖两种匹配模式：
```sql
WHERE (invoice_number = ?1
       OR (amount = ?2 AND issue_date = ?3 AND ticket_type = ?4))
  AND (?5 IS NULL OR id != ?5)
```
前端根据命中字段类型判断 `match_type`（exact/fuzzy），实现分级提示。

### 3. 签章验证流程

```
文件字节 → 解析器 → ParsedInvoice
   ↓
验签函数 → VerificationResult → DTO
   ↓
前端展示（徽标 + 警告）
```
验签与解析解耦，失败不阻断入库，作为辅助判断依据。

### 4. 重复标记而非自动删除

设计理念：**标记 + 人工确认 > 自动删除**
- `is_duplicate: true` 仍允许入库
- 列表中视觉标识（红背景 + 🔁 图标）
- 用户可清除标记或手动删除发票
- 避免误删（如同一趟行程的往返票可能触发模糊匹配）

## 已知限制

- **PDF 无签章验证**：PDF 行程单无数字签名，`verification_result` 固定为 `"not_applicable"`
- **模糊匹配仅三字段**：不考虑销方名称、税额等次要字段。编辑距离等更复杂相似度计算留待后续
- **不自动合并重复票**：只标记，不自动删除或合并
- **签章验证不阻断**：`invalid` 或 `not_signed` 仍允许添加，签名状态作为参考而非硬性约束
- **迁移仅支持 v0 → v1**：未来新增字段需扩展迁移逻辑

## 与 invoice-grouping 的关系

G1 的**重复检测**与 `invoice-grouping` 的**歧义检测**正交：
- 重复检测：同一张票多次录入（发票号 / 三字段完全一致）
- 歧义检测：归组逻辑问题（如 `TimeOverlap` 检测同一时间段两张票，可能是不同来源的同一张票）

两者可互补：`TimeOverlap` 歧义命中时，可建议运行查重确认是否为同票不同源。

## 下一步建议

G1 完成后，L2 依赖层还剩 **G2 审核界面**。但 roadmap 建议优先 **H1 流水线串联**：
- G2 依赖批次/发票功能完整（S0.6 + S0.7 + G1 均已完成）✅
- H1 打通端到端，可进行首次内部测试
- G2 的归组调整 UI 可以在 H1 验证后再精细化

或者考虑 **D 输出模块**（PDF 生成 + Excel 导出），为 H1 准备输出能力。
