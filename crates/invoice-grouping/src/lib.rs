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
    _invoices: &[ParsedInvoice],
    _config: &GroupingConfig,
) -> Result<GroupingResult, anyhow::Error> {
    // TODO: Task 3-5 实现
    Ok(GroupingResult {
        trips: vec![],
        unresolved_ambiguities: vec![],
        overall_confidence: 0.0,
    })
}
