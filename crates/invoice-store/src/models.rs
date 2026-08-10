//! 数据库模型定义
//!
//! 包含账号、凭证、批次、发票等核心数据模型

use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// 邮箱账号
///
/// 存储在 accounts.db 的 accounts 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// 账号 ID（主键）
    pub id: i64,

    /// 邮箱地址
    pub email: String,

    /// IMAP 服务器地址
    pub imap_server: String,

    /// IMAP 端口
    pub imap_port: u16,

    /// 是否启用（用于临时禁用账号）
    pub enabled: bool,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,
}

/// 邮箱凭证（加密存储）
///
/// 存储在 accounts.db 的 credentials 表
#[derive(Debug, Clone)]
pub struct Credential {
    /// 凭证 ID（主键）
    pub id: i64,

    /// 关联的账号 ID（外键）
    pub account_id: i64,

    /// 加密后的密码（格式：[nonce || ciphertext || tag]）
    pub encrypted_password: Vec<u8>,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,
}

/// 报销批次
///
/// 存储在 ledger.db 的 batches 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    /// 批次 ID（主键）
    pub id: i64,

    /// 批次名称（如 "2026年7月出差"）
    pub name: String,

    /// 批次月份（用于归组，格式 YYYY-MM）
    pub month: String,

    /// 批次状态
    pub status: BatchStatus,

    /// 批次总金额
    pub total_amount: Decimal,

    /// 发票张数
    pub invoice_count: i32,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,

    /// 提交时间（状态变为 Submitted 时）
    pub submitted_at: Option<NaiveDateTime>,
}

/// 批次状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchStatus {
    /// 草稿（正在编辑）
    Draft,

    /// 已提交（等待审核）
    Submitted,

    /// 已批准（等待打款）
    Approved,

    /// 已完成（已打款）
    Completed,

    /// 已拒绝
    Rejected,
}

impl BatchStatus {
    /// 转换为数据库存储的整数
    pub fn to_i32(self) -> i32 {
        match self {
            BatchStatus::Draft => 0,
            BatchStatus::Submitted => 1,
            BatchStatus::Approved => 2,
            BatchStatus::Completed => 3,
            BatchStatus::Rejected => 4,
        }
    }

    /// 从数据库整数转换
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(BatchStatus::Draft),
            1 => Some(BatchStatus::Submitted),
            2 => Some(BatchStatus::Approved),
            3 => Some(BatchStatus::Completed),
            4 => Some(BatchStatus::Rejected),
            _ => None,
        }
    }
}

/// 已报销的发票记录
///
/// 存储在 ledger.db 的 reported_invoices 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportedInvoice {
    /// 记录 ID（主键）
    pub id: i64,

    /// 关联的批次 ID（外键）
    pub batch_id: i64,

    /// 发票号码（20 位）
    pub invoice_number: String,

    /// 开票日期
    pub issue_date: NaiveDate,

    /// 发票金额
    pub amount: Decimal,

    /// 税额
    pub tax_amount: Option<Decimal>,

    /// 购方名称
    pub buyer_name: Option<String>,

    /// 销方名称
    pub seller_name: Option<String>,

    /// 票据类型
    pub ticket_type: TicketType,

    /// 出发城市（交通票）
    pub city: Option<String>,

    /// 出发时间（交通票）
    pub departure_time: Option<NaiveDateTime>,

    /// 入住日期（酒店）
    pub checkin_date: Option<NaiveDate>,

    /// 发票文件路径
    pub file_path: String,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,
}

/// 票据类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketType {
    /// 火车票
    Rail,

    /// 飞机票
    Flight,

    /// 酒店
    Hotel,

    /// 城市交通（出租车、地铁、公交）
    CityTransport,

    /// 餐饮
    Meal,

    /// 其他
    Other,
}

impl TicketType {
    /// 转换为数据库存储的字符串
    pub fn to_str(self) -> &'static str {
        match self {
            TicketType::Rail => "rail",
            TicketType::Flight => "flight",
            TicketType::Hotel => "hotel",
            TicketType::CityTransport => "city_transport",
            TicketType::Meal => "meal",
            TicketType::Other => "other",
        }
    }

    /// 从数据库字符串转换
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "rail" => Some(TicketType::Rail),
            "flight" => Some(TicketType::Flight),
            "hotel" => Some(TicketType::Hotel),
            "city_transport" => Some(TicketType::CityTransport),
            "meal" => Some(TicketType::Meal),
            "other" => Some(TicketType::Other),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_status_conversion() {
        assert_eq!(BatchStatus::Draft.to_i32(), 0);
        assert_eq!(BatchStatus::Submitted.to_i32(), 1);
        assert_eq!(BatchStatus::from_i32(0), Some(BatchStatus::Draft));
        assert_eq!(BatchStatus::from_i32(99), None);
    }

    #[test]
    fn ticket_type_conversion() {
        assert_eq!(TicketType::Rail.to_str(), "rail");
        assert_eq!(TicketType::Flight.to_str(), "flight");
        assert_eq!(TicketType::from_str("rail"), Some(TicketType::Rail));
        assert_eq!(TicketType::from_str("unknown"), None);
    }

    #[test]
    fn ticket_type_roundtrip() {
        let types = [
            TicketType::Rail,
            TicketType::Flight,
            TicketType::Hotel,
            TicketType::CityTransport,
            TicketType::Meal,
            TicketType::Other,
        ];

        for ticket_type in types {
            let s = ticket_type.to_str();
            let parsed = TicketType::from_str(s);
            assert_eq!(parsed, Some(ticket_type));
        }
    }
}
