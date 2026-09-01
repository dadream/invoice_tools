use invoice_parse::model::ParsedInvoice;
use invoice_parse::ocr_worker_protocol::{
    OcrWorkerOperation, OcrWorkerRequest, OcrWorkerResponse, OCR_WORKER_PROTOCOL_VERSION,
};
use std::io::{Read, Write};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;

fn failure(code: &str, message: &str) -> OcrWorkerResponse {
    OcrWorkerResponse::Failure {
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn parse(request: OcrWorkerRequest) -> OcrWorkerResponse {
    if request.protocol_version != OCR_WORKER_PROTOCOL_VERSION {
        return failure("protocol_version", "OCR worker 协议版本不匹配");
    }
    let extension = request
        .input_path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if request.operation == OcrWorkerOperation::SupportingDocument {
        let facts = match extension.as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "bmp" => {
                invoice_parse::ocr::recognize_offline(&request.input_path, &request.asset_dir)
                    .map(|boxes| {
                        boxes
                            .into_iter()
                            .map(|text_box| text_box.text)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .map_err(|error| error.to_string())
                    .map(|text| invoice_parse::pdf::extract_supporting_document_facts(&text))
            }
            "pdf" => invoice_parse::pdf_ocr::extract_scanned_supporting_document_facts(
                &request.input_path,
                &request.asset_dir,
            )
            .map_err(|error| error.to_string()),
            _ => Err("OCR worker 不支持该配套材料类型".to_string()),
        };
        return match facts {
            Ok(facts) => OcrWorkerResponse::SupportingDocumentSuccess { facts },
            Err(message) => failure("supporting_parse", &message),
        };
    }

    let result: Result<ParsedInvoice, String> = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => {
            invoice_parse::ocr::parse_invoice_image(&request.input_path, &request.asset_dir)
                .map_err(|error| error.to_string())
        }
        "pdf" => invoice_parse::pdf_ocr::parse_scanned_invoice_pdf(
            &request.input_path,
            &request.asset_dir,
            request.ticket_type,
        )
        .map_err(|error| error.to_string()),
        _ => Err("OCR worker 不支持该文件类型".to_string()),
    };
    match result {
        Ok(mut invoice) => {
            invoice.ticket_type = invoice_parse::expense_classifier::resolve_ticket_type_hint(
                invoice.ticket_type,
                request.ticket_type,
            );
            OcrWorkerResponse::Success { invoice }
        }
        Err(message) => failure("parse", &message),
    }
}

fn run() -> OcrWorkerResponse {
    if invoice_parse::windows_security::harden_process_dll_search().is_err() {
        return failure("dll_search", "OCR worker 安全初始化失败");
    }
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failure("stdin", "OCR worker 无法读取请求");
    }
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return failure("request_size", "OCR worker 请求超过 64 KiB 限制");
    }
    let request: OcrWorkerRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return failure("request_json", "OCR worker 请求格式无效"),
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(request))) {
        Ok(response) => response,
        Err(_) => failure("panic", "OCR worker 发生异常"),
    }
}

fn main() {
    let response = run();
    let payload = serde_json::to_vec(&response).unwrap_or_else(|_| {
        r#"{"status":"failure","code":"response_json","message":"OCR worker 输出失败"}"#
            .as_bytes()
            .to_vec()
    });
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&payload);
    let _ = stdout.flush();
}
