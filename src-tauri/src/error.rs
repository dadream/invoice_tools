use serde::{Deserialize, Serialize};

/// 错误分类。与前端 `ui/src/lib/ipc.ts` 的 `ErrorKind` 联动，改动需同步两侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Database,
    Parse,
    Network,
    Io,
    Validation,
    Internal,
}

impl ErrorKind {
    /// 是否值得让用户重试。不可恢复的错误应引导用户查看日志或反馈。
    pub fn recoverable(&self) -> bool {
        matches!(self, ErrorKind::Network | ErrorKind::Validation | ErrorKind::Io)
    }

    fn label(&self) -> &'static str {
        match self {
            ErrorKind::Database => "数据库错误",
            ErrorKind::Parse => "解析错误",
            ErrorKind::Network => "网络错误",
            ErrorKind::Io => "文件错误",
            ErrorKind::Validation => "验证错误",
            ErrorKind::Internal => "内部错误",
        }
    }
}

/// 跨 IPC 传递的统一错误。序列化形状固定为
/// `{"kind":"validation","message":"...","recoverable":true}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    kind: ErrorKind,
    message: String,
    /// 由 `kind` 推导，冗余写入以便前端无需维护映射表。
    #[serde(default)]
    recoverable: bool,
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), recoverable: kind.recoverable() }
    }

    #[allow(dead_code)]
    pub fn database(message: impl Into<String>) -> Self { Self::new(ErrorKind::Database, message) }
    #[allow(dead_code)]
    pub fn parse(message: impl Into<String>) -> Self { Self::new(ErrorKind::Parse, message) }
    #[allow(dead_code)]
    pub fn network(message: impl Into<String>) -> Self { Self::new(ErrorKind::Network, message) }
    #[allow(dead_code)]
    pub fn io(message: impl Into<String>) -> Self { Self::new(ErrorKind::Io, message) }
    #[allow(dead_code)]
    pub fn validation(message: impl Into<String>) -> Self { Self::new(ErrorKind::Validation, message) }
    #[allow(dead_code)]
    pub fn internal(message: impl Into<String>) -> Self { Self::new(ErrorKind::Internal, message) }

    #[allow(dead_code)]
    pub fn kind(&self) -> ErrorKind { self.kind }
    #[allow(dead_code)]
    pub fn message(&self) -> &str { &self.message }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.message)
    }
}

impl std::error::Error for AppError {}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::io(err.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_frontend_contract() {
        let err = AppError::validation("姓名不能为空");
        let json: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "validation");
        assert_eq!(json["message"], "姓名不能为空");
        assert_eq!(json["recoverable"], true);
    }

    #[test]
    fn internal_errors_are_not_recoverable() {
        let json = serde_json::to_value(AppError::internal("boom")).unwrap();
        assert_eq!(json["kind"], "internal");
        assert_eq!(json["recoverable"], false);
    }

    #[test]
    fn network_and_io_are_recoverable() {
        assert!(ErrorKind::Network.recoverable());
        assert!(ErrorKind::Io.recoverable());
        assert!(ErrorKind::Validation.recoverable());
        assert!(!ErrorKind::Database.recoverable());
        assert!(!ErrorKind::Parse.recoverable());
        assert!(!ErrorKind::Internal.recoverable());
    }

    #[test]
    fn display_is_chinese_and_includes_message() {
        assert_eq!(AppError::database("连接超时").to_string(), "数据库错误: 连接超时");
    }

    #[test]
    fn converts_from_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err: AppError = io.into();
        assert_eq!(serde_json::to_value(&err).unwrap()["kind"], "io");
    }

    #[test]
    fn converts_from_anyhow() {
        let err: AppError = anyhow::anyhow!("unexpected").into();
        assert_eq!(serde_json::to_value(&err).unwrap()["kind"], "internal");
    }

    #[test]
    fn roundtrips_through_json() {
        let original = AppError::parse("字段缺失");
        let text = serde_json::to_string(&original).unwrap();
        let back: AppError = serde_json::from_str(&text).unwrap();
        assert_eq!(back.kind(), ErrorKind::Parse);
        assert_eq!(back.message(), "字段缺失");
    }
}
