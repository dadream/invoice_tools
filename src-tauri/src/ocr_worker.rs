use crate::error::{AppError, AppResult};
use invoice_parse::model::{ParsedInvoice, TicketType};
use invoice_parse::ocr_worker_protocol::{
    OcrWorkerOperation, OcrWorkerRequest, OcrWorkerResponse, OCR_WORKER_PROTOCOL_VERSION,
};
use invoice_parse::pdf::SupportingDocumentFacts;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const OCR_WORKER_FILE: &str = "invoice-ocr-worker.exe";
const OCR_WORKER_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WORKER_OUTPUT_BYTES: usize = 1024 * 1024;
static OCR_PROCESS_SLOT: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
struct PortableManifest {
    files: Vec<PortableManifestFile>,
}

#[derive(Deserialize)]
struct PortableManifestFile {
    path: String,
    sha256: String,
}

struct ProcessOutcome {
    output: Output,
    timed_out: bool,
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path).map_err(|_| AppError::parse("离线 OCR worker 无法读取"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| AppError::parse("离线 OCR worker 无法校验"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:X}", hasher.finalize()))
}

fn resolve_worker_path() -> AppResult<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("INVOICE_ASSISTANT_OCR_WORKER") {
        return verify_worker_file(PathBuf::from(path), false);
    }

    let current_exe =
        std::env::current_exe().map_err(|_| AppError::parse("无法定位离线 OCR worker"))?;
    let parent = current_exe
        .parent()
        .ok_or_else(|| AppError::parse("无法定位离线 OCR worker"))?;
    let sibling = parent.join(OCR_WORKER_FILE);
    if sibling.is_file() {
        return verify_worker_file(sibling, !cfg!(debug_assertions));
    }
    #[cfg(debug_assertions)]
    if matches!(
        parent.file_name().and_then(|name| name.to_str()),
        Some("deps" | "examples")
    ) {
        if let Some(target_dir) = parent.parent() {
            let debug_sibling = target_dir.join(OCR_WORKER_FILE);
            if debug_sibling.is_file() {
                return verify_worker_file(debug_sibling, false);
            }
        }
    }
    Err(AppError::parse(
        "离线 OCR worker 缺失；请重新解压完整便携包",
    ))
}

fn verify_worker_file(path: PathBuf, require_manifest: bool) -> AppResult<PathBuf> {
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| AppError::parse("离线 OCR worker 缺失；请重新解压完整便携包"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::parse("离线 OCR worker 文件不安全"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::parse("离线 OCR worker 路径无效"))?;
    let manifest_path = parent.join("manifest.json");
    if !manifest_path.is_file() {
        if require_manifest {
            return Err(AppError::parse("便携包 manifest 缺失，无法校验 OCR worker"));
        }
        return Ok(path);
    }
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|_| AppError::parse("便携包 manifest 无法读取"))?;
    if !manifest_metadata.is_file()
        || manifest_metadata.file_type().is_symlink()
        || manifest_metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(AppError::parse("便携包 manifest 不安全"));
    }
    let manifest: PortableManifest = serde_json::from_reader(
        File::open(&manifest_path).map_err(|_| AppError::parse("便携包 manifest 无法读取"))?,
    )
    .map_err(|_| AppError::parse("便携包 manifest 格式无效"))?;
    let expected = manifest
        .files
        .iter()
        .find(|entry| entry.path.eq_ignore_ascii_case(OCR_WORKER_FILE))
        .ok_or_else(|| AppError::parse("便携包 manifest 未声明 OCR worker"))?;
    if sha256_file(&path)? != expected.sha256 {
        return Err(AppError::parse(
            "离线 OCR worker 校验失败；请重新下载便携包",
        ));
    }
    Ok(path)
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> AppResult<ProcessOutcome> {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .map_err(|_| AppError::parse("离线 OCR worker 状态检查失败"))?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(|_| AppError::parse("离线 OCR worker 输出读取失败"))?;
            return Ok(ProcessOutcome {
                output,
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .map_err(|_| AppError::parse("离线 OCR worker 超时清理失败"))?;
            return Ok(ProcessOutcome {
                output,
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_worker(request: &OcrWorkerRequest) -> AppResult<OcrWorkerResponse> {
    let _slot = OCR_PROCESS_SLOT
        .try_lock()
        .map_err(|_| AppError::parse("离线 OCR 正在处理另一个文件，请稍后重试"))?;
    let worker_path = resolve_worker_path()?;
    let worker_directory = worker_path
        .parent()
        .ok_or_else(|| AppError::parse("离线 OCR worker 路径无效"))?;
    let mut command = Command::new(&worker_path);
    command
        .current_dir(worker_directory)
        .env_remove("PATH")
        .env_remove("ORT_DYLIB_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|_| AppError::parse("离线 OCR worker 无法启动"))?;
    let write_result = if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(&mut stdin, request)
            .and_then(|_| stdin.flush().map_err(serde_json::Error::io))
    } else {
        Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "worker stdin unavailable",
        )))
    };
    if write_result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::parse("离线 OCR worker 请求写入失败"));
    }
    let outcome = wait_with_timeout(child, OCR_WORKER_TIMEOUT)?;
    if outcome.timed_out {
        tracing::warn!(
            timeout_seconds = OCR_WORKER_TIMEOUT.as_secs(),
            "离线 OCR worker 超时并已终止"
        );
        return Err(AppError::parse(
            "离线 OCR 超过 45 秒限制；该文件已停止，请检查清晰度或拆分页数",
        ));
    }
    if !outcome.output.status.success() {
        return Err(AppError::parse("离线 OCR worker 异常退出"));
    }
    if outcome.output.stdout.len() > MAX_WORKER_OUTPUT_BYTES {
        return Err(AppError::parse("离线 OCR worker 输出超过限制"));
    }
    serde_json::from_slice(&outcome.output.stdout)
        .map_err(|_| AppError::parse("离线 OCR worker 输出格式无效"))
}

pub fn parse_with_worker(
    input_path: &Path,
    asset_dir: &Path,
    ticket_type: TicketType,
) -> AppResult<ParsedInvoice> {
    let response = run_worker(&OcrWorkerRequest {
        protocol_version: OCR_WORKER_PROTOCOL_VERSION,
        input_path: input_path.to_path_buf(),
        asset_dir: asset_dir.to_path_buf(),
        ticket_type,
        operation: OcrWorkerOperation::Invoice,
    })?;
    match response {
        OcrWorkerResponse::Success { invoice } => Ok(invoice),
        OcrWorkerResponse::SupportingDocumentSuccess { .. } => {
            Err(AppError::parse("离线 OCR worker 返回了错误的响应类型"))
        }
        OcrWorkerResponse::Failure { message, .. } => {
            Err(AppError::parse(format!("解析失败: {message}")))
        }
    }
}

pub fn supporting_facts_with_worker(
    input_path: &Path,
    asset_dir: &Path,
) -> AppResult<Option<SupportingDocumentFacts>> {
    let response = run_worker(&OcrWorkerRequest {
        protocol_version: OCR_WORKER_PROTOCOL_VERSION,
        input_path: input_path.to_path_buf(),
        asset_dir: asset_dir.to_path_buf(),
        ticket_type: TicketType::Other,
        operation: OcrWorkerOperation::SupportingDocument,
    })?;
    match response {
        OcrWorkerResponse::SupportingDocumentSuccess { facts } => Ok(facts),
        OcrWorkerResponse::Success { .. } => {
            Err(AppError::parse("离线 OCR worker 返回了错误的响应类型"))
        }
        OcrWorkerResponse::Failure { message, .. } => {
            Err(AppError::parse(format!("解析失败: {message}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn child_process_sleeper() {
        if std::env::var_os("INVOICE_OCR_TIMEOUT_CHILD").is_some() {
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    #[test]
    fn timeout_kills_child_process() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("child_process_sleeper")
            .arg("--nocapture")
            .env("INVOICE_OCR_TIMEOUT_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().unwrap();
        let outcome = wait_with_timeout(child, Duration::from_millis(50)).unwrap();
        assert!(outcome.timed_out);
        assert!(!outcome.output.status.success());
    }

    #[test]
    fn occupied_slot_rejects_another_worker_immediately() {
        let _guard = OCR_PROCESS_SLOT.lock().unwrap();
        let error = parse_with_worker(
            Path::new("unused.png"),
            Path::new("unused-assets"),
            TicketType::Other,
        )
        .unwrap_err();
        assert!(error.message().contains("正在处理另一个文件"));
    }

    #[test]
    fn portable_manifest_detects_worker_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let worker = temp.path().join(OCR_WORKER_FILE);
        fs::write(&worker, b"worker-v1").unwrap();
        let hash = sha256_file(&worker).unwrap();
        let manifest = serde_json::json!({
            "files": [{"path": OCR_WORKER_FILE, "sha256": hash}]
        });
        fs::write(
            temp.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(verify_worker_file(worker.clone(), true).unwrap(), worker);

        fs::write(&worker, b"worker-tampered").unwrap();
        let error = verify_worker_file(worker, true).unwrap_err();
        assert!(error.message().contains("校验失败"));
    }
}
