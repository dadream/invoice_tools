# G1 校验去重 Implementation Plan

**日期**: 2026-08-11
**任务**: G1 校验去重（多字段去重 + 签章验证 + 重复标记）
**前置**: S0.7 发票添加流程（已完成，commit 2279a7f）

## 目标

将 S0.7 的单发票号查重扩展为**多字段组合去重**，并接入 invoice-parse 已有的
**SM2/SM3 签章验证**能力，在添加发票时给出明确的重复/验签结果，供用户决策。

## 已验证的现有能力

以下能力已在代码库中实现并测试，G1 任务只需**接入和扩展**，不需从头实现：

### 1. 签章验证（`crates/invoice-parse/src/verify.rs`）

已有函数：
```rust
// verify.rs:26
pub fn verify_xml_signature(xml_bytes: &[u8])
    -> Result<VerificationResult, ParseError>

// verify.rs:130
pub fn verify_ofd_signature(ofd_bytes: &[u8])
    -> Result<VerificationResult, ParseError>
```

`VerificationResult`（`model.rs:60`）：
```rust
pub enum VerificationResult {
    Valid,           // 签名验证通过
    Invalid { reason: String },  // 签名不通过或被篡改
    NotSigned,       // 文件未签名（部分 OFD 没有签名文件）
}
```

签章验证是**可选操作**——未签名文件返回 `NotSigned` 而非错误，不阻断解析。
签名验证依赖 `smcrypto` crate（已在 `invoice-parse/Cargo.toml`）。

CLI 已有 `verify` 子命令验证单个文件（`main.rs:174`），`verify-all` 遍历全部样本。

### 2. 歧义检测（`crates/invoice-grouping/src/ambiguity.rs:16`）

已有函数：
```rust
pub fn detect_ambiguities(
    invoices: &[(usize, &ParsedInvoice)],
    trips: &[Trip],
    config: &GroupingConfig,
) -> Vec<Ambiguity>
```

能检测 5 类歧义（`types.rs:59`）：
- `NoReturnTicket` —— 单程票且下一趟起点不同
- `WeekendBetweenTrips` —— 周末夹在两趟中间
- `TransferStopover` —— 中转停留 4-12h
- `MultipleVisitsSameCity` —— 短期内多次往返同城
- `TimeOverlap` —— 两张票时间重叠，显示同时在两个城市

这些歧义**与重复检测正交**——歧义是归组逻辑问题，重复是同一张票多次录入。
但 `TimeOverlap` 检测可以辅助去重（同一时间段两张票金额、日期、城市均接近，
可能是同一张票的不同来源）。

### 3. 当前去重（`src-tauri/src/commands/invoice.rs:253`）

```rust
pub fn check_duplicate(state, invoice_number: String)
    -> AppResult<DuplicateCheckDto>
```

调用 `ledger_db.rs:302` 的 `find_invoice_by_number`，只按发票号精确匹配。
返回 `{ is_duplicate, existing_batch_id, existing_batch_name }`。

**局限**：
- 只按发票号查，扫描件与 XML/OFD 电子票同票不同源会漏检
- 不检测金额/日期/票种组合（用户可能改票号重复提交）
- 没有"疑似重复"等级，只有布尔结果

### 4. 数据库架构（`ledger_db.rs:56`）

`reported_invoices` 表字段：
- `id, batch_id, invoice_number, issue_date, amount, tax_amount,
   buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
   file_path, created_at, updated_at`

无 `verification_result` 或 `duplicate_of` 列 —— **需新增**。

## 任务拆分

三个任务顺序执行，每个任务一个 subagent。

---

## Task 1: 扩展数据库存储验签与去重结果

**文件**: `crates/invoice-store/src/ledger_db.rs`（改）、`models.rs`（改）

### Step 1: 新增字段到 `reported_invoices` 表

迁移 SQL（在 `LedgerDb::new` 的建表逻辑后追加 `ALTER TABLE` 语句，用
`PRAGMA user_version` 做版本控制）：

```sql
-- user_version 0 → 1: 添加验签与去重字段
ALTER TABLE reported_invoices ADD COLUMN verification_result TEXT;
ALTER TABLE reported_invoices ADD COLUMN is_duplicate INTEGER DEFAULT 0;
ALTER TABLE reported_invoices ADD COLUMN duplicate_reason TEXT;
```

- `verification_result`: `"valid"` / `"invalid"` / `"not_signed"` / `NULL`（未验证）
- `is_duplicate`: 0/1 布尔，1 表示被标记为重复
- `duplicate_reason`: 重复原因文本（如"发票号+金额+日期完全一致"）

`ReportedInvoice` 结构体（`models.rs:147`）同步新增三个字段：
```rust
pub verification_result: Option<String>,
pub is_duplicate: bool,
pub duplicate_reason: Option<String>,
```

更新 `add_invoice`、`list_invoices_by_batch`、`get_invoice` 的 SQL 与行解析器
（`parse_invoice_row`）覆盖新字段。

### Step 2: 新增多字段查重方法

```rust
/// 按多字段组合查找疑似重复（不含自身 id）
/// 匹配规则：发票号完全一致 **或** (金额+日期+票种) 三项一致
pub fn find_potential_duplicates(
    &self,
    invoice_number: &str,
    amount: &Decimal,
    issue_date: &NaiveDate,
    ticket_type: &str,
    exclude_id: Option<i64>,  // 排除自身（编辑场景）
) -> StoreResult<Vec<ReportedInvoice>>
```

SQL：
```sql
SELECT ... FROM reported_invoices
WHERE (invoice_number = ?1
       OR (amount = ?2 AND issue_date = ?3 AND ticket_type = ?4))
  AND (?5 IS NULL OR id != ?5)
```

返回命中列表，由调用方决定是否阻断或仅标记。

### Step 3: 测试

- 迁移逻辑：空库初始化跳过 ALTER，已有库升级到 user_version 1
- 重复插入同发票号 → `find_potential_duplicates` 返回命中
- 不同发票号但金额+日期+票种相同 → 同样命中
- `exclude_id` 排除自身
- 新字段在 `add_invoice` / `list_invoices_by_batch` 中正确读写

验证：
```bash
cargo test -p invoice-store
```

---

## Task 2: 后端接入验签与增强查重

**文件**: `src-tauri/src/commands/invoice.rs`（改）

### Step 1: `parse_invoice` 命令同步返回验签结果

在调用解析器后，根据文件扩展名调用验签：

```rust
let verification = match ext.as_str() {
    "xml" => {
        let vr = invoice_parse::verify::verify_xml_signature(&bytes)
            .unwrap_or(VerificationResult::Invalid {
                reason: "验证过程出错".into()
            });
        verification_result_to_string(&vr)
    }
    "ofd" => {
        let vr = invoice_parse::verify::verify_ofd_signature(&bytes)
            .unwrap_or(VerificationResult::Invalid {
                reason: "验证过程出错".into()
            });
        verification_result_to_string(&vr)
    }
    "pdf" => "not_applicable".to_string(),  // PDF 无签章
    _ => "not_applicable".to_string(),
};
```

`ParsedInvoiceDto` 新增 `verification_result: String` 字段，前端展示用。

### Step 2: 改造 `check_duplicate` 为多字段检测

```rust
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DuplicateCheckDto {
    pub is_duplicate: bool,
    pub match_type: Option<String>,  // "exact" / "fuzzy" / null
    pub existing_invoices: Vec<InvoiceSummaryDto>,  // 可能多条
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct InvoiceSummaryDto {
    pub id: i64,
    pub batch_id: i64,
    pub batch_name: String,
    pub invoice_number: String,
    pub amount: String,
    pub issue_date: String,
}

#[tauri::command]
pub fn check_duplicate(
    state: tauri::State<Mutex<AppState>>,
    invoice_number: String,
    amount: String,       // Decimal 字符串
    issue_date: String,   // YYYY-MM-DD
    ticket_type: String,
) -> AppResult<DuplicateCheckDto>
```

调用 `find_potential_duplicates`，根据命中字段判断 `match_type`：
- 发票号一致 → `"exact"`
- 金额+日期+票种一致但发票号不同 → `"fuzzy"`

前端可根据 `match_type` 调整提示（exact 直接阻断，fuzzy 警告但允许确认）。

### Step 3: `add_invoice_to_batch` 写入验签与查重结果

在构造 `ReportedInvoice` 时：
- `verification_result` 取自解析后验签结果
- 调用 `find_potential_duplicates`，命中则 `is_duplicate: true`，
  `duplicate_reason` 记录匹配类型与命中发票号
- 即使 `is_duplicate: true` 仍允许插入（**不自动删除**，由用户在列表中看到并手动处理）

### Step 4: 测试

- `parse_invoice` XML/OFD 样本返回 verification_result 非空
- `check_duplicate` 传入三字段命中既有记录 → `match_type: "fuzzy"`
- `add_invoice_to_batch` 重复发票入库后 `is_duplicate: true`

验证：
```bash
source scripts/tauri-env.sh
cargo test -p invoice-assistant
```

---

## Task 3: 前端展示验签与重复标记

**文件**: `ui/src/lib/types.ts`（改）、
`ui/src/routes/invoices/ParseResultCard.svelte`（改）、
`InvoiceList.svelte`（改）

### Step 1: 扩展类型定义

```ts
export type VerificationResult = 'valid' | 'invalid' | 'not_signed' | 'not_applicable' | null

export interface ParsedInvoice {
  // ...现有字段
  verification_result: VerificationResult
}

export interface Invoice {
  // ...现有字段
  verification_result: VerificationResult | null
  is_duplicate: boolean
  duplicate_reason: string | null
}

export interface DuplicateCheck {
  is_duplicate: boolean
  match_type: 'exact' | 'fuzzy' | null
  existing_invoices: InvoiceSummary[]
}

export interface InvoiceSummary {
  id: number
  batch_id: number
  batch_name: string
  invoice_number: string
  amount: string
  issue_date: string
}

export const VERIFICATION_LABELS: Record<VerificationResult, string> = {
  valid: '✓ 签名有效',
  invalid: '✗ 签名无效',
  not_signed: '未签名',
  not_applicable: 'N/A',
  null: '未验证',
}

export const VERIFICATION_COLORS: Record<VerificationResult, string> = {
  valid: 'green',
  invalid: 'red',
  not_signed: 'gray',
  not_applicable: 'gray',
  null: 'gray',
}
```

### Step 2: `ParseResultCard` 展示验签结果

在解析级别徽标旁新增验签徽标，颜色映射到 `VERIFICATION_COLORS`。
`invalid` 时在卡片顶部显示醒目警告（与重复警告同级别）。

`check_duplicate` 改为传入四个参数（`invoiceNumber, amount, issueDate, ticketType`）。
根据 `match_type` 调整提示：
- `exact`: "该发票已在《X 批次》中报销（发票号完全一致）" + 禁用确认按钮
- `fuzzy`: "疑似重复：发票 Y 的金额、日期、票种与该票一致，但发票号不同。
  请核对是否为同一张票。" + 允许确认但标黄警告

展开既有发票列表（`existing_invoices`），点击可跳转到对应批次。

### Step 3: `InvoiceList` 标记重复行

`is_duplicate: true` 的行背景色设为浅红，发票号列后追加 `🔁` 图标，
hover 显示 `duplicate_reason`。

操作列新增"取消重复标记"按钮（仅 `is_duplicate: true` 时显示），
调用新命令 `clear_duplicate_flag(invoice_id)` 重置标记。

### Step 4: 新增 `clear_duplicate_flag` 命令（后端补充）

```rust
#[tauri::command]
pub fn clear_duplicate_flag(
    state: tauri::State<Mutex<AppState>>,
    invoice_id: i64,
) -> AppResult<()>
{
    let db = state.lock().unwrap().ledger_db();
    db.conn.execute(
        "UPDATE reported_invoices
         SET is_duplicate = 0, duplicate_reason = NULL
         WHERE id = ?1",
        params![invoice_id],
    )?;
    Ok(())
}
```

注册到 `main.rs` 的 `generate_handler!`。

### Step 5: 前端校验

```bash
cd ui && npm run check && npm run build
```

---

## 验收标准

- [ ] 数据库迁移：user_version 0 → 1，新字段可读写
- [ ] `find_potential_duplicates` 按发票号精确匹配 + 三字段模糊匹配
- [ ] `parse_invoice` 返回 XML/OFD 的验签结果
- [ ] `check_duplicate` 传四参数，返回 match_type 与既有票列表
- [ ] `add_invoice_to_batch` 重复票仍入库但标记 `is_duplicate: true`
- [ ] 前端解析结果卡展示验签徽标，invalid 显示警告
- [ ] 前端查重提示区分 exact（阻断）与 fuzzy（警告）
- [ ] 发票列表重复行有视觉标识，可清除标记
- [ ] `cargo test --workspace` 全绿（S0.7 基线 234）
- [ ] `npm run check` 0 errors / 0 warnings

## 已知限制

- **PDF 无签章验证**：PDF 行程单无数字签名，`verification_result` 固定为 `not_applicable`
- **模糊匹配仅三字段**：不考虑销方名称、税额等次要字段。更复杂的相似度计算
  （如编辑距离）留待后续优化
- **不自动合并重复票**：只标记，由用户决定保留哪一条并手动删除另一条
- **签章验证不阻断入库**：`invalid` 或 `not_signed` 仍允许添加，
  签名状态作为参考信息而非硬性约束

## 下一步

G1 完成后，L2 依赖层还剩 **G2 审核界面**。建议下一步 **H1 流水线串联**，
因为：
- G2 依赖批次/发票功能完整（S0.6 + S0.7 + G1 均已完成）
- H1 打通端到端，可进行首次内部测试
- G2 的归组调整 UI 可以在 H1 验证后再精细化
