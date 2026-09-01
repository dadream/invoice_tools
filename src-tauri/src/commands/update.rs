use std::io::Read;
use std::time::Duration;

use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::redirect::Policy;
use reqwest::Url;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const PRODUCT_ID: &str = "com.dadream.invoiceassistant";
const UPDATE_CHANNEL: &str = "internal-alpha";
const UPDATE_MANIFEST_URL: Option<&str> = option_env!("INVOICE_UPDATE_MANIFEST_URL");
const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SUMMARY_CHARS: usize = 1000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    NotConfigured,
    UpToDate,
    UpdateAvailable,
    CurrentVersionNewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub configured: bool,
    pub status: UpdateStatus,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_summary: Option<String>,
    pub sha256: Option<String>,
    pub download_page_url: Option<String>,
    pub checked_at_utc: Option<String>,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateManifest {
    schema_version: u32,
    product: String,
    channel: String,
    version: String,
    summary: String,
    sha256: String,
    download_page_url: String,
    published_at_utc: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchFailure {
    Timeout,
    Offline,
    HttpStatus,
    InvalidContentType,
    TooLarge,
    Read,
}

trait ManifestFetcher {
    fn fetch(&self, url: &Url) -> Result<Vec<u8>, FetchFailure>;
}

struct ReqwestManifestFetcher;

impl ManifestFetcher for ReqwestManifestFetcher {
    fn fetch(&self, url: &Url) -> Result<Vec<u8>, FetchFailure> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .map_err(|_| FetchFailure::Offline)?;
        let response = client
            .get(url.clone())
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    FetchFailure::Timeout
                } else {
                    FetchFailure::Offline
                }
            })?;

        if !response.status().is_success() {
            return Err(FetchFailure::HttpStatus);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(FetchFailure::InvalidContentType);
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|bytes| bytes > MAX_MANIFEST_BYTES)
        {
            return Err(FetchFailure::TooLarge);
        }

        let mut body = Vec::with_capacity(4096);
        response
            .take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| FetchFailure::Read)?;
        if body.len() > MAX_MANIFEST_BYTES {
            return Err(FetchFailure::TooLarge);
        }
        Ok(body)
    }
}

/// 只在用户从设置页明确点击后调用。未配置正式清单地址时不会产生网络请求。
#[tauri::command]
pub async fn check_for_updates() -> AppResult<UpdateCheckResult> {
    let Some(manifest_url) = UPDATE_MANIFEST_URL else {
        return Ok(not_configured_result(env!("CARGO_PKG_VERSION")));
    };
    let manifest_url = manifest_url.to_string();
    tokio::task::spawn_blocking(move || {
        check_for_updates_with(
            Some(manifest_url.as_str()),
            env!("CARGO_PKG_VERSION"),
            &ReqwestManifestFetcher,
        )
    })
    .await
    .map_err(|_| AppError::internal("版本检查任务异常结束，请重试"))?
}

fn check_for_updates_with(
    manifest_url: Option<&str>,
    current_version: &str,
    fetcher: &impl ManifestFetcher,
) -> AppResult<UpdateCheckResult> {
    let Some(manifest_url) = manifest_url else {
        return Ok(not_configured_result(current_version));
    };
    let url = validate_https_url(manifest_url, "版本清单地址")?;
    let host = url.host_str().unwrap_or("invalid").to_string();
    tracing::info!(host, "用户主动检查版本");
    let body = fetcher.fetch(&url).map_err(map_fetch_failure)?;
    evaluate_manifest(&body, current_version)
}

fn evaluate_manifest(body: &[u8], current_version: &str) -> AppResult<UpdateCheckResult> {
    if body.len() > MAX_MANIFEST_BYTES {
        return Err(AppError::validation("版本清单超过 64 KiB 安全上限"));
    }
    let manifest: UpdateManifest = serde_json::from_slice(body)
        .map_err(|_| AppError::validation("版本清单不是受支持的 JSON 格式"))?;
    validate_manifest(&manifest)?;

    let current = Version::parse(current_version)
        .map_err(|_| AppError::internal("当前应用版本不是有效的语义版本"))?;
    let latest = Version::parse(&manifest.version)
        .map_err(|_| AppError::validation("版本清单中的版本号无效"))?;
    let status = if latest > current {
        UpdateStatus::UpdateAvailable
    } else if latest == current {
        UpdateStatus::UpToDate
    } else {
        UpdateStatus::CurrentVersionNewer
    };
    let message = match status {
        UpdateStatus::UpdateAvailable => "发现新版本；请核对发布说明和 SHA-256 后从下载页手动获取",
        UpdateStatus::UpToDate => "当前已是此发布渠道的最新版本",
        UpdateStatus::CurrentVersionNewer => "当前版本高于发布渠道版本；不会自动降级",
        UpdateStatus::NotConfigured => unreachable!(),
    };

    Ok(UpdateCheckResult {
        configured: true,
        status,
        current_version: current_version.to_string(),
        latest_version: Some(manifest.version),
        release_summary: Some(manifest.summary),
        sha256: Some(manifest.sha256.to_ascii_uppercase()),
        download_page_url: Some(manifest.download_page_url),
        checked_at_utc: Some(Utc::now().to_rfc3339()),
        message: message.to_string(),
    })
}

fn validate_manifest(manifest: &UpdateManifest) -> AppResult<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(AppError::validation("版本清单 schemaVersion 不受支持"));
    }
    if manifest.product != PRODUCT_ID {
        return Err(AppError::validation("版本清单不属于本产品"));
    }
    if manifest.channel != UPDATE_CHANNEL {
        return Err(AppError::validation("版本清单发布渠道与当前应用不一致"));
    }
    if manifest.summary.trim().is_empty()
        || manifest.summary.chars().count() > MAX_SUMMARY_CHARS
        || manifest.summary.chars().any(char::is_control)
    {
        return Err(AppError::validation("版本说明为空、过长或包含控制字符"));
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::validation("版本清单 SHA-256 无效"));
    }
    validate_https_url(&manifest.download_page_url, "下载页地址")?;
    chrono::DateTime::parse_from_rfc3339(&manifest.published_at_utc)
        .map_err(|_| AppError::validation("版本清单发布时间无效"))?;
    Ok(())
}

fn validate_https_url(value: &str, label: &str) -> AppResult<Url> {
    let url = Url::parse(value).map_err(|_| AppError::validation(format!("{label}无效")))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::validation(format!(
            "{label}必须是无凭据、无片段的 HTTPS 地址"
        )));
    }
    Ok(url)
}

fn map_fetch_failure(failure: FetchFailure) -> AppError {
    match failure {
        FetchFailure::Timeout => AppError::network("版本检查超时；请检查网络或稍后重试"),
        FetchFailure::Offline => AppError::network("无法连接版本服务；未下载或修改任何程序文件"),
        FetchFailure::HttpStatus => AppError::network("版本服务返回错误状态；请稍后重试"),
        FetchFailure::InvalidContentType => AppError::validation("版本服务未返回 application/json"),
        FetchFailure::TooLarge => AppError::validation("版本清单超过 64 KiB 安全上限"),
        FetchFailure::Read => AppError::network("读取版本清单失败；请稍后重试"),
    }
}

fn not_configured_result(current_version: &str) -> UpdateCheckResult {
    UpdateCheckResult {
        configured: false,
        status: UpdateStatus::NotConfigured,
        current_version: current_version.to_string(),
        latest_version: None,
        release_summary: None,
        sha256: None,
        download_page_url: None,
        checked_at_utc: None,
        message: "当前内部 Alpha 未配置正式版本清单；本次未发起网络请求".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::error::ErrorKind;

    struct FakeFetcher {
        body: Result<Vec<u8>, FetchFailure>,
        calls: Cell<usize>,
    }

    impl ManifestFetcher for FakeFetcher {
        fn fetch(&self, _url: &Url) -> Result<Vec<u8>, FetchFailure> {
            self.calls.set(self.calls.get() + 1);
            self.body.clone()
        }
    }

    fn manifest(version: &str) -> Vec<u8> {
        serde_json::json!({
            "schemaVersion": 1,
            "product": PRODUCT_ID,
            "channel": UPDATE_CHANNEL,
            "version": version,
            "summary": "安全与稳定性更新",
            "sha256": "a".repeat(64),
            "downloadPageUrl": "https://downloads.example.invalid/invoice-assistant",
            "publishedAtUtc": "2026-08-20T00:00:00Z"
        })
        .to_string()
        .into_bytes()
    }

    fn fake(body: Result<Vec<u8>, FetchFailure>) -> FakeFetcher {
        FakeFetcher {
            body,
            calls: Cell::new(0),
        }
    }

    #[test]
    fn unconfigured_check_is_offline_and_does_not_call_fetcher() {
        let fetcher = fake(Err(FetchFailure::Offline));
        let result = check_for_updates_with(None, "0.1.0", &fetcher).unwrap();
        assert_eq!(result.status, UpdateStatus::NotConfigured);
        assert!(!result.configured);
        assert_eq!(fetcher.calls.get(), 0);
        assert!(result.message.contains("未发起网络请求"));
    }

    #[test]
    fn timeout_and_offline_are_recoverable_network_errors() {
        for failure in [FetchFailure::Timeout, FetchFailure::Offline] {
            let error = check_for_updates_with(
                Some("https://updates.example.invalid/version.json"),
                "0.1.0",
                &fake(Err(failure)),
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Network);
            assert!(!error.message().contains("example.invalid"));
        }
    }

    #[test]
    fn newer_equal_and_older_versions_have_explicit_states() {
        for (version, expected) in [
            ("0.2.0", UpdateStatus::UpdateAvailable),
            ("0.1.0", UpdateStatus::UpToDate),
            ("0.0.9", UpdateStatus::CurrentVersionNewer),
        ] {
            let result = check_for_updates_with(
                Some("https://updates.example.invalid/version.json"),
                "0.1.0",
                &fake(Ok(manifest(version))),
            )
            .unwrap();
            assert_eq!(result.status, expected);
            assert_eq!(result.sha256.as_deref(), Some("A".repeat(64).as_str()));
        }
    }

    #[test]
    fn invalid_download_url_and_hash_are_rejected() {
        let mut value: serde_json::Value = serde_json::from_slice(&manifest("0.2.0")).unwrap();
        value["downloadPageUrl"] = "http://downloads.example.invalid/file.zip".into();
        assert_eq!(
            evaluate_manifest(value.to_string().as_bytes(), "0.1.0")
                .unwrap_err()
                .kind(),
            ErrorKind::Validation
        );
        value["downloadPageUrl"] = "https://downloads.example.invalid/".into();
        value["sha256"] = "not-a-hash".into();
        assert_eq!(
            evaluate_manifest(value.to_string().as_bytes(), "0.1.0")
                .unwrap_err()
                .kind(),
            ErrorKind::Validation
        );
    }

    #[test]
    fn oversized_or_wrong_channel_manifest_is_rejected() {
        assert_eq!(
            evaluate_manifest(&vec![b' '; MAX_MANIFEST_BYTES + 1], "0.1.0")
                .unwrap_err()
                .kind(),
            ErrorKind::Validation
        );
        let mut value: serde_json::Value = serde_json::from_slice(&manifest("0.2.0")).unwrap();
        value["channel"] = "stable".into();
        assert_eq!(
            evaluate_manifest(value.to_string().as_bytes(), "0.1.0")
                .unwrap_err()
                .kind(),
            ErrorKind::Validation
        );
    }

    #[test]
    fn manifest_url_must_be_https_without_credentials_or_fragment() {
        for url in [
            "http://updates.example.invalid/version.json",
            "https://user:pass@updates.example.invalid/version.json",
            "https://updates.example.invalid/version.json#fragment",
        ] {
            let error = check_for_updates_with(Some(url), "0.1.0", &fake(Ok(manifest("0.2.0"))))
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
        }
    }
}
