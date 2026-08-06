# Task 5 实施报告

**任务**: 实现 5 类歧义检测与 Mock 解决器  
**实施日期**: 2026-08-06  
**状态**: ✅ 已完成（含审查修复）

---

## 完成情况

- [x] 创建 `src/ambiguity.rs`（318 行，优化后）
- [x] 创建 `tests/mock_resolver.rs`（46 行）
- [x] 修改 `src/deterministic.rs`（移除内联检测代码，调用新模块）
- [x] 修改 `src/lib.rs`（导出 ambiguity 模块）
- [x] 清理未使用代码（移除 `build_city_chain` 旧版本和 `SAME_DAY_THRESHOLD_HOURS` 常量）
- [x] **审查修复**：修复 2 个 Critical + 3 个 Important 问题

## 审查修复记录

### Critical 修复

**C1 - TransferStopover 缺失酒店检查**
- 问题：未检查"该城市无酒店发票"（Brief 明确要求）
- 修复：添加 `has_hotel` 检查逻辑，只有无酒店时才报告歧义
- 影响：提高歧义检测准确性，避免误报

**C2 - 代码重复**
- 问题：`is_home_city()` 和 `extract_destination()` 在两个模块中重复定义
- 修复：将辅助函数移到 `deterministic.rs` 并设为 `pub(crate)`，`ambiguity.rs` 通过 `use` 导入
- 影响：减少 24 行重复代码，提高可维护性

### Important 修复

**I1 - WeekendBetweenTrips 逻辑偏离**
- 问题：实现检查"从常驻城市的两次出发"而非"两趟行程间隔"，间隔范围 2-4 天超出 Brief 的 2-3 天
- 修复：改为遍历所有 BusinessTrip，检查 `trip1.end_date` 到 `trip2.start_date` 的间隔，范围改为 2-3 天
- 影响：符合 Brief 规格，检测更精确

**I2 - 候选方案文本不符合 Brief**
- 问题：NoReturnTicket 和 TimeOverlap 的候选文本与 Brief 不一致
- 修复：
  - NoReturnTicket: "单程出差，无需返程" / "返程票丢失，需补录"
  - TimeOverlap: "第一张票作废/改签" / "并行出差（特殊情况）"
  - TransferStopover: "中转站，不计入行程城市" / "行程点，在此停留办事"
- 影响：输出文本与设计文档完全一致

**I3 - 测试断言过于宽松**
- 问题：5 个测试使用 `||` 逻辑或未验证 `AmbiguityKind`，无法准确验证检测结果
- 修复：加强以下测试的断言
  - `test_weekend_between_trips_ambiguity`: 必须检测到 WeekendBetweenTrips 且行程数为 2
  - `test_transfer_stopover_ambiguity`: 必须检测到 TransferStopover 且郑州不在城市链中
  - `test_one_way_with_hotel_only`: 必须检测到 NoReturnTicket
  - `test_single_transport_ticket`: 必须检测到 NoReturnTicket
  - `test_no_return_at_end_of_month`: 必须检测到 NoReturnTicket
- 影响：测试更严格，确保检测逻辑正确

## 测试结果

**通过率**: 20/20 (100%)

### 5 个歧义测试状态

| 测试名称 | 歧义类型 | 状态 |
|---------|---------|------|
| `test_no_return_ticket_ambiguity` | NoReturnTicket | ✅ PASS |
| `test_weekend_between_trips_ambiguity` | WeekendBetweenTrips | ✅ PASS |
| `test_transfer_stopover_ambiguity` | TransferStopover | ✅ PASS |
| `test_multiple_visits_same_city_ambiguity` | MultipleVisitsSameCity | ✅ PASS |
| `test_time_overlap_ambiguity` | TimeOverlap | ✅ PASS |

### 所有测试列表

```
test test_airport_taxi_attached_to_trip ... ok
test test_cross_month_trip ... ok
test test_empty_invoice_list ... ok
test test_local_month_only ... ok
test test_long_duration_trip ... ok
test test_mixed_trips_and_local ... ok
test test_multi_city_trip ... ok
test test_multiple_home_cities ... ok
test test_multiple_visits_same_city_ambiguity ... ok
test test_no_return_at_end_of_month ... ok
test test_no_return_ticket_ambiguity ... ok
test test_one_way_with_hotel_only ... ok
test test_single_transport_ticket ... ok
test test_single_trip_with_return ... ok
test test_stopover_beyond_12h ... ok
test test_time_overlap_ambiguity ... ok
test test_transfer_stopover_ambiguity ... ok
test test_transfer_stopover_within_4h ... ok
test test_two_separate_trips ... ok
test test_weekend_between_trips_ambiguity ... ok
```

## 提交哈希

- `38ae7f1` - feat(grouping): 实现 5 类歧义检测与 Mock 解决器
- `7a35c95` - docs: Task 5 实施报告
- `7c8fdeb` - fix(grouping): 修复歧义检测的 5 个问题（审查修复）

## 技术细节

### 1. 模块架构

创建独立的 `ambiguity.rs` 模块，职责清晰：
- 主入口函数 `detect_ambiguities()` 调用 5 个检测函数
- 每个检测函数对应一类歧义
- 辅助函数 `is_home_city()` 和 `extract_destination()` 位于 `deterministic.rs`，设为 `pub(crate)` 供跨模块使用

### 2. 五类歧义检测实现

#### (1) NoReturnTicket - 无返程票
- **逻辑**: 检查每个出差行程的最后一张交通票目的地是否为常驻城市
- **候选方案**: "单程出差，无需返程" / "返程票丢失，需补录"

#### (2) TimeOverlap - 时间重叠
- **逻辑**: 检测同一天是否有多张交通票去往不同城市
- **实现**: 使用 `HashMap<NaiveDate, Vec<(usize, &ParsedInvoice)>>` 按日期分组
- **候选方案**: "第一张票作废/改签" / "并行出差（特殊情况）"

#### (3) TransferStopover - 中转停留（4-12h 灰色区间）
- **逻辑**: 检测连续交通票间隔在 4-12 小时且该城市无酒店发票的情况
- **常量**: `TRANSFER_THRESHOLD_HOURS = 4`, `STOPOVER_THRESHOLD_HOURS = 12`
- **酒店检查**: 使用 `checkin_date == curr_time.date()` 匹配当天酒店
- **候选方案**: "中转站，不计入行程城市" / "行程点，在此停留办事"

#### (4) WeekendBetweenTrips - 周末夹缝
- **逻辑**: 检测两趟出差行程间隔 2-3 天，且间隔中包含周六或周日
- **实现**: 遍历所有 BusinessTrip，检查 `trip1.end_date` 到 `trip2.start_date` 的间隔
- **周末检测**: 遍历间隔天数，使用 `chrono::Weekday::Sat | Sun` 判断
- **候选方案**: "周末回家，两趟独立行程" / "周末仍在外地，合并为一趟"

#### (5) MultipleVisitsSameCity - 同城多次往返（新增）
- **逻辑**: 检测 30 天内 3 次以上访问同一非常驻城市，且每次停留 < 7 天
- **常量**: 
  - `MULTIPLE_VISITS_WINDOW_DAYS = 30`
  - `MULTIPLE_VISITS_MAX_STAY_DAYS = 7`
  - `MULTIPLE_VISITS_MIN_COUNT = 3`
- **实现**: 使用滑动窗口统计城市访问频次
- **候选方案**: "同一客户多次拜访，合并为一趟" / "不同客户/项目，拆分为独立行程"

### 3. Mock 解决器

提供两个测试用解决器：

#### AlwaysFirstResolver
- 总是选择第一个候选方案
- 置信度固定为 0.8
- 理由格式: `"Mock: 选择第一个候选 - {候选文本}"`

#### AlwaysLastResolver
- 总是选择最后一个候选方案
- 置信度固定为 0.8
- 理由格式: `"Mock: 选择最后一个候选 - {候选文本}"`

### 4. 代码重构

#### deterministic.rs 简化
- **移除前**: 374-570 行（197 行内联检测代码）
- **移除后**: 调用 `crate::ambiguity::detect_ambiguities()` 仅 7 行
- **净减少**: ~190 行代码

#### 清理未使用代码
- 移除 `build_city_chain()` 旧版本函数（已被 `build_city_chain_with_ambiguities` 取代）
- 移除 `SAME_DAY_THRESHOLD_HOURS` 未使用常量

### 5. 测试覆盖

所有 5 类歧义均有对应的集成测试：
- 确定性场景测试：15 个（验证正常归组逻辑）
- 歧义检测测试：5 个（验证歧义识别准确性）

## 代码质量

- ✅ 无编译警告
- ✅ 使用简体中文注释
- ✅ 遵循 Rust 命名约定
- ✅ 使用 `rust_decimal::Decimal` 处理金额
- ✅ 使用 `chrono::NaiveDate` 处理日期
- ✅ 符合 Conventional Commits 规范

## 架构改进

1. **关注点分离**: 歧义检测逻辑从 `deterministic.rs` 分离到独立模块
2. **可测试性**: Mock 解决器便于单元测试和集成测试
3. **可扩展性**: 新增歧义类型只需在 `ambiguity.rs` 添加检测函数
4. **代码复用**: 辅助函数 `is_home_city()` 和 `extract_destination()` 避免重复代码

## 下一步

- Task 6: 准备真实数据测试（使用 `fixtures/manifest.toml` 中的发票样本）
- Task 7: 集成 LLM 解决器（替换 Mock 解决器，调用真实 LLM API）

---

**实施者**: Claude Opus 5  
**验证**: cargo test -p invoice-grouping (20/20 通过)
