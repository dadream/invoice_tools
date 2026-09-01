pub mod ambiguity;
mod deterministic;
pub mod types;

use invoice_parse::model::ParsedInvoice;
use types::*;

/// 持久化在归组结果与输出任务键中的确定性规则版本。
pub const GROUPING_RULE_VERSION: &str = "deterministic-v2";

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
    // Step 1-6: 确定性算法
    let (trips, ambiguities) = deterministic::group_deterministic(invoices, config);

    // Step 7: 如果有歧义且提供了解决器，调用处理器
    if !ambiguities.is_empty() {
        let resolutions = config.ambiguity_handler.resolve(&ambiguities)?;
        // TODO: 应用解决方案调整行程（Task 5）
        // 目前简单返回未解决的歧义
        let _ = resolutions; // 暂时不使用
    }

    let overall_confidence = calculate_confidence(&trips);

    Ok(GroupingResult {
        trips,
        ambiguities,
        overall_confidence,
    })
}

/// 计算整体置信度
fn calculate_confidence(trips: &[Trip]) -> f32 {
    if trips.is_empty() {
        return 0.0;
    }
    trips.iter().map(|t| t.confidence).sum::<f32>() / trips.len() as f32
}
