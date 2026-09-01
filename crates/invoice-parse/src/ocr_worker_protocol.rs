use crate::model::{ParsedInvoice, TicketType};
use crate::pdf::SupportingDocumentFacts;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const OCR_WORKER_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrWorkerOperation {
    #[default]
    Invoice,
    SupportingDocument,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrWorkerRequest {
    pub protocol_version: u32,
    pub input_path: PathBuf,
    pub asset_dir: PathBuf,
    pub ticket_type: TicketType,
    #[serde(default)]
    pub operation: OcrWorkerOperation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OcrWorkerResponse {
    Success {
        invoice: ParsedInvoice,
    },
    SupportingDocumentSuccess {
        facts: Option<SupportingDocumentFacts>,
    },
    Failure {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_without_losing_windows_paths() {
        let request = OcrWorkerRequest {
            protocol_version: OCR_WORKER_PROTOCOL_VERSION,
            input_path: PathBuf::from(r"C:\发票 样本\invoice.png"),
            asset_dir: PathBuf::from(r"C:\程序\ocr"),
            ticket_type: TicketType::Other,
            operation: OcrWorkerOperation::SupportingDocument,
        };
        let json = serde_json::to_vec(&request).unwrap();
        let restored: OcrWorkerRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored.protocol_version, OCR_WORKER_PROTOCOL_VERSION);
        assert_eq!(restored.input_path, request.input_path);
        assert_eq!(restored.asset_dir, request.asset_dir);
        assert_eq!(restored.ticket_type, TicketType::Other);
        assert_eq!(restored.operation, OcrWorkerOperation::SupportingDocument);
    }
}
