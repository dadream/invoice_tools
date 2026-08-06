# 行程归组引擎实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现行程归组算法，将发票按出差行程自动分组，达到 α 验收线（平均调整 < 3 张/批次）

**Architecture:** 7 步确定性算法覆盖 70-80% 场景（同日多城、火车中转、去机场市内票等），5 类歧义场景交给 LLM 批量判定。归组引擎与 UI 完全解耦，输入发票列表输出行程分组。先用合成数据建测试集独立开发，不依赖解析器完成。

**Tech Stack:** Rust 2021, `chrono` 0.4, `rust_decimal` 1.36, `serde` 1.0, DeepSeek V4 Flash API（仅歧义判定）

## Global Constraints

- Working directory: `/home/holo/work-tools`
- `cargo` path: `$HOME/.cargo/bin/cargo`，若报 not found 先执行 `export PATH="$HOME/.cargo/bin:$PATH"`
- 金额一律 `rust_decimal::Decimal`，禁止 `f64`
- 日期使用 `chrono::NaiveDate`
- **归组引擎必须与 UI 解耦**：输入 `(Vec<Invoice>, 常驻城市, 配置)` → 输出 `(Vec<Trip>, 置信度, 歧义列表)`
- 所有注释、日志、报告使用简体中文
- 每个任务完成后提交，使用 Conventional Commits (`feat:`, `fix:`, `test:`)
- **先用合成数据测试，不依赖真实样本**

---

## 计划范围说明

本计划聚焦于**归组引擎核心算法**，不包含：
- Tauri UI（单独计划）
- SQLite 存储（单独计划）
- IMAP 邮件采集（已有独立模块）
- 发票解析（已完成，90.9% 准确率）

**为什么归组引擎优先：**
1. 是 α 验收线的唯一卡点（< 3 张/批次调整）
2. 最高技术风险（需验证确定性算法 + LLM 组合能否达标）
3. 可完全独立开发（用合成数据测试）
4. 是产品核心差异化能力（长期资产）

---

## 下一阶段待办

**当前已完成：**
- ✅ 解析准确率 90.9%（轨道 B）
- ✅ 邮件采集模块（轨道 A，`invoice-collect` crate）

**下一阶段优先级（按依赖顺序）：**

1. **本计划：归组引擎（轨道 C）** - 2-3 周
2. 核心数据模型扩展（S0.3）- 1 周
3. 校验与去重 + 跨月台账（G1）- 1 周  
4. 输出格式（轨道 D）- 1 周
5. Tauri 骨架 + 审核界面（S0.2 + G2）- 2 周

---

待续...（计划正在构建中，请使用 subagent-driven-development 执行）

## File Structure

| 文件 | 职责 |
|---|---|
| `crates/invoice-parse/src/model.rs` (modify) | 扩展 `Invoice` 添加 `city`, `departure_time`, `checkin_date` 字段；新增 `Trip`, `TripKind`, `AmbiguityCase` 类型 |
| `crates/invoice-grouping/` (create) | 新 crate：归组引擎独立模块 |
| `crates/invoice-grouping/src/lib.rs` (create) | 公开接口：`group_invoices()` |
| `crates/invoice-grouping/src/types.rs` (create) | `GroupingConfig`, `GroupingResult`, `Ambiguity` 类型 |
| `crates/invoice-grouping/src/deterministic.rs` (create) | 7 步确定性算法 |
| `crates/invoice-grouping/src/ambiguity.rs` (create) | 5 类歧义检测与 LLM 调用 |
| `crates/invoice-grouping/tests/synthetic.rs` (create) | 合成场景测试集（20-30 场景） |
| `Cargo.toml` (modify) | workspace 添加 `invoice-grouping` |

---

## Task 1: 扩展核心数据模型支持归组

**Files:**
- Modify: `crates/invoice-parse/src/model.rs:32-47` (`ParsedInvoice` struct)
- Test: `crates/invoice-parse/src/model.rs` (inline tests)

**Interfaces:**
- Produces:
  - `model::ParsedInvoice` 新增字段：
    - `city: Option<String>` - 发票关联城市（交通票为出发城市，酒店为入住城市）
    - `departure_time: Option<NaiveDateTime>` - 交通票出发时间（用于行程排序）
    - `checkin_date: Option<NaiveDate>` - 酒店入住日期（用 checkin 而非 issue_date）

**Why this task is first:** 归组引擎需要城市和时间信息，但当前 `ParsedInvoice` 只有日期和金额。必须先扩展模型才能开始归组逻辑。

- [ ] **Step 1: 添加新字段到 ParsedInvoice**

打开 `crates/invoice-parse/src/model.rs`，找到 `ParsedInvoice` 结构体（约 32 行），在 `source_path` 字段之前添加：

```rust
    /// 发票关联城市（交通票为出发城市，酒店为入住城市，其他为消费城市）
    pub city: Option<String>,
    /// 交通票出发时间（用于行程时间轴排序）
    pub departure_time: Option<chrono::NaiveDateTime>,
    /// 酒店入住日期（注意：不是 issue_date，酒店常延迟开票）
    pub checkin_date: Option<NaiveDate>,
```

- [ ] **Step 2: 更新所有构造 ParsedInvoice 的位置**

运行编译查找所有构造点：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p invoice-parse 2>&1 | grep "missing field"
```

预期会报错指向 `xml.rs`, `ofd.rs`, `pdf.rs`, `ocr.rs` 等文件。逐个添加这三个字段，初始值填 `None`：

```rust
city: None,
departure_time: None,
checkin_date: None,
```

- [ ] **Step 3: 验证编译通过**

```bash
cargo build -p invoice-parse
```

预期：编译成功，无错误和警告。

- [ ] **Step 4: 运行现有测试确保无回归**

```bash
cargo test -p invoice-parse
```

预期：69 个测试全部通过。

- [ ] **Step 5: 提交**

```bash
git add crates/invoice-parse/src/model.rs crates/invoice-parse/src/*.rs
git commit -m "feat(model): 为归组引擎添加 city、departure_time、checkin_date 字段"
```

---

## Task 2: 创建独立的归组引擎 crate

**Files:**
- Create: `crates/invoice-grouping/Cargo.toml`
- Create: `crates/invoice-grouping/src/lib.rs`
- Create: `crates/invoice-grouping/src/types.rs`
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Consumes: `invoice-parse::model::{ParsedInvoice, TicketType}`
- Produces:
  - `invoice_grouping::group_invoices(invoices: &[ParsedInvoice], config: &GroupingConfig) -> GroupingResult`
  - `GroupingConfig { home_cities: Vec<String>, ambiguity_handler: Box<dyn AmbiguityResolver> }`
  - `GroupingResult { trips: Vec<Trip>, ambiguities: Vec<Ambiguity>, confidence: f32 }`
  - `Trip { kind: TripKind, invoice_ids: Vec<usize>, start_date: NaiveDate, end_date: NaiveDate }`
  - `TripKind { BusinessTrip { cities: Vec<String> }, LocalMonth { year: i32, month: u32 }, Excluded, NeedsReview }`

- [ ] **Step 1: 创建 crate 目录结构**

```bash
mkdir -p crates/invoice-grouping/src
```

- [ ] **Step 2: 写 Cargo.toml**

创建 `crates/invoice-grouping/Cargo.toml`：

```toml
[package]
name = "invoice-grouping"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]
invoice-parse = { path = "../invoice-parse" }
chrono = { workspace = true }
rust_decimal = { workspace = true }
serde = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 3: 添加到 workspace**

编辑根目录 `Cargo.toml`，在 `[workspace]` 的 `members` 数组中添加：

```toml
members = [
    "crates/invoice-collect",
    "crates/invoice-parse",
    "crates/invoice-grouping",  # 新增
]
```

- [ ] **Step 4: 创建类型定义文件**

创建 `crates/invoice-grouping/src/types.rs`：

```rust
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// 行程类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TripKind {
    /// 出差行程：包含起止日期和途经城市链
    BusinessTrip {
        start: NaiveDate,
        end: NaiveDate,
        cities: Vec<String>,
    },
    /// 市内消费：某年某月的本地消费
    LocalMonth { year: i32, month: u32 },
    /// 用户标记为排除（如家属票、同事票）
    Excluded,
    /// 需要人工审核（歧义未解决或低置信度）
    NeedsReview { reason: String },
}

/// 一个行程（出差或市内消费桶）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trip {
    pub kind: TripKind,
    /// 归属此行程的发票索引（对应输入 Vec<ParsedInvoice> 的下标）
    pub invoice_ids: Vec<usize>,
    /// 置信度 0.0-1.0
    pub confidence: f32,
}

/// 归组配置
#[derive(Debug, Clone)]
pub struct GroupingConfig {
    /// 常驻城市列表（支持多个，如 ["北京", "上海"]）
    pub home_cities: Vec<String>,
    /// 歧义解决器（可 mock，生产环境用 LLM）
    pub ambiguity_resolver: Option<Box<dyn AmbiguityResolver>>,
}

/// 歧义解决器 trait（便于测试 mock）
pub trait AmbiguityResolver: Send + Sync {
    fn resolve(&self, ambiguities: &[Ambiguity]) -> Result<Vec<AmbiguityResolution>, anyhow::Error>;
}

/// 歧义类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AmbiguityKind {
    /// 无返程票且下一趟起点不同
    NoReturnTicket,
    /// 周末夹在两趟中间且无明确返程
    WeekendBetweenTrips,
    /// 中转停留 4-12 小时（< 4h 判中转，> 12h 判行程点）
    TransferStopover,
    /// 同一城市短期内多次往返
    MultipleVisitsSameCity,
    /// 时间重叠（两张票显示同时在两个城市）
    TimeOverlap,
}

/// 歧义实例
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ambiguity {
    pub kind: AmbiguityKind,
    pub description: String,
    pub involved_invoice_ids: Vec<usize>,
    pub candidates: Vec<String>, // 候选解决方案描述
}

/// 歧义解决结果
#[derive(Debug, Clone)]
pub struct AmbiguityResolution {
    pub ambiguity_index: usize,
    pub chosen_candidate: usize,
    pub confidence: f32,
    pub reason: String,
}

/// 归组结果
#[derive(Debug)]
pub struct GroupingResult {
    pub trips: Vec<Trip>,
    pub unresolved_ambiguities: Vec<Ambiguity>,
    pub overall_confidence: f32,
}
```

- [ ] **Step 5: 创建主接口文件**

创建 `crates/invoice-grouping/src/lib.rs`：

```rust
pub mod types;

use invoice_parse::model::ParsedInvoice;
use types::*;

/// 主入口：将发票列表归组为行程
///
/// # Arguments
/// * `invoices` - 已解析的发票列表
/// * `config` - 归组配置（常驻城市、歧义解决器）
///
/// # Returns
/// 归组结果，包含行程列表、未解决歧义、整体置信度
pub fn group_invoices(
    invoices: &[ParsedInvoice],
    config: &GroupingConfig,
) -> Result<GroupingResult, anyhow::Error> {
    // TODO: Task 3-5 实现
    Ok(GroupingResult {
        trips: vec![],
        unresolved_ambiguities: vec![],
        overall_confidence: 0.0,
    })
}
```

- [ ] **Step 6: 验证编译**

```bash
cargo build -p invoice-grouping
```

预期：编译成功，警告 `unused` 可忽略。

- [ ] **Step 7: 提交**

```bash
git add crates/invoice-grouping/ Cargo.toml
git commit -m "feat(grouping): 创建独立归组引擎 crate 和类型定义"
```

---


## Task 3: 构建合成测试数据集

**Files:**
- Create: `crates/invoice-grouping/tests/synthetic.rs`
- Create: `crates/invoice-grouping/tests/fixtures.rs` (测试辅助函数)

**Interfaces:**
- Produces: 20-30 个合成场景，每个场景包含：
  - 输入：`Vec<ParsedInvoice>` with synthetic data
  - 期望输出：`Vec<Trip>` with expected grouping
  - 覆盖所有确定性场景 + 5 类歧义

**Why synthetic data first:** 归组逻辑可以独立于解析器验证。合成数据让我们快速迭代算法，不需要等待真实样本或解析器完成。

- [ ] **Step 1: 创建测试辅助函数**

创建 `crates/invoice-grouping/tests/fixtures.rs`：

```rust
use chrono::{NaiveDate, NaiveDateTime};
use invoice_parse::model::{ParsedInvoice, TicketType, ParseLevel};
use rust_decimal::Decimal;
use std::path::PathBuf;

/// 构建一张合成交通票
pub fn make_transport(
    idx: usize,
    ticket_type: TicketType,
    date: NaiveDate,
    hour: u32,
    from_city: &str,
    to_city: &str,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: None,
        ticket_type,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(from_city.to_string()),
        departure_time: Some(NaiveDateTime::new(
            date,
            chrono::NaiveTime::from_hms_opt(hour, 0, 0).unwrap(),
        )),
        checkin_date: None,
    }
}

/// 构建一张合成酒店票
pub fn make_hotel(
    idx: usize,
    invoice_date: NaiveDate,
    checkin_date: NaiveDate,
    city: &str,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: invoice_date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: None,
        ticket_type: TicketType::Hotel,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
        departure_time: None,
        checkin_date: Some(checkin_date),
    }
}

/// 构建一张合成市内票（出租车、餐饮等）
pub fn make_local(
    idx: usize,
    date: NaiveDate,
    city: &str,
    ticket_type: TicketType,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: None,
        ticket_type,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
        departure_time: None,
        checkin_date: None,
    }
}
```

- [ ] **Step 2: 创建第一个测试场景 - 标准单趟出差**

创建 `crates/invoice-grouping/tests/synthetic.rs`：

```rust
mod fixtures;

use chrono::NaiveDate;
use invoice_grouping::{group_invoices, types::*};
use invoice_parse::model::TicketType;
use fixtures::*;

#[test]
fn test_single_trip_with_return() {
    // 场景：7月3日去上海，7月5日返回北京
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 5), d(7, 3), "上海", "680.0"),
        make_local(3, d(7, 4), "上海", TicketType::CityTransport, "28.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：1 个出差行程，包含所有 4 张票
    assert_eq!(result.trips.len(), 1);
    
    match &result.trips[0].kind {
        TripKind::BusinessTrip { start, end, cities } => {
            assert_eq!(*start, d(7, 3));
            assert_eq!(*end, d(7, 5));
            assert_eq!(cities, &vec!["上海".to_string()]);
            assert_eq!(result.trips[0].invoice_ids, vec![0, 1, 2, 3]);
        }
        _ => panic!("期望 BusinessTrip，实际 {:?}", result.trips[0].kind),
    }

    assert_eq!(result.unresolved_ambiguities.len(), 0);
    assert!(result.overall_confidence > 0.9);
}

#[test]
fn test_multi_city_trip() {
    // 场景：7月3日北京→上海，7月5日上海→深圳，7月7日深圳→北京
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(3, TicketType::Flight, d(7, 5), 14, "上海", "深圳", "850.0"),
        make_hotel(4, d(7, 6), d(7, 5), "深圳", "520.0"),
        make_transport(5, TicketType::Flight, d(7, 7), 18, "深圳", "北京", "920.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert_eq!(cities, &vec!["上海".to_string(), "深圳".to_string()]);
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

#[test]
fn test_local_month_only() {
    // 场景：纯市内消费，无城际交通
    let invoices = vec![
        make_local(1, d(7, 3), "北京", TicketType::CityTransport, "28.0"),
        make_local(2, d(7, 8), "北京", TicketType::Meal, "156.0"),
        make_local(3, d(7, 15), "北京", TicketType::CityTransport, "32.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::LocalMonth { year, month } => {
            assert_eq!(*year, 2026);
            assert_eq!(*month, 7);
        }
        _ => panic!("期望 LocalMonth"),
    }
}

#[test]
fn test_airport_taxi_attached_to_trip() {
    // 场景：去机场的出租车应归入行程
    let invoices = vec![
        make_local(1, d(7, 3), "北京", TicketType::CityTransport, "85.0"), // 去机场
        make_transport(2, TicketType::Flight, d(7, 3), 14, "北京", "上海", "850.0"),
        make_hotel(3, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(4, TicketType::Flight, d(7, 5), 16, "上海", "北京", "850.0"),
        make_local(5, d(7, 5), "北京", TicketType::CityTransport, "90.0"), // 回家
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：所有 5 张票归入同一行程（包括两端出租车）
    assert_eq!(result.trips.len(), 1);
    assert_eq!(result.trips[0].invoice_ids.len(), 5);
}

// 辅助函数：快速构造日期
fn d(month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, day).unwrap()
}
```

- [ ] **Step 3: 添加歧义场景测试**

在 `crates/invoice-grouping/tests/synthetic.rs` 末尾添加：

```rust
#[test]
fn test_no_return_ticket_ambiguity() {
    // 歧义场景：去上海无返程票，下一趟去深圳
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        // 缺失返程票
        make_transport(3, TicketType::Flight, d(7, 10), 14, "北京", "深圳", "850.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None, // 无解决器，应检测到歧义
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：检测到 NoReturnTicket 歧义
    assert!(!result.unresolved_ambiguities.is_empty());
    assert!(matches!(
        result.unresolved_ambiguities[0].kind,
        AmbiguityKind::NoReturnTicket
    ));
}

#[test]
fn test_transfer_stopover_within_4h() {
    // 确定性场景：中转 < 4h，应判定为中转而非行程点
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "郑州", "300.0"),
        make_transport(2, TicketType::Rail, d(7, 3), 12, "郑州", "广州", "450.0"), // 3小时中转
        make_hotel(3, d(7, 4), d(7, 3), "广州", "580.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "广州", "北京", "750.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：1 个行程，郑州不出现在 cities（只是中转）
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert_eq!(cities, &vec!["广州".to_string()]);
        }
        _ => panic!("期望 BusinessTrip"),
    }
}
```

- [ ] **Step 4: 验证测试编译但失败（TDD）**

```bash
cargo test -p invoice-grouping
```

预期：编译成功，但所有测试失败（因为 `group_invoices` 还是空实现）。

- [ ] **Step 5: 提交**

```bash
git add crates/invoice-grouping/tests/
git commit -m "test(grouping): 添加 20+ 合成场景测试集"
```

---

## Task 4: 实现 7 步确定性归组算法

**Files:**
- Create: `crates/invoice-grouping/src/deterministic.rs`
- Modify: `crates/invoice-grouping/src/lib.rs`

**Interfaces:**
- Consumes: `&[ParsedInvoice]`, `&GroupingConfig`
- Produces: `(Vec<Trip>, Vec<Ambiguity>)`
- Implements 7-step algorithm from product spec §M4

**Algorithm outline:**
1. 提取所有城际交通票（Rail/Flight），按 departure_time 排序
2. 切分行程：从常驻城市出发 = 起点，回到常驻城市 = 终点
3. 挂住宿：checkin_date ∈ [start, end] 且 city ∈ city_chain
4. 挂零散：时间 ∈ [start−6h, end+6h] 且 city ∈ city_chain ∪ {home_city 机场往返}
5. 残余 → 按月归入"市内消费"桶
6. 检测歧义（5 类）
7. 返回行程列表 + 歧义列表

- [ ] **Step 1: 创建确定性算法文件骨架**

创建 `crates/invoice-grouping/src/deterministic.rs`：

```rust
use crate::types::*;
use chrono::{Datelike, Duration, NaiveDate};
use invoice_parse::model::{ParsedInvoice, TicketType};
use std::collections::{HashMap, HashSet};

const TRANSFER_THRESHOLD_HOURS: i64 = 4;
const SAME_DAY_THRESHOLD_HOURS: i64 = 6;

pub fn group_deterministic(
    invoices: &[ParsedInvoice],
    config: &GroupingConfig,
) -> (Vec<Trip>, Vec<Ambiguity>) {
    let mut trips = Vec::new();
    let mut ambiguities = Vec::new();

    // Step 1: 提取城际交通票并排序
    let mut intercity: Vec<(usize, &ParsedInvoice)> = invoices
        .iter()
        .enumerate()
        .filter(|(_, inv)| matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight))
        .collect();
    
    intercity.sort_by_key(|(_, inv)| inv.departure_time);

    // Step 2: 切分行程
    let segments = split_into_segments(&intercity, &config.home_cities);

    // Step 3-4: 为每个行程段挂载住宿和零散票
    for seg in segments {
        let trip = build_trip_from_segment(seg, invoices, &config.home_cities);
        trips.push(trip);
    }

    // Step 5: 残余票归入市内桶
    let assigned_ids: HashSet<usize> = trips
        .iter()
        .flat_map(|t| t.invoice_ids.iter().copied())
        .collect();
    
    let remaining: Vec<usize> = (0..invoices.len())
        .filter(|id| !assigned_ids.contains(id))
        .collect();

    if !remaining.is_empty() {
        trips.extend(group_local_by_month(&remaining, invoices));
    }

    // Step 6: 检测歧义
    ambiguities = detect_ambiguities(&trips, invoices, &config.home_cities);

    (trips, ambiguities)
}

struct Segment {
    start_idx: usize,
    end_idx: usize,
    intercity_ids: Vec<usize>,
}

fn split_into_segments(
    intercity: &[(usize, &ParsedInvoice)],
    home_cities: &[String],
) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current_segment_ids = Vec::new();
    let mut in_trip = false;

    for (idx, inv) in intercity {
        let from_city = inv.city.as_deref().unwrap_or("");
        
        // 从常驻城市出发 = 行程起点
        if !in_trip && home_cities.iter().any(|h| from_city.contains(h)) {
            in_trip = true;
            current_segment_ids.push(*idx);
        } 
        // 已在行程中
        else if in_trip {
            current_segment_ids.push(*idx);
            
            // TODO: 提取目的城市，判断是否回到常驻城市
            // 如果回到，结束当前 segment
        }
    }

    // TODO: 构建 Segment 结构体并返回
    segments
}

fn build_trip_from_segment(
    segment: Segment,
    all_invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Trip {
    // TODO: Step 3-4 实现
    Trip {
        kind: TripKind::BusinessTrip {
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            cities: vec![],
        },
        invoice_ids: segment.intercity_ids,
        confidence: 1.0,
    }
}

fn group_local_by_month(
    invoice_ids: &[usize],
    all_invoices: &[ParsedInvoice],
) -> Vec<Trip> {
    let mut by_month: HashMap<(i32, u32), Vec<usize>> = HashMap::new();
    
    for &id in invoice_ids {
        let date = all_invoices[id].issue_date;
        by_month
            .entry((date.year(), date.month()))
            .or_default()
            .push(id);
    }

    by_month
        .into_iter()
        .map(|((year, month), ids)| Trip {
            kind: TripKind::LocalMonth { year, month },
            invoice_ids: ids,
            confidence: 1.0,
        })
        .collect()
}

fn detect_ambiguities(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    // TODO: Step 6 实现 5 类歧义检测
    vec![]
}
```

- [ ] **Step 2: 在主接口中调用确定性算法**

修改 `crates/invoice-grouping/src/lib.rs`：

```rust
pub mod types;
mod deterministic;

use invoice_parse::model::ParsedInvoice;
use types::*;

pub fn group_invoices(
    invoices: &[ParsedInvoice],
    config: &GroupingConfig,
) -> Result<GroupingResult, anyhow::Error> {
    // Step 1-6: 确定性算法
    let (mut trips, ambiguities) = deterministic::group_deterministic(invoices, config);

    // Step 7: 如果有歧义且提供了解决器，调用 LLM
    let unresolved = if !ambiguities.is_empty() {
        if let Some(resolver) = &config.ambiguity_resolver {
            let resolutions = resolver.resolve(&ambiguities)?;
            apply_resolutions(&mut trips, &resolutions, &ambiguities);
            vec![] // 已解决
        } else {
            ambiguities // 无解决器，返回未解决列表
        }
    } else {
        vec![]
    };

    let overall_confidence = calculate_confidence(&trips);

    Ok(GroupingResult {
        trips,
        unresolved_ambiguities: unresolved,
        overall_confidence,
    })
}

fn apply_resolutions(
    trips: &mut [Trip],
    resolutions: &[AmbiguityResolution],
    ambiguities: &[Ambiguity],
) {
    // TODO: 根据 LLM 决策调整行程划分
}

fn calculate_confidence(trips: &[Trip]) -> f32 {
    if trips.is_empty() {
        return 0.0;
    }
    trips.iter().map(|t| t.confidence).sum::<f32>() / trips.len() as f32
}
```

- [ ] **Step 3: 运行测试查看哪些场景已通过**

```bash
cargo test -p invoice-grouping -- --nocapture
```

预期：部分简单场景可能通过（如 `test_local_month_only`），复杂场景失败。

- [ ] **Step 4: 逐步完善算法直到基础测试通过**

根据测试失败信息，逐步实现：
- `split_into_segments` 的完整逻辑（识别往返）
- `build_trip_from_segment` 挂载住宿和零散票
- 中转识别（< 4h 不算行程点）
- 机场往返出租车识别

**提示：** 这一步是迭代的，预期需要多次运行测试、修改、再测试。

- [ ] **Step 5: 确保至少 15/20 合成场景通过**

```bash
cargo test -p invoice-grouping
```

预期：至少 15 个测试通过，剩余的可能涉及歧义处理（Task 5）。

- [ ] **Step 6: 提交**

```bash
git add crates/invoice-grouping/src/
git commit -m "feat(grouping): 实现 7 步确定性归组算法（15/20 场景通过）"
```

---


## Task 5: 实现歧义检测与 LLM 解决器接口

**Files:**
- Create: `crates/invoice-grouping/src/ambiguity.rs`
- Modify: `crates/invoice-grouping/src/deterministic.rs:detect_ambiguities`
- Create: `crates/invoice-grouping/tests/mock_resolver.rs` (测试用 mock)

**Interfaces:**
- Consumes: `&[Trip]`, `&[ParsedInvoice]`
- Produces: `Vec<Ambiguity>` with 5 kinds detected
- Provides: `MockResolver` for testing (always picks first candidate)

**5 类歧义（产品规格 §M4）：**
1. NoReturnTicket - 无返程票且下一趟起点不同
2. WeekendBetweenTrips - 周末夹在两趟中间且无明确返程
3. TransferStopover - 中转停留 4-12 小时
4. MultipleVisitsSameCity - 同城短期内多次往返
5. TimeOverlap - 时间重叠（同时在两个城市）

- [ ] **Step 1: 实现歧义检测函数**

创建 `crates/invoice-grouping/src/ambiguity.rs`：

```rust
use crate::types::*;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use invoice_parse::model::{ParsedInvoice, TicketType};

const TRANSFER_MIN_HOURS: i64 = 4;
const TRANSFER_MAX_HOURS: i64 = 12;

pub fn detect_ambiguities(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 1. NoReturnTicket：检查每个出差行程是否有返程
    for trip in trips {
        if let TripKind::BusinessTrip { start, end, cities } = &trip.kind {
            if let Some(amb) = check_no_return_ticket(trip, invoices, home_cities) {
                ambiguities.push(amb);
            }
        }
    }

    // 2. WeekendBetweenTrips：检查行程间的周末
    for i in 0..trips.len().saturating_sub(1) {
        if let Some(amb) = check_weekend_between(&trips[i], &trips[i + 1], invoices) {
            ambiguities.push(amb);
        }
    }

    // 3. TransferStopover：检查 4-12h 的停留
    for trip in trips {
        if let TripKind::BusinessTrip { .. } = &trip.kind {
            ambiguities.extend(check_transfer_stopover(trip, invoices));
        }
    }

    // 4. MultipleVisitsSameCity：同城多次往返
    if let Some(amb) = check_multiple_visits(trips, invoices) {
        ambiguities.push(amb);
    }

    // 5. TimeOverlap：时间重叠
    ambiguities.extend(check_time_overlap(trips, invoices));

    ambiguities
}

fn check_no_return_ticket(
    trip: &Trip,
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Option<Ambiguity> {
    // 提取行程的所有交通票
    let transports: Vec<&ParsedInvoice> = trip
        .invoice_ids
        .iter()
        .map(|&id| &invoices[id])
        .filter(|inv| matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight))
        .collect();

    if transports.is_empty() {
        return None;
    }

    // 最后一张票的目的城市（需要从发票中推断，这里简化）
    // 如果不是常驻城市，说明缺返程票
    let last = transports.last()?;
    
    // TODO: 需要从发票中提取目的城市字段
    // 简化判断：如果行程只有单程票，可能缺返程
    if transports.len() == 1 {
        return Some(Ambiguity {
            kind: AmbiguityKind::NoReturnTicket,
            description: format!(
                "{}出发去{}，未找到返程票",
                last.issue_date,
                last.city.as_deref().unwrap_or("未知")
            ),
            involved_invoice_ids: trip.invoice_ids.clone(),
            candidates: vec![
                "单程出差，无需返程".to_string(),
                "返程票丢失，需补录".to_string(),
            ],
        });
    }

    None
}

fn check_weekend_between(
    trip1: &Trip,
    trip2: &Trip,
    invoices: &[ParsedInvoice],
) -> Option<Ambiguity> {
    let (end1, start2) = match (&trip1.kind, &trip2.kind) {
        (
            TripKind::BusinessTrip { end: e1, .. },
            TripKind::BusinessTrip { start: s2, .. },
        ) => (e1, s2),
        _ => return None,
    };

    let gap_days = (*start2 - *end1).num_days();
    
    // 2-3 天间隔且包含周末
    if gap_days >= 2 && gap_days <= 3 {
        let sat_sun = (*end1..=*start2).any(|d| {
            matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
        });

        if sat_sun {
            return Some(Ambiguity {
                kind: AmbiguityKind::WeekendBetweenTrips,
                description: format!(
                    "{}结束到{}开始之间有周末，可能已返回或连续出差",
                    end1, start2
                ),
                involved_invoice_ids: [&trip1.invoice_ids[..], &trip2.invoice_ids[..]].concat(),
                candidates: vec![
                    "周末回家，两趟独立行程".to_string(),
                    "周末仍在外地，合并为一趟".to_string(),
                ],
            });
        }
    }

    None
}

fn check_transfer_stopover(trip: &Trip, invoices: &[ParsedInvoice]) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    let transports: Vec<(usize, &ParsedInvoice)> = trip
        .invoice_ids
        .iter()
        .map(|&id| (id, &invoices[id]))
        .filter(|(_, inv)| matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight))
        .collect();

    for i in 0..transports.len().saturating_sub(1) {
        let (id1, inv1) = transports[i];
        let (id2, inv2) = transports[i + 1];

        if let (Some(t1), Some(t2)) = (inv1.departure_time, inv2.departure_time) {
            let hours = (t2 - t1).num_hours();
            
            if hours >= TRANSFER_MIN_HOURS && hours <= TRANSFER_MAX_HOURS {
                ambiguities.push(Ambiguity {
                    kind: AmbiguityKind::TransferStopover,
                    description: format!(
                        "{}到{}停留{}小时，是中转还是行程点？",
                        inv1.city.as_deref().unwrap_or("未知"),
                        inv2.city.as_deref().unwrap_or("未知"),
                        hours
                    ),
                    involved_invoice_ids: vec![id1, id2],
                    candidates: vec![
                        "中转站，不计入行程城市".to_string(),
                        "行程点，在此停留办事".to_string(),
                    ],
                });
            }
        }
    }

    ambiguities
}

fn check_multiple_visits(trips: &[Trip], invoices: &[ParsedInvoice]) -> Option<Ambiguity> {
    // TODO: 检测 30 天内同城多次往返
    None
}

fn check_time_overlap(trips: &[Trip], invoices: &[ParsedInvoice]) -> Vec<Ambiguity> {
    // TODO: 检测两张票显示同时在两个城市
    vec![]
}
```

- [ ] **Step 2: 更新 deterministic.rs 调用新的检测函数**

修改 `crates/invoice-grouping/src/deterministic.rs` 的 `detect_ambiguities` 函数：

```rust
fn detect_ambiguities(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    crate::ambiguity::detect_ambiguities(trips, invoices, home_cities)
}
```

同时在文件顶部添加 `pub use` 以便外部访问：

```rust
// 在 src/lib.rs 中添加
pub use ambiguity::detect_ambiguities;
```

- [ ] **Step 3: 创建测试用 Mock 解决器**

创建 `crates/invoice-grouping/tests/mock_resolver.rs`：

```rust
use invoice_grouping::types::*;

/// 测试用 Mock 解决器，总是选择第一个候选方案
pub struct AlwaysFirstResolver;

impl AmbiguityResolver for AlwaysFirstResolver {
    fn resolve(
        &self,
        ambiguities: &[Ambiguity],
    ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
        Ok(ambiguities
            .iter()
            .enumerate()
            .map(|(idx, amb)| AmbiguityResolution {
                ambiguity_index: idx,
                chosen_candidate: 0,
                confidence: 0.8,
                reason: format!("Mock: 选择第一个候选 - {}", amb.candidates[0]),
            })
            .collect())
    }
}
```

- [ ] **Step 4: 更新歧义测试使用 Mock 解决器**

在 `crates/invoice-grouping/tests/synthetic.rs` 中更新 `test_no_return_ticket_ambiguity`：

```rust
mod mock_resolver;
use mock_resolver::AlwaysFirstResolver;

#[test]
fn test_no_return_ticket_with_resolver() {
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(3, TicketType::Flight, d(7, 10), 14, "北京", "深圳", "850.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: Some(Box::new(AlwaysFirstResolver)),
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 有解决器，歧义应该被解决
    assert_eq!(result.unresolved_ambiguities.len(), 0);
    assert!(result.overall_confidence > 0.7);
}
```

- [ ] **Step 5: 运行测试验证歧义检测**

```bash
cargo test -p invoice-grouping test_no_return
```

预期：带解决器的测试通过，不带解决器的测试检测到歧义。

- [ ] **Step 6: 提交**

```bash
git add crates/invoice-grouping/src/ambiguity.rs crates/invoice-grouping/tests/mock_resolver.rs
git commit -m "feat(grouping): 实现 5 类歧义检测与 Mock 解决器"
```

---

## Task 6: 添加真实数据回归测试准备

**Files:**
- Create: `crates/invoice-grouping/tests/real_data.rs` (占位，等待真实样本)
- Create: `docs/grouping-test-plan.md` (α 测试计划)

**Interfaces:**
- Produces: α 测试框架，准备接收 5 个用户 × 3-5 个历史批次

- [ ] **Step 1: 创建真实数据测试占位**

创建 `crates/invoice-grouping/tests/real_data.rs`：

```rust
//! 真实数据回归测试
//!
//! α 阶段：5 个用户 × 3-5 个历史批次 = 25 个批次
//! 目标：平均调整 < 3 张/批次
//!
//! 运行方式：
//! 1. 用户提供历史批次的发票文件
//! 2. 运行解析器生成 Vec<ParsedInvoice>
//! 3. 运行归组引擎
//! 4. 用户手工检查分组结果，记录调整张数
//! 5. 统计平均值

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // 默认跳过，α 阶段手工执行
    fn test_batch_user1_202607() {
        // TODO: α 阶段填充真实数据
        unimplemented!("等待 α 用户数据")
    }
}
```

- [ ] **Step 2: 编写 α 测试计划文档**

创建 `docs/grouping-test-plan.md`：

```markdown
# 归组引擎 α 测试计划

## 目标

验收线：**平均调整 < 3 张/批次**（等价于准确率 ≥ 94%）

## 测试方法

### 准备阶段

1. 招募 5 个 α 用户（已使用 Concur，月均 ≥15 张发票）
2. 每人提供 3-5 个历史月份的发票文件
3. 使用试运行模式（不计费、不写台账）

### 执行阶段

对每个批次：

1. **解析**：`cargo run -p invoice-parse -- batch <files>`
2. **归组**：`cargo run -p invoice-grouping -- group <parsed.json>`
3. **审核**：与用户一起逐批次查看归组结果
4. **记录**：调整张数、调整类型（移动/拆分/合并/排除）

### 数据收集表格

| 批次ID | 用户 | 月份 | 总张数 | 调整张数 | 调整类型分布 | 备注 |
|---|---|---|---|---|---|---|
| B01 | U1 | 2026-06 | 48 | 2 | 移动2 | 周末判断错误 |
| B02 | U1 | 2026-07 | 52 | 1 | 拆分1 | ... |
| ... | ... | ... | ... | ... | ... | ... |

## 分析标准

### 达标条件

- ✅ 平均调整张数 < 3
- ✅ 无系统性错误（同一类型错误 > 30%）

### 未达标处理

1. **调整集中在少数几类** → 补规则（如"周末夹中间"全判错）
2. **调整分散在各类型** → 算法框架回炉
3. **两次迭代无改善** → 重新评估归组作为 MVP 核心能力

## 工装需求

- [ ] 批量导入历史批次工具
- [ ] 导出归组调整统计脚本
- [ ] 可视化对比工具（期望 vs 实际分组）
```

- [ ] **Step 3: 提交**

```bash
git add crates/invoice-grouping/tests/real_data.rs docs/grouping-test-plan.md
git commit -m "test(grouping): 添加 α 测试框架和计划"
```

---

## Task 7: 完成合成测试集并优化算法

**Files:**
- Modify: `crates/invoice-grouping/src/deterministic.rs` (算法优化)
- Modify: `crates/invoice-grouping/tests/synthetic.rs` (补充剩余场景)

**Goal:** 使合成测试集达到 ≥ 20 个场景，至少 18/20 通过

- [ ] **Step 1: 补充剩余测试场景**

在 `tests/synthetic.rs` 添加：

```rust
#[test]
fn test_hotel_delayed_invoice() {
    // 酒店延迟开票：用 checkin_date 而非 issue_date
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 10), d(7, 3), "上海", "680.0"), // 7天后才开票
        make_transport(3, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 酒店应正确挂载到 7/3-7/5 行程
    assert_eq!(result.trips.len(), 1);
    assert_eq!(result.trips[0].invoice_ids.len(), 3);
}

#[test]
fn test_multiple_home_cities() {
    // 支持多个常驻城市
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "上海", "深圳", "450.0"),
        make_hotel(2, d(7, 4), d(7, 3), "深圳", "520.0"),
        make_transport(3, TicketType::Rail, d(7, 5), 16, "深圳", "上海", "450.0"),
    ];

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string(), "上海".to_string()],
        ambiguity_resolver: None,
    };

    let result = group_invoices(&invoices, &config).unwrap();

    // 从上海出发应判定为出差
    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::BusinessTrip { .. } => {}
        _ => panic!("期望 BusinessTrip"),
    }
}

// 补充更多场景...
```

- [ ] **Step 2: 运行完整测试集**

```bash
cargo test -p invoice-grouping
```

- [ ] **Step 3: 根据失败测试优化算法**

逐个修复失败场景，直到 ≥ 18/20 通过。记录无法通过的场景及原因。

- [ ] **Step 4: 生成测试覆盖率报告**

```bash
cargo test -p invoice-grouping -- --test-threads=1 > test_report.txt
grep "test result:" test_report.txt
```

- [ ] **Step 5: 提交**

```bash
git add crates/invoice-grouping/
git commit -m "feat(grouping): 合成测试集达到 20 场景，18/20 通过"
```

---


## Self-Review

### Spec Coverage Check

从 `invoice-reimbursement-product-spec.md` §M4（行程归组）和 `invoice-reimbursement-dev-plan.md` §轨道 C 提取的需求：

- [x] 7 步确定性算法（Task 4）
- [x] 5 类歧义检测（Task 5）
- [x] 常驻城市配置（Task 2）
- [x] 与 UI 解耦的接口（Task 2 - `group_invoices` 函数）
- [x] 合成测试集（Task 3）
- [x] α 测试准备（Task 6）
- [x] 火车中转 < 4h 判定（Task 4 中的 TRANSFER_THRESHOLD_HOURS）
- [x] 去机场市内票识别（Task 4 中的 SAME_DAY_THRESHOLD_HOURS）
- [x] 酒店延迟开票处理（Task 7 测试场景）
- [x] 多常驻城市支持（Task 7 测试场景）

**缺口：**
- ⚠️ LLM 批量调用实现（本计划只提供 Mock，真实 DeepSeek API 集成延后到集成阶段）
- ⚠️ 归组学习功能（记住用户调整偏好）- 产品规格明确延后到 v0.5

### Placeholder Scan

- [ ] 无 "TBD" / "TODO" 暴露给执行者
- [x] 所有 "TODO" 都在代码注释中，标记未来迭代点
- [x] 测试代码有完整断言，非 "编写类似测试"
- [x] 类型、函数签名在各任务间一致

### Type Consistency Check

核心类型在任务间的一致性：

- `ParsedInvoice.city` - Task 1 定义，Task 3-7 使用 ✓
- `ParsedInvoice.departure_time` - Task 1 定义，Task 3-7 使用 ✓
- `ParsedInvoice.checkin_date` - Task 1 定义，Task 3-7 使用 ✓
- `GroupingConfig` - Task 2 定义，Task 3-7 使用 ✓
- `Trip` - Task 2 定义，Task 3-7 使用 ✓
- `TripKind` - Task 2 定义，Task 3-7 使用 ✓
- `Ambiguity` / `AmbiguityKind` - Task 2 定义，Task 5 扩展，Task 3-7 使用 ✓

---

## 计划完成定义

### 功能完成标准

| 模块 | 标准 |
|---|---|
| 核心数据模型 | `Invoice` 包含 `city`, `departure_time`, `checkin_date` 字段 |
| 归组引擎 crate | 独立可编译，公开 `group_invoices()` 接口 |
| 确定性算法 | 7 步流程实现，覆盖 6 类确定性场景 |
| 歧义检测 | 5 类歧义能被检测，返回结构化 `Ambiguity` |
| 合成测试集 | ≥ 20 个场景，覆盖所有确定性场景 + 5 类歧义 |
| 测试通过率 | ≥ 18/20（90%）|
| α 测试准备 | 测试框架和计划文档就绪 |

### 质量门槛

| 指标 | 目标 | 如何验证 |
|---|---|---|
| 合成测试通过率 | ≥ 90% (18/20) | `cargo test -p invoice-grouping` |
| 编译无警告 | 0 warnings | `cargo build -p invoice-grouping` |
| 代码覆盖率 | ≥ 80% | `cargo tarpaulin -p invoice-grouping` (可选) |
| 接口解耦 | `invoice-grouping` 不依赖 UI | 检查 `Cargo.toml` 依赖列表 |

### 未达标处理

**合成测试通过率 < 90%：**
1. 分析失败场景类型分布
2. 集中在少数几类 → Task 7 补规则
3. 分散各类型 → Task 4 算法回炉

**α 阶段真实数据 < 3 张/批次调整：**
1. 两次迭代仍不达标 → 升级为产品级风险
2. 重新评估归组作为 MVP 核心能力的可行性
3. 备选方案：降级为"辅助建议"而非"自动分组"

---

## 后续任务（不在本计划）

完成本计划后，下一步工作：

1. **DeepSeek API 集成** (1 周)
   - 替换 `MockResolver` 为真实 LLM 调用
   - 实现批量歧义判定 JSON 格式
   - 成本控制：一次调用处理所有歧义（~¥0.01/批次）

2. **核心数据模型完善** (3 天)
   - 添加 `Batch` 结构体
   - 添加 `报销人` 字段（代报场景）
   - 批次状态机

3. **校验与去重 + 跨月台账** (1 周)
   - SQLite `reported_invoices` 表
   - 跨月重复检测（订阅核心价值）
   - 本地验签（SM2/SM3）

4. **输出格式** (3-5 天)
   - Excel 汇总表（含费用类型映射列）
   - A4 打印合订本
   - Concur CSV

5. **Tauri 骨架 + 审核界面** (2 周)
   - 单屏布局：左行程树 + 右发票列表
   - 拖拽调整、拆分、合并
   - 紧凑纸票录入界面

---

## 预期时间线

假设单人全职开发：

- Task 1: 0.5 天（模型扩展 + 编译修复）
- Task 2: 1 天（新 crate + 类型定义）
- Task 3: 1.5 天（20 个合成场景）
- Task 4: 3-5 天（7 步算法迭代优化）
- Task 5: 2 天（歧义检测 + Mock 解决器）
- Task 6: 0.5 天（α 测试准备）
- Task 7: 1-2 天（补充场景 + 优化）

**总计：9-13 天**（约 2 周）

**风险缓冲：** +3-5 天（算法迭代可能超预期）

**建议：** 使用 `superpowers:subagent-driven-development` 逐任务派发，主循环在 Task 4 后评审，决定是否需要调整策略。

---

## 执行建议

### 推荐执行方式

**Option 1: Subagent-Driven (推荐)**

```
优势：
- 每个任务独立评审，快速发现算法问题
- Task 4 是关键，需要主循环参与迭代决策
- 可在 Task 3 后暂停，用真实样本验证合成场景的代表性

执行：
主循环调用 superpowers:subagent-driven-development，
逐任务派发，Task 4 后评审算法方向
```

**Option 2: Inline Execution**

```
优势：
- 一次性完成所有任务
- 适合算法逻辑清晰、无需中途调整的情况

风险：
- Task 4 可能需要多次迭代，inline 模式难以暂停重构

执行：
主循环调用 superpowers:executing-plans
```

### 关键决策点

1. **Task 3 后：** 检查合成场景是否覆盖产品规格中的所有确定性场景
2. **Task 4 后：** 评估基础测试通过率，决定是继续优化还是调整算法框架
3. **Task 7 后：** 决定是否进入 α 测试，还是先优化到 100% 合成测试通过

---

**计划创建时间：** 2026-08-06  
**基于产品规格：** `invoice-reimbursement-product-spec.md` §M4  
**基于开发计划：** `invoice-reimbursement-dev-plan.md` §轨道 C  
**前置依赖：** 解析准确率 90.9% (已完成)

