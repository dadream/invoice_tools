//! 真实数据回归测试
//!
//! α 阶段：5 个用户 × 3-5 个历史批次 = 15-25 个批次
//! 目标：平均调整 < 3 张/批次
//!
//! 运行方式：
//! 1. 用户提供历史批次的发票文件
//! 2. 运行解析器生成 Vec<ParsedInvoice>
//! 3. 运行归组引擎
//! 4. 用户手工检查分组结果，记录调整张数
//! 5. 统计平均值
//!
//! 注意：这些测试默认被跳过（#[ignore]），需要在 α 测试阶段手工执行

#[cfg(test)]
mod tests {
    use invoice_grouping::{group_invoices, types::GroupingConfig};
    use invoice_parse::model::ParsedInvoice;

    /// 占位测试：用户1，2026年7月批次
    ///
    /// α 阶段需要填充：
    /// - 实际发票数据（从解析结果加载）
    /// - 预期分组数量
    /// - 每个分组的预期发票张数
    #[test]
    #[ignore] // 默认跳过，α 阶段手工执行
    fn test_batch_user1_202607() {
        // TODO: α 阶段填充真实数据
        // let invoices = load_parsed_invoices("fixtures/alpha/user1_202607.json");
        // let config = GroupingConfig::default();
        // let result = group_invoices(&invoices, &config).unwrap();
        // assert_eq!(result.trips.len(), EXPECTED_GROUP_COUNT);
        unimplemented!("等待 α 用户数据")
    }

    /// 占位测试：用户1，2026年8月批次
    #[test]
    #[ignore]
    fn test_batch_user1_202608() {
        unimplemented!("等待 α 用户数据")
    }

    /// 占位测试：用户2，2026年7月批次
    #[test]
    #[ignore]
    fn test_batch_user2_202607() {
        unimplemented!("等待 α 用户数据")
    }

    /// 占位测试：用户2，2026年8月批次
    #[test]
    #[ignore]
    fn test_batch_user2_202608() {
        unimplemented!("等待 α 用户数据")
    }

    /// 占位测试：用户3，2026年7月批次
    #[test]
    #[ignore]
    fn test_batch_user3_202607() {
        unimplemented!("等待 α 用户数据")
    }

    /// 辅助函数示例：从 JSON 文件加载解析结果
    ///
    /// α 阶段实现：
    /// ```rust,ignore
    /// fn load_parsed_invoices(path: &str) -> Vec<ParsedInvoice> {
    ///     let json = std::fs::read_to_string(path).unwrap();
    ///     serde_json::from_str(&json).unwrap()
    /// }
    /// ```
    #[allow(dead_code)]
    fn _example_load_parsed_invoices(_path: &str) -> Vec<ParsedInvoice> {
        unimplemented!("α 阶段实现")
    }

    /// 辅助函数示例：验证分组质量
    ///
    /// α 阶段实现：
    /// - 检查分组数量
    /// - 验证每个分组的时间跨度
    /// - 检查票据类型一致性
    #[allow(dead_code)]
    fn _example_assert_grouping_quality(
        _trips: &[invoice_grouping::types::Trip],
        _expected_count: usize,
    ) {
        unimplemented!("α 阶段实现")
    }
}
