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
    /// 快递/物流：与市内消费和差旅费用分开，按实际发生月份归组。
    CourierMonth { year: i32, month: u32 },
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
    /// 行程开始日期
    pub start_date: NaiveDate,
    /// 行程结束日期
    pub end_date: NaiveDate,
    /// 置信度 0.0-1.0
    pub confidence: f32,
}

/// 归组配置
pub struct GroupingConfig {
    /// 常驻城市列表（支持多个，如 ["北京", "上海"]）
    pub home_cities: Vec<String>,
    /// 用户维护的常驻城市车站别名。`None` 使用随程序发布的默认库；`Some` 表示用户库为准。
    pub home_station_aliases: Option<Vec<StationCityAlias>>,
    /// 歧义解决器（可 mock，生产环境用 LLM）
    pub ambiguity_handler: Box<dyn AmbiguityResolver>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationCityAlias {
    pub station_name: String,
    pub city_name: String,
}

impl std::fmt::Debug for GroupingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupingConfig")
            .field("home_cities", &self.home_cities)
            .field("home_station_aliases", &self.home_station_aliases)
            .field("ambiguity_handler", &"<resolver>")
            .finish()
    }
}

/// 歧义解决器 trait（便于测试 mock）
pub trait AmbiguityResolver: Send + Sync {
    fn resolve(&self, ambiguities: &[Ambiguity])
        -> Result<Vec<AmbiguityResolution>, anyhow::Error>;
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
    /// 同一笔非交通费用同时满足多个行程的日期/地点条件
    MultipleTripMatch,
    /// 异地住宿形成了出差候选，但员工没有个人交通票据。
    MissingTransportEvidence,
    /// 退票/改签费用无法唯一匹配到所属出差。
    TransportAdjustmentMatch,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupingResult {
    pub trips: Vec<Trip>,
    pub ambiguities: Vec<Ambiguity>,
    pub overall_confidence: f32,
}
