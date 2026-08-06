# Task 5 实施报告

**任务**: 实现 5 类歧义检测与 Mock 解决器  
**实施日期**: 2026-08-06  
**状态**: ✅ 已完成

---

## 完成情况

- [x] 创建 `src/ambiguity.rs`（340 行）
- [x] 创建 `tests/mock_resolver.rs`（46 行）
- [x] 修改 `src/deterministic.rs`（移除内联检测代码，调用新模块）
- [x] 修改 `src/lib.rs`（导出 ambiguity 模块）
- [x] 清理未使用代码（移除 `build_city_chain` 旧版本和 `SAME_DAY_THRESHOLD_HOURS` 常量）

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

## 技术细节

### 1. 模块架构

创建独立的 `ambiguity.rs` 模块，职责清晰：
- 主入口函数 `detect_ambiguities()` 调用 5 个检测函数
- 每个检测函数对应一类歧义
- 辅助函数 `is_home_city()` 和 `extract_destination()` 支持城市判断和目的地提取

### 2. 五类歧义检测实现

#### (1) NoReturnTicket - 无返程票
- **逻辑**: 检查每个出差行程的最后一张交通票目的地是否为常驻城市
- **候选方案**: "行程未结束，等待下月数据" / "行程已结束，未录入返程票"

#### (2) TimeOverlap - 时间重叠
- **逻辑**: 检测同一天是否有多张交通票去往不同城市
- **实现**: 使用 `HashMap<NaiveDate, Vec<(usize, &ParsedInvoice)>>` 按日期分组
- **候选方案**: "同事代订票据" / "退改签重复"

#### (3) TransferStopover - 中转停留（4-12h 灰色区间）
- **逻辑**: 检测连续交通票间隔在 4-12 小时的情况
- **常量**: `TRANSFER_THRESHOLD_HOURS = 4`, `STOPOVER_THRESHOLD_HOURS = 12`
- **候选方案**: "中转点，不计入行程城市" / "行程点，计入行程城市"

#### (4) WeekendBetweenTrips - 周末夹缝
- **逻辑**: 检测从常驻城市出发的两次行程间隔 2-4 天，且跨越周末（周五到周一）
- **实现**: 使用 `chrono::Weekday::number_from_monday()` 判断星期几
- **候选方案**: "周末回家了，两次独立出差" / "周末留在外地，连续出差"

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
