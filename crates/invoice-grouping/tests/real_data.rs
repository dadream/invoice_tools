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
    use invoice_grouping::group_invoices;
    use invoice_grouping::types::{
        Ambiguity, AmbiguityResolution, AmbiguityResolver, GroupingConfig, TripKind,
    };
    use invoice_parse::model::ParsedInvoice;
    use std::path::Path;

    struct NoopResolver;

    impl AmbiguityResolver for NoopResolver {
        fn resolve(
            &self,
            _ambiguities: &[Ambiguity],
        ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

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

    /// 私有真实原件只通过显式环境变量启用；仅输出聚合数量，不输出文件名、
    /// 票号、乘车人、站点或日期。
    #[test]
    #[ignore = "requires an explicitly authorized private PDF sample directory"]
    fn private_rail_invoices_create_business_trip_groups() {
        let root = std::env::var_os("INVOICE_REAL_GROUPING_PDF_ROOT")
            .expect("INVOICE_REAL_GROUPING_PDF_ROOT is required");
        let expected: usize = std::env::var("INVOICE_REAL_GROUPING_RAIL_EXPECTED")
            .expect("INVOICE_REAL_GROUPING_RAIL_EXPECTED is required")
            .parse()
            .expect("INVOICE_REAL_GROUPING_RAIL_EXPECTED must be an integer");
        let mut invoices = Vec::new();
        for entry in std::fs::read_dir(root).expect("private PDF directory must be readable") {
            let path = entry
                .expect("private directory entry must be readable")
                .path();
            if !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            {
                continue;
            }
            let bytes = std::fs::read(&path).expect("private PDF must be readable");
            if let Ok(invoice) = invoice_parse::pdf_embedded::parse_embedded_rail_invoice(
                &bytes,
                Path::new("private-sample.pdf"),
            ) {
                invoices.push(invoice);
            }
        }

        let result = group_invoices(
            &invoices,
            &GroupingConfig {
                home_cities: vec!["北京".to_string()],
                home_station_aliases: None,
                ambiguity_handler: Box::new(NoopResolver),
            },
        )
        .expect("private rail grouping must complete");
        let business_trips = result
            .trips
            .iter()
            .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
            .count();

        println!("rail_invoices={}", invoices.len());
        println!("business_trip_groups={business_trips}");
        println!("private_values_logged=false");
        assert_eq!(invoices.len(), expected);
        assert!(
            business_trips > 0,
            "rail invoices must not all fall into local-month groups"
        );
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
