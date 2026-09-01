pub mod expense_classifier;
pub mod field_extractor;
pub mod manifest;
pub mod model;
pub mod ocr;
pub mod ocr_worker_protocol;
pub mod ofd;
pub mod ofd_preview;
pub mod ofd_text;
pub mod pdf;
pub mod pdf_embedded;
pub mod pdf_ocr;
pub mod pdf_preview;
pub mod pdf_text;
pub mod report;
pub mod station_city;
pub mod verify;
pub mod windows_security;
pub mod xml;

/// 解析器实现版本，供持久化解析结果记录生成器版本。
pub const PARSER_VERSION: &str = env!("CARGO_PKG_VERSION");
