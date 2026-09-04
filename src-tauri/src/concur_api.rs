//! SAP Concur 草稿 API 适配器。
//!
//! 访问令牌只保存在应用进程内存中。这里仅允许 SAP Concur 官方 HTTPS API
//! 主机，避免设置页提供任意网络请求能力。所有写入都停留在未提交草稿。

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

use chrono::Utc;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};

const MAX_RESPONSE_CHARS: usize = 8_000;
const REQUEST_TIMEOUT_SECONDS: u64 = 45;
const BROWSER_OAUTH_TIMEOUT_SECONDS: u64 = 300;
const BROWSER_OAUTH_CALLBACK_PORT: u16 = 53_682;
const BROWSER_OAUTH_CALLBACK_PATH: &str = "/concur/oauth/callback";
const BROWSER_OAUTH_SCOPES: &str = "EXPRPT IMAGE";

fn browser_oauth_redirect_uri() -> String {
    format!("http://127.0.0.1:{BROWSER_OAUTH_CALLBACK_PORT}{BROWSER_OAUTH_CALLBACK_PATH}")
}

#[derive(Clone)]
pub struct ConcurApiSession {
    pub base_url: String,
    pub access_token: Zeroizing<String>,
    pub authorization_method: String,
    pub granted_scopes: Vec<String>,
    pub connected_account: Option<ConcurConnectedAccount>,
    pub capability_checks: Vec<ConcurCapabilityTestStep>,
    pub read_verified: bool,
    pub draft_workflow_verified: bool,
    pub verified_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurConnectionStatus {
    pub configured: bool,
    pub base_url: Option<String>,
    pub read_verified: bool,
    pub draft_workflow_verified: bool,
    pub verified_at: Option<String>,
    pub authorization_method: Option<String>,
    pub granted_scopes: Vec<String>,
    pub connected_account: Option<ConcurConnectedAccount>,
    pub capability_checks: Vec<ConcurCapabilityTestStep>,
    pub reason: String,
}

impl ConcurConnectionStatus {
    pub fn disconnected() -> Self {
        Self {
            configured: false,
            base_url: None,
            read_verified: false,
            draft_workflow_verified: false,
            verified_at: None,
            authorization_method: None,
            granted_scopes: Vec::new(),
            connected_account: None,
            capability_checks: Vec::new(),
            reason: "尚未连接 Concur；访问令牌只在本次程序运行期间使用".to_string(),
        }
    }
}

impl From<&ConcurApiSession> for ConcurConnectionStatus {
    fn from(session: &ConcurApiSession) -> Self {
        Self {
            configured: true,
            base_url: Some(session.base_url.clone()),
            read_verified: session.read_verified,
            draft_workflow_verified: session.draft_workflow_verified,
            verified_at: Some(session.verified_at.clone()),
            authorization_method: Some(session.authorization_method.clone()),
            granted_scopes: session.granted_scopes.clone(),
            connected_account: session.connected_account.clone(),
            capability_checks: session.capability_checks.clone(),
            reason: if session.draft_workflow_verified {
                "草稿、费用和附件能力已在当前会话完成验证，可以创建真实报销草稿".to_string()
            } else {
                "只读连接已通过；完成草稿闭环测试后才会启用真实交付".to_string()
            },
        }
    }
}

#[derive(Deserialize)]
pub struct ConcurConnectionInput {
    pub base_url: String,
    pub access_token: String,
}

#[derive(Deserialize)]
pub struct ConcurBrowserOauthInput {
    pub base_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurBrowserOauthConfig {
    pub redirect_uri: String,
    pub scopes: String,
    pub timeout_seconds: u64,
}

pub fn browser_oauth_config() -> ConcurBrowserOauthConfig {
    ConcurBrowserOauthConfig {
        redirect_uri: browser_oauth_redirect_uri(),
        scopes: BROWSER_OAUTH_SCOPES.to_string(),
        timeout_seconds: BROWSER_OAUTH_TIMEOUT_SECONDS,
    }
}

pub struct ConcurBrowserOauthAttempt {
    listener: TcpListener,
    authorize_url: String,
    requested_base_url: String,
    client_id: String,
    client_secret: Zeroizing<String>,
    expected_state: String,
}

impl ConcurBrowserOauthAttempt {
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }
}

pub struct ConcurBrowserOauthOutcome {
    pub session: Option<ConcurApiSession>,
    pub result: ConcurCapabilityTestResult,
}

struct BrowserOauthCallback {
    code: Zeroizing<String>,
    geolocation: Option<String>,
}

#[derive(Deserialize)]
struct OauthTokenResponse {
    access_token: Zeroizing<String>,
    #[serde(default)]
    refresh_token: Option<Zeroizing<String>>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    geolocation: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConcurDraftWorkflowTestInput {
    pub expense_type_code: String,
    pub payment_type_id: String,
    pub location_id: Option<String>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurCapabilityTestStep {
    pub key: String,
    pub label: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConcurConnectedAccount {
    pub login_id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurCapabilityTestResult {
    pub success: bool,
    pub checked_at: String,
    pub draft_report_id: Option<String>,
    pub draft_report_name: Option<String>,
    pub connected_account: Option<ConcurConnectedAccount>,
    pub steps: Vec<ConcurCapabilityTestStep>,
    pub next_action: String,
}

#[derive(Debug, Clone)]
pub struct ConcurApiCallError {
    pub message: String,
    /// 写请求已发出但无法确定服务器是否完成时，必须先人工核对，不能盲目重试。
    pub result_unknown: bool,
}

impl std::fmt::Display for ConcurApiCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub struct ConcurApiClient {
    client: Client,
    base_url: String,
    access_token: Zeroizing<String>,
}

impl ConcurApiClient {
    pub fn new(base_url: &str, access_token: Zeroizing<String>) -> AppResult<Self> {
        let base_url = validate_concur_base_url(base_url)?;
        if access_token.trim().is_empty() || access_token.chars().count() > 16_384 {
            return Err(AppError::validation("Concur 访问令牌为空或长度异常"));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
            .user_agent("invoice-assistant/0.1")
            .build()
            .map_err(|error| AppError::network(format!("初始化 Concur 网络连接失败：{error}")))?;
        Ok(Self {
            client,
            base_url,
            access_token,
        })
    }

    pub fn from_session(session: &ConcurApiSession) -> AppResult<Self> {
        Self::new(&session.base_url, session.access_token.clone())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        request.bearer_auth(self.access_token.as_str())
    }

    pub fn probe_reports(&self) -> Result<Option<ConcurConnectedAccount>, ConcurApiCallError> {
        let response = self
            .auth(
                self.client
                    .get(self.url("/api/v3.0/expense/reports?limit=1")),
            )
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|error| transport_error("连接 Concur 报销单接口", error, false))?;
        response_json(response, "读取当前账号报销单", false)
            .map(|value| account_from_reports(&value))
    }

    pub fn create_report(
        &self,
        name: &str,
        report_date: &str,
        target_fields: Option<&Value>,
    ) -> Result<String, ConcurApiCallError> {
        let payload = report_v3_payload(name, report_date, target_fields)?;
        let response = self
            .auth(self.client.post(self.url("/api/v3.0/expense/reports")))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(payload.to_string())
            .send()
            .map_err(|error| transport_error("创建 Concur 报销单草稿", error, true))?;
        let value = response_json(response, "创建 Concur 报销单草稿", true)?;
        response_id(&value, "报销单")
    }

    pub fn get_report(&self, report_id: &str) -> Result<Value, ConcurApiCallError> {
        let response = self
            .auth(
                self.client
                    .get(self.url(&format!("/api/v3.0/expense/reports/{report_id}"))),
            )
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|error| transport_error("回读 Concur 报销单草稿", error, false))?;
        response_json(response, "回读 Concur 报销单草稿", false)
    }

    pub fn create_expense(
        &self,
        report_id: &str,
        target_fields: &Value,
    ) -> Result<String, ConcurApiCallError> {
        let payload = expense_v3_payload(report_id, target_fields)?;
        let response = self
            .auth(self.client.post(self.url("/api/v3.0/expense/entries")))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(payload.to_string())
            .send()
            .map_err(|error| transport_error("创建 Concur 费用", error, true))?;
        let value = response_json(response, "创建 Concur 费用", true)?;
        response_id(&value, "费用")
    }

    pub fn get_expense(&self, expense_id: &str) -> Result<Value, ConcurApiCallError> {
        let response = self
            .auth(
                self.client
                    .get(self.url(&format!("/api/v3.0/expense/entries/{expense_id}"))),
            )
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|error| transport_error("回读 Concur 费用", error, false))?;
        response_json(response, "回读 Concur 费用", false)
    }

    pub fn upload_expense_pdf(
        &self,
        expense_id: &str,
        bytes: Vec<u8>,
    ) -> Result<String, ConcurApiCallError> {
        if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
            return Err(ConcurApiCallError {
                message: "费用材料合订 PDF 必须小于等于 10 MB".to_string(),
                result_unknown: false,
            });
        }
        let response = self
            .auth(
                self.client
                    .post(self.url(&format!("/api/image/v1.0/expenseentry/{expense_id}"))),
            )
            .header(CONTENT_TYPE, "application/pdf")
            .header(ACCEPT, "application/xml")
            .body(bytes)
            .send()
            .map_err(|error| transport_error("上传 Concur 费用材料", error, true))?;
        let text = response_text(response, "上传 Concur 费用材料", true)?;
        Ok(xml_tag(&text, "Id").unwrap_or_else(|| format!("expenseentry:{expense_id}")))
    }

    pub fn verify_expense_image(&self, expense_id: &str) -> Result<(), ConcurApiCallError> {
        let response = self
            .auth(
                self.client
                    .get(self.url(&format!("/api/image/v1.0/expenseentry/{expense_id}"))),
            )
            .header(ACCEPT, "application/xml")
            .send()
            .map_err(|error| transport_error("回读 Concur 费用附件", error, false))?;
        let text = response_text(response, "回读 Concur 费用附件", false)?;
        if xml_tag(&text, "Url").is_none() {
            return Err(ConcurApiCallError {
                message: "Concur 未返回可核对的费用附件地址".to_string(),
                result_unknown: false,
            });
        }
        Ok(())
    }
}

pub fn validate_concur_base_url(value: &str) -> AppResult<String> {
    let parsed = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::validation("Concur 数据中心地址无效"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::validation("Concur 数据中心地址缺少主机名"))?
        .to_ascii_lowercase();
    let allowed_host = host == "cn.api.concurcdc.cn" || host.ends_with(".api.concursolutions.com");
    if parsed.scheme() != "https"
        || !allowed_host
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(AppError::validation(
            "仅支持 SAP Concur 官方 HTTPS 数据中心地址，例如 https://cn.api.concurcdc.cn",
        ));
    }
    Ok(format!("https://{host}"))
}

fn browser_authorization_origin(base_url: &str) -> AppResult<String> {
    let base_url = validate_concur_base_url(base_url)?;
    let parsed = reqwest::Url::parse(&base_url)
        .map_err(|_| AppError::validation("Concur 数据中心地址无效"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::validation("Concur 数据中心地址缺少主机名"))?;
    if host == "cn.api.concurcdc.cn" {
        return Ok("https://www-cn.api.concurcdc.cn".to_string());
    }
    let geolocation = host
        .strip_suffix(".api.concursolutions.com")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::validation("无法确定 Concur 浏览器授权地址"))?;
    Ok(format!("https://www-{geolocation}.api.concursolutions.com"))
}

fn normalize_token_geolocation(value: &str) -> AppResult<String> {
    let parsed = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::validation("Concur 授权回调的数据中心地址无效"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::validation("Concur 授权回调缺少数据中心主机名"))?;
    validate_concur_base_url(&format!("https://{host}"))
}

fn browser_authorize_url(base_url: &str, client_id: &str, state: &str) -> AppResult<String> {
    let mut url = reqwest::Url::parse(&format!(
        "{}/oauth2/v0/authorize",
        browser_authorization_origin(base_url)?
    ))
    .map_err(|_| AppError::internal("生成 Concur 浏览器授权地址失败"))?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", &browser_oauth_redirect_uri())
        .append_pair("scope", BROWSER_OAUTH_SCOPES)
        .append_pair("response_type", "code")
        .append_pair("state", state);
    Ok(url.to_string())
}

pub fn prepare_browser_oauth(
    input: ConcurBrowserOauthInput,
) -> AppResult<ConcurBrowserOauthAttempt> {
    if !input.confirmed {
        return Err(AppError::validation(
            "请确认当前浏览器授权仅用于本次 Concur 能力测试",
        ));
    }
    let requested_base_url = validate_concur_base_url(&input.base_url)?;
    let client_id = required_identifier(&input.client_id, "Concur Client ID")?;
    let client_secret = Zeroizing::new(input.client_secret);
    if client_secret.trim().is_empty()
        || client_secret.chars().count() > 4_096
        || client_secret.chars().any(char::is_control)
    {
        return Err(AppError::validation("Concur Client Secret 为空或格式异常"));
    }
    let expected_state = uuid::Uuid::new_v4().to_string();
    let authorize_url = browser_authorize_url(&requested_base_url, &client_id, &expected_state)?;
    let listener = TcpListener::bind(("127.0.0.1", BROWSER_OAUTH_CALLBACK_PORT)).map_err(|_| {
        AppError::io(format!(
            "本机回调端口 {BROWSER_OAUTH_CALLBACK_PORT} 被占用；请关闭占用该端口的程序后重试"
        ))
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|_| AppError::io("无法初始化 Concur 本机授权回调"))?;
    Ok(ConcurBrowserOauthAttempt {
        listener,
        authorize_url,
        requested_base_url,
        client_id,
        client_secret,
        expected_state,
    })
}

pub fn complete_browser_oauth(
    attempt: ConcurBrowserOauthAttempt,
) -> AppResult<ConcurBrowserOauthOutcome> {
    let callback = wait_for_browser_oauth_callback(&attempt.listener, &attempt.expected_state)?;
    let callback_base_url = callback
        .geolocation
        .as_deref()
        .map(normalize_token_geolocation)
        .transpose()?
        .unwrap_or_else(|| attempt.requested_base_url.clone());
    let token_response = exchange_browser_authorization_code(
        &callback_base_url,
        &attempt.client_id,
        &attempt.client_secret,
        &callback.code,
    )?;
    let base_url = token_response
        .geolocation
        .as_deref()
        .map(normalize_token_geolocation)
        .transpose()?
        .unwrap_or(callback_base_url);
    let granted_scopes = parse_granted_scopes(token_response.scope.as_deref().unwrap_or_default());
    let missing_scopes = ["EXPRPT", "IMAGE"]
        .into_iter()
        .filter(|required| {
            !granted_scopes
                .iter()
                .any(|scope| scope.eq_ignore_ascii_case(required))
        })
        .collect::<Vec<_>>();
    let now = Utc::now().to_rfc3339();
    let mut steps = vec![
        test_step(
            "browser_authorization",
            "浏览器登录与授权回调",
            "passed",
            "系统浏览器已完成 Concur 授权回调；软件未读取浏览器 Cookie",
        ),
        test_step(
            "token_exchange",
            "交换临时授权码",
            "passed",
            "已取得本次会话访问令牌；刷新令牌未保存",
        ),
    ];
    if missing_scopes.is_empty() {
        steps.push(test_step(
            "required_scopes",
            "检查接口权限",
            "passed",
            "授权响应包含 EXPRPT 与 IMAGE",
        ));
    } else if granted_scopes.is_empty() {
        steps.push(test_step(
            "required_scopes",
            "检查接口权限",
            "not_tested",
            "授权响应未列出权限，将通过实际只读接口继续确认",
        ));
    } else {
        steps.push(test_step(
            "required_scopes",
            "检查接口权限",
            "failed",
            &format!(
                "授权缺少 {}；请由企业管理员更新应用权限",
                missing_scopes.join("、")
            ),
        ));
        return Ok(ConcurBrowserOauthOutcome {
            session: None,
            result: ConcurCapabilityTestResult {
                success: false,
                checked_at: now,
                draft_report_id: None,
                draft_report_name: None,
                connected_account: None,
                steps,
                next_action: "请联系 Concur 管理员为当前 OAuth 应用补充 EXPRPT 与 IMAGE 权限后重试"
                    .to_string(),
            },
        });
    }

    let client = ConcurApiClient::new(&base_url, token_response.access_token.clone())?;
    let connected_account = match client.probe_reports() {
        Ok(account) => account,
        Err(error) => {
            steps.push(test_step(
                "report_read",
                "读取当前账号报销单",
                "failed",
                &error.message,
            ));
            return Ok(ConcurBrowserOauthOutcome {
                session: None,
                result: ConcurCapabilityTestResult {
                    success: false,
                    checked_at: now,
                    draft_report_id: None,
                    draft_report_name: None,
                    connected_account: None,
                    steps,
                    next_action: "浏览器授权已完成，但当前用户或应用不能读取报销单；请核对账号角色、数据中心和 EXPRPT 权限".to_string(),
                },
            });
        }
    };
    steps.push(test_step(
        "report_read",
        "读取当前账号报销单",
        "passed",
        if connected_account.is_some() {
            "浏览器 OAuth 访问令牌可以读取当前账号的报销单"
        } else {
            "报销单接口可访问；当前账号没有返回可用于识别账号的历史报销单"
        },
    ));
    let session = ConcurApiSession {
        base_url,
        access_token: token_response.access_token,
        authorization_method: "browser_oauth".to_string(),
        granted_scopes,
        connected_account: connected_account.clone(),
        capability_checks: steps.clone(),
        read_verified: true,
        draft_workflow_verified: false,
        verified_at: now.clone(),
    };
    // 明确消费并丢弃刷新令牌；Zeroizing 会在离开作用域时清零其缓冲区。
    drop(token_response.refresh_token);
    Ok(ConcurBrowserOauthOutcome {
        session: Some(session),
        result: ConcurCapabilityTestResult {
            success: true,
            checked_at: now,
            draft_report_id: None,
            draft_report_name: None,
            connected_account,
            steps,
            next_action: "浏览器授权与只读能力已通过；可继续执行未提交测试草稿闭环".to_string(),
        },
    })
}

fn wait_for_browser_oauth_callback(
    listener: &TcpListener,
    expected_state: &str,
) -> AppResult<BrowserOauthCallback> {
    let deadline = Instant::now() + Duration::from_secs(BROWSER_OAUTH_TIMEOUT_SECONDS);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => match read_browser_callback(&mut stream, expected_state) {
                Ok(Some(callback)) => {
                    write_browser_callback_page(&mut stream, true);
                    return Ok(callback);
                }
                Ok(None) => {
                    write_browser_callback_page(&mut stream, false);
                }
                Err(error) => {
                    write_browser_callback_page(&mut stream, false);
                    return Err(error);
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return Err(AppError::io("接收 Concur 本机授权回调失败")),
        }
    }
    Err(AppError::network(
        "等待 Concur 浏览器授权超时；没有保存任何令牌，请重新开始授权",
    ))
}

fn read_browser_callback(
    stream: &mut TcpStream,
    expected_state: &str,
) -> AppResult<Option<BrowserOauthCallback>> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| AppError::io("无法读取 Concur 本机授权回调"))?;
    let mut buffer = [0_u8; 16_384];
    let bytes = stream
        .read(&mut buffer)
        .map_err(|_| AppError::io("Concur 本机授权回调读取失败"))?;
    let request = std::str::from_utf8(&buffer[..bytes])
        .map_err(|_| AppError::validation("Concur 本机授权回调格式无效"))?;
    let mut request_parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if request_parts.next() != Some("GET") {
        return Ok(None);
    }
    let target = request_parts.next().unwrap_or_default();
    parse_browser_callback_target(target, expected_state)
}

fn parse_browser_callback_target(
    target: &str,
    expected_state: &str,
) -> AppResult<Option<BrowserOauthCallback>> {
    let url = reqwest::Url::parse(&format!(
        "http://127.0.0.1:{BROWSER_OAUTH_CALLBACK_PORT}{target}"
    ))
    .map_err(|_| AppError::validation("Concur 本机授权回调地址无效"))?;
    if url.path() != BROWSER_OAUTH_CALLBACK_PATH {
        return Ok(None);
    }
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(error) = query.get("error") {
        let reason = query
            .get("error_description")
            .map(|value| value.as_ref())
            .unwrap_or_else(|| error.as_ref());
        return Err(AppError::validation(format!(
            "Concur 未完成授权：{}",
            reason.chars().take(300).collect::<String>()
        )));
    }
    if query.get("state").map(|value| value.as_ref()) != Some(expected_state) {
        return Err(AppError::validation(
            "Concur 授权回调校验失败；已拒绝使用该回调，请重新授权",
        ));
    }
    let code = query
        .get("code")
        .map(|value| value.as_ref())
        .filter(|value| !value.trim().is_empty() && value.chars().count() <= 4_096)
        .ok_or_else(|| AppError::validation("Concur 授权回调没有有效临时授权码"))?;
    Ok(Some(BrowserOauthCallback {
        code: Zeroizing::new(code.to_string()),
        geolocation: query.get("geolocation").map(ToString::to_string),
    }))
}

fn write_browser_callback_page(stream: &mut TcpStream, success: bool) {
    let (title, message) = if success {
        (
            "Concur 授权已返回",
            "请返回发票报销助手查看能力测试结果；此页面可以关闭。",
        )
    } else {
        (
            "未完成 Concur 授权",
            "请返回发票报销助手查看原因并重新操作。",
        )
    };
    let body = format!(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title><style>body{{margin:0;background:#f3f4f2;color:#17221d;font:16px/1.6 system-ui}}main{{max-width:620px;margin:12vh auto;padding:32px;background:#fff;border-left:6px solid #136b52}}h1{{margin:0 0 12px;font-size:26px}}p{{margin:0;color:#596870}}</style><main><h1>{title}</h1><p>{message}</p></main></html>"
    );
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        if success { "200 OK" } else { "400 Bad Request" },
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn exchange_browser_authorization_code(
    base_url: &str,
    client_id: &str,
    client_secret: &Zeroizing<String>,
    code: &Zeroizing<String>,
) -> AppResult<OauthTokenResponse> {
    let client = Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .user_agent("invoice-assistant/0.1")
        .build()
        .map_err(|_| AppError::network("初始化 Concur OAuth 连接失败"))?;
    let redirect_uri = browser_oauth_redirect_uri();
    let mut encoded_form = reqwest::Url::parse("https://localhost/")
        .map_err(|_| AppError::internal("生成 Concur OAuth 请求失败"))?;
    encoded_form
        .query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("client_secret", client_secret.as_str())
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("code", code.as_str())
        .append_pair("grant_type", "authorization_code");
    let encoded_form = Zeroizing::new(encoded_form.query().unwrap_or_default().to_string());
    let response = client
        .post(format!("{base_url}/oauth2/v0/token"))
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(encoded_form.as_str().to_string())
        .send()
        .map_err(|_| AppError::network("Concur 临时授权码交换失败；请检查网络后重新授权"))?;
    let status = response.status();
    let body = Zeroizing::new(
        response
            .text()
            .map_err(|_| AppError::network("无法读取 Concur OAuth 响应"))?,
    );
    if !status.is_success() {
        return Err(AppError::network(format!(
            "Concur OAuth 授权码交换失败（HTTP {}）：{}",
            status.as_u16(),
            readable_error_detail(body.as_str())
        )));
    }
    let token: OauthTokenResponse = serde_json::from_str(body.as_str())
        .map_err(|_| AppError::network("Concur OAuth 响应格式无法识别"))?;
    if token.access_token.trim().is_empty() || token.access_token.chars().count() > 16_384 {
        return Err(AppError::network("Concur OAuth 响应没有有效访问令牌"));
    }
    Ok(token)
}

fn parse_granted_scopes(value: &str) -> Vec<String> {
    value
        .split(|character: char| character.is_whitespace() || character == ',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn read_connection_test(
    input: ConcurConnectionInput,
) -> AppResult<(ConcurApiSession, ConcurCapabilityTestResult)> {
    let token = Zeroizing::new(input.access_token);
    let client = ConcurApiClient::new(&input.base_url, token.clone())?;
    let connected_account = client.probe_reports().map_err(api_error_to_app)?;
    let now = Utc::now().to_rfc3339();
    let steps = vec![test_step(
        "report_read",
        "读取当前账号报销单",
        "passed",
        if connected_account.is_some() {
            "访问令牌有效，可以读取当前账号的报销单"
        } else {
            "报销单接口可访问；当前账号没有返回可用于识别账号的历史报销单"
        },
    )];
    let session = ConcurApiSession {
        base_url: client.base_url().to_string(),
        access_token: token,
        authorization_method: "manual_access_token".to_string(),
        granted_scopes: Vec::new(),
        connected_account: connected_account.clone(),
        capability_checks: steps.clone(),
        read_verified: true,
        draft_workflow_verified: false,
        verified_at: now.clone(),
    };
    Ok((
        session,
        ConcurCapabilityTestResult {
            success: true,
            checked_at: now,
            draft_report_id: None,
            draft_report_name: None,
            connected_account,
            steps,
            next_action: "如需启用真实交付，请继续执行一次草稿闭环测试".to_string(),
        },
    ))
}

pub fn draft_workflow_test(
    session: &ConcurApiSession,
    input: &ConcurDraftWorkflowTestInput,
) -> AppResult<ConcurCapabilityTestResult> {
    if !input.confirmed {
        return Err(AppError::validation(
            "请先确认本次测试会在 Concur 创建一份未提交测试草稿",
        ));
    }
    let expense_type = required_identifier(&input.expense_type_code, "费用类型代码")?;
    let payment_type = required_identifier(&input.payment_type_id, "付款类型 ID")?;
    let location_id = input
        .location_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| required_identifier(value, "地点 ID"))
        .transpose()?;
    let client = ConcurApiClient::from_session(session)?;
    let now = Utc::now();
    let report_name = format!("[发票助手能力测试] {}", now.format("%Y%m%d-%H%M%S"));
    let report_date = now.format("%Y-%m-%d").to_string();
    let mut connected_account = session.connected_account.clone();
    let mut steps = vec![test_step(
        "report_read",
        "读取当前账号报销单",
        "passed",
        "已通过只读连接测试",
    )];

    let report_id = match client.create_report(&report_name, &report_date, None) {
        Ok(value) => value,
        Err(error) => {
            return Ok(failed_workflow_result(
                ConcurWorkflowProgress {
                    checked_at: now.to_rfc3339(),
                    report_id: None,
                    report_name: Some(report_name),
                    connected_account,
                    steps,
                },
                "report_create",
                "创建未提交测试草稿",
                error,
            ))
        }
    };
    steps.push(test_step(
        "report_create",
        "创建未提交测试草稿",
        "passed",
        &format!("已创建草稿，报销单 ID：{report_id}"),
    ));
    match client.get_report(&report_id) {
        Ok(report)
            if report.get("ApprovalStatusCode").and_then(Value::as_str) == Some("A_NOTF") =>
        {
            connected_account = account_from_report(&report).or(connected_account);
            steps.push(test_step(
                "report_readback",
                "回读草稿状态",
                "passed",
                "状态为未提交；软件不会执行最终提交",
            ));
        }
        Ok(_) => {
            return Ok(failed_workflow_result(
                ConcurWorkflowProgress {
                    checked_at: now.to_rfc3339(),
                    report_id: Some(report_id),
                    report_name: Some(report_name),
                    connected_account,
                    steps,
                },
                "report_readback",
                "回读草稿状态",
                ConcurApiCallError {
                    message: "报销单已创建，但未能确认其状态为未提交；请在 Concur 人工核对"
                        .to_string(),
                    result_unknown: true,
                },
            ));
        }
        Err(error) => {
            return Ok(failed_workflow_result(
                ConcurWorkflowProgress {
                    checked_at: now.to_rfc3339(),
                    report_id: Some(report_id),
                    report_name: Some(report_name),
                    connected_account,
                    steps,
                },
                "report_readback",
                "回读草稿状态",
                error,
            ))
        }
    }

    let mut target = json!({
        "expense_type_id": expense_type,
        "payment_type_id": payment_type,
        "transaction_date": report_date,
        "amount": "0.01",
        "currency": "CNY",
        "business_purpose": "Invoice Assistant capability test"
    });
    if let Some(location_id) = location_id {
        target["purchase_city_id"] = Value::String(location_id);
    }
    let expense_id = match client.create_expense(&report_id, &target) {
        Ok(value) => value,
        Err(error) => {
            return Ok(failed_workflow_result(
                ConcurWorkflowProgress {
                    checked_at: now.to_rfc3339(),
                    report_id: Some(report_id),
                    report_name: Some(report_name),
                    connected_account,
                    steps,
                },
                "expense_create",
                "创建 0.01 元测试费用",
                error,
            ))
        }
    };
    steps.push(test_step(
        "expense_create",
        "创建 0.01 元测试费用",
        "passed",
        &format!("已创建费用，费用 ID：{expense_id}"),
    ));
    if let Err(error) = client.get_expense(&expense_id) {
        return Ok(failed_workflow_result(
            ConcurWorkflowProgress {
                checked_at: now.to_rfc3339(),
                report_id: Some(report_id),
                report_name: Some(report_name),
                connected_account,
                steps,
            },
            "expense_readback",
            "回读测试费用",
            error,
        ));
    }
    steps.push(test_step(
        "expense_readback",
        "回读测试费用",
        "passed",
        "费用可回读，报销单与费用关联可用",
    ));

    let test_pdf = build_test_receipt_pdf()?;
    if let Err(error) = client.upload_expense_pdf(&expense_id, test_pdf) {
        return Ok(failed_workflow_result(
            ConcurWorkflowProgress {
                checked_at: now.to_rfc3339(),
                report_id: Some(report_id),
                report_name: Some(report_name),
                connected_account,
                steps,
            },
            "attachment_upload",
            "上传测试 PDF",
            error,
        ));
    }
    steps.push(test_step(
        "attachment_upload",
        "上传测试 PDF",
        "passed",
        "PDF 已关联到测试费用",
    ));
    if let Err(error) = client.verify_expense_image(&expense_id) {
        return Ok(failed_workflow_result(
            ConcurWorkflowProgress {
                checked_at: now.to_rfc3339(),
                report_id: Some(report_id),
                report_name: Some(report_name),
                connected_account,
                steps,
            },
            "attachment_readback",
            "回读费用附件",
            error,
        ));
    }
    steps.push(test_step(
        "attachment_readback",
        "回读费用附件",
        "passed",
        "附件可回读，完整草稿能力已验证",
    ));

    Ok(ConcurCapabilityTestResult {
        success: true,
        checked_at: now.to_rfc3339(),
        draft_report_id: Some(report_id),
        draft_report_name: Some(report_name),
        connected_account,
        steps,
        next_action: "请在 Concur 删除这份测试草稿；当前程序会话已允许真实草稿交付".to_string(),
    })
}

struct ConcurWorkflowProgress {
    checked_at: String,
    report_id: Option<String>,
    report_name: Option<String>,
    connected_account: Option<ConcurConnectedAccount>,
    steps: Vec<ConcurCapabilityTestStep>,
}

fn failed_workflow_result(
    mut progress: ConcurWorkflowProgress,
    key: &str,
    label: &str,
    error: ConcurApiCallError,
) -> ConcurCapabilityTestResult {
    let message = if error.result_unknown {
        format!(
            "{}；外部结果不确定，请先在 Concur 核对，不要立即重试",
            error.message
        )
    } else {
        error.message
    };
    progress
        .steps
        .push(test_step(key, label, "failed", &message));
    ConcurCapabilityTestResult {
        success: false,
        checked_at: progress.checked_at,
        draft_report_id: progress.report_id,
        draft_report_name: progress.report_name,
        connected_account: progress.connected_account,
        steps: progress.steps,
        next_action: "根据失败步骤检查应用权限、目标选项 ID 或企业配置；如已产生草稿，请先在 Concur 核对并删除".to_string(),
    }
}

fn test_step(key: &str, label: &str, status: &str, message: &str) -> ConcurCapabilityTestStep {
    ConcurCapabilityTestStep {
        key: key.to_string(),
        label: label.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    }
}

fn required_identifier(value: &str, label: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
        return Err(AppError::validation(format!("{label}为空或格式异常")));
    }
    Ok(value.to_string())
}

fn report_v3_payload(
    name: &str,
    report_date: &str,
    target_fields: Option<&Value>,
) -> Result<Value, ConcurApiCallError> {
    let mut payload = serde_json::Map::new();
    payload.insert("Name".into(), Value::String(name.to_string()));
    payload.insert(
        "UserDefinedDate".into(),
        Value::String(report_date.to_string()),
    );
    let Some(fields) = target_fields.and_then(Value::as_object) else {
        return Ok(Value::Object(payload));
    };
    if fields
        .get("comment")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(ConcurApiCallError {
            message: "当前 Concur Reports v3 不支持写入报销单 Comment；请清空后重新预检，或在 Concur 草稿中补充".to_string(),
            result_unknown: false,
        });
    }
    for (key, value) in fields {
        if matches!(key.as_str(), "name" | "date" | "comment") || !value_present(value) {
            continue;
        }
        let supported = matches!(
            key.as_str(),
            "Country" | "CountrySubdivision" | "CurrencyCode" | "LedgerName"
        ) || key
            .strip_prefix("Custom")
            .and_then(|value| value.parse::<u8>().ok())
            .is_some_and(|index| (1..=20).contains(&index))
            || key
                .strip_prefix("OrgUnit")
                .and_then(|value| value.parse::<u8>().ok())
                .is_some_and(|index| (1..=6).contains(&index));
        if !supported {
            return Err(ConcurApiCallError {
                message: format!(
                    "报销单目标字段 {key} 不能由当前官方 API 安全写入；请从映射配置移除或改用受支持字段"
                ),
                result_unknown: false,
            });
        }
        payload.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(payload))
}

fn build_test_receipt_pdf() -> AppResult<Vec<u8>> {
    use printpdf::{BuiltinFont, Mm, PdfDocument};

    let (document, page, layer) = PdfDocument::new(
        "Invoice Assistant capability test",
        Mm(150.0),
        Mm(90.0),
        "Test receipt",
    );
    let font = document
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|error| AppError::internal(format!("生成测试 PDF 字体失败：{error:?}")))?;
    document.get_page(page).get_layer(layer).use_text(
        "Invoice Assistant capability test receipt",
        12.0,
        Mm(12.0),
        Mm(48.0),
        &font,
    );
    document
        .save_to_bytes()
        .map_err(|error| AppError::internal(format!("生成测试 PDF 失败：{error:?}")))
}

fn expense_v3_payload(report_id: &str, target_fields: &Value) -> Result<Value, ConcurApiCallError> {
    let object = target_fields
        .as_object()
        .ok_or_else(|| ConcurApiCallError {
            message: "冻结费用投影不是有效对象".to_string(),
            result_unknown: false,
        })?;
    let required = |key: &str, label: &str| -> Result<Value, ConcurApiCallError> {
        object
            .get(key)
            .cloned()
            .filter(value_present)
            .ok_or_else(|| ConcurApiCallError {
                message: format!("{label}缺失；请更新 Concur 映射配置并重新预检"),
                result_unknown: false,
            })
    };
    let mut payload = serde_json::Map::new();
    payload.insert("ReportID".into(), Value::String(report_id.to_string()));
    payload.insert(
        "ExpenseTypeCode".into(),
        required("expense_type_id", "费用类型代码")?,
    );
    payload.insert(
        "PaymentTypeID".into(),
        required("payment_type_id", "付款类型 ID")?,
    );
    payload.insert(
        "TransactionDate".into(),
        required("transaction_date", "交易日期")?,
    );
    payload.insert("TransactionAmount".into(), required("amount", "实际金额")?);
    payload.insert(
        "TransactionCurrencyCode".into(),
        required("currency", "币种")?,
    );
    payload.insert("TaxReceiptType".into(), Value::String("T".to_string()));
    if let Some(value) = object
        .get("purchase_city_id")
        .cloned()
        .filter(value_present)
    {
        payload.insert("LocationID".into(), value);
    }
    if let Some(value) = object.get("business_purpose").and_then(Value::as_str) {
        payload.insert(
            "Description".into(),
            Value::String(value.chars().take(64).collect()),
        );
    }
    // v3 VendorDescription 是只读字段，VAT 的完整写入也不在这个 endpoint 的稳定
    // contract 中。它们保留在冻结投影中供用户对账，不伪造为“已写入”。
    for (key, value) in object {
        let known_projection_field = matches!(
            key.as_str(),
            "expense_type_id"
                | "payment_type_id"
                | "transaction_date"
                | "amount"
                | "currency"
                | "purchase_city_id"
                | "business_purpose"
                | "vendor_name"
                | "vat_amount"
                | "vat_rate_ids"
        );
        let supported_custom = indexed_field(key, "Custom", 40) || indexed_field(key, "OrgUnit", 6);
        if supported_custom && value_present(value) {
            payload.insert(key.clone(), value.clone());
        } else if !known_projection_field && value_present(value) {
            return Err(ConcurApiCallError {
                message: format!(
                    "费用目标字段 {key} 不能由当前官方 API 安全写入；请从映射配置移除或改用 Custom/OrgUnit 字段"
                ),
                result_unknown: false,
            });
        }
    }
    Ok(Value::Object(payload))
}

fn indexed_field(value: &str, prefix: &str, maximum: u8) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.parse::<u8>().ok())
        .is_some_and(|index| (1..=maximum).contains(&index))
}

fn value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn account_from_reports(value: &Value) -> Option<ConcurConnectedAccount> {
    value
        .get("Items")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(account_from_report)
}

fn account_from_report(value: &Value) -> Option<ConcurConnectedAccount> {
    let safe_field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && text.chars().count() <= 320)
            .map(str::to_string)
    };
    let login_id = safe_field("OwnerLoginID");
    let display_name = safe_field("OwnerName");
    (login_id.is_some() || display_name.is_some()).then_some(ConcurConnectedAccount {
        login_id,
        display_name,
    })
}

fn response_id(value: &Value, label: &str) -> Result<String, ConcurApiCallError> {
    value
        .get("ID")
        .or_else(|| value.get("Id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| ConcurApiCallError {
            message: format!("Concur 已响应，但没有返回{label} ID"),
            result_unknown: true,
        })
}

fn response_json(
    response: Response,
    operation: &str,
    write: bool,
) -> Result<Value, ConcurApiCallError> {
    let text = response_text(response, operation, write)?;
    serde_json::from_str(&text).map_err(|_| ConcurApiCallError {
        message: format!("{operation}响应格式无法识别"),
        result_unknown: write,
    })
}

fn response_text(
    response: Response,
    operation: &str,
    write: bool,
) -> Result<String, ConcurApiCallError> {
    let status = response.status();
    let text = response.text().map_err(|error| ConcurApiCallError {
        message: format!("{operation}响应读取失败：{error}"),
        result_unknown: write,
    })?;
    if !status.is_success() {
        let detail = text.chars().take(MAX_RESPONSE_CHARS).collect::<String>();
        return Err(ConcurApiCallError {
            message: format!(
                "{operation}失败（HTTP {}）：{}",
                status.as_u16(),
                readable_error_detail(&detail)
            ),
            result_unknown: write && status.is_server_error(),
        });
    }
    Ok(text)
}

fn readable_error_detail(value: &str) -> String {
    if value.trim().is_empty() {
        return "Concur 未返回错误说明".to_string();
    }
    if let Ok(json) = serde_json::from_str::<Value>(value) {
        for key in ["message", "Message", "error_description", "error"] {
            if let Some(message) = json.get(key).and_then(Value::as_str) {
                return message.chars().take(500).collect();
            }
        }
    }
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

fn transport_error(operation: &str, error: reqwest::Error, write: bool) -> ConcurApiCallError {
    ConcurApiCallError {
        message: format!("{operation}网络失败：{error}"),
        result_unknown: write,
    }
}

fn api_error_to_app(error: ConcurApiCallError) -> AppError {
    AppError::network(error.message)
}

fn xml_tag(value: &str, tag: &str) -> Option<String> {
    let start_marker = format!("<{tag}>");
    let end_marker = format!("</{tag}>");
    let start = value.find(&start_marker)? + start_marker.len();
    let end = value[start..].find(&end_marker)? + start;
    let text = value[start..end].trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_official_https_api_hosts() {
        assert_eq!(
            validate_concur_base_url("https://cn.api.concurcdc.cn/").unwrap(),
            "https://cn.api.concurcdc.cn"
        );
        assert_eq!(
            validate_concur_base_url("https://us2.api.concursolutions.com").unwrap(),
            "https://us2.api.concursolutions.com"
        );
        assert!(validate_concur_base_url("http://cn.api.concurcdc.cn").is_err());
        assert!(validate_concur_base_url("https://example.com").is_err());
        assert!(validate_concur_base_url("https://us.api.concursolutions.com/path").is_err());
    }

    #[test]
    fn derives_official_browser_authorization_host_from_data_center() {
        assert_eq!(
            browser_authorization_origin("https://cn.api.concurcdc.cn").unwrap(),
            "https://www-cn.api.concurcdc.cn"
        );
        assert_eq!(
            browser_authorization_origin("https://us2.api.concursolutions.com").unwrap(),
            "https://www-us2.api.concursolutions.com"
        );
    }

    #[test]
    fn browser_authorize_url_contains_fixed_callback_scope_and_state() {
        let value = browser_authorize_url("https://cn.api.concurcdc.cn", "client-123", "state-456")
            .unwrap();
        let parsed = reqwest::Url::parse(&value).unwrap();
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        let redirect_uri = browser_oauth_redirect_uri();
        assert_eq!(parsed.host_str(), Some("www-cn.api.concurcdc.cn"));
        assert_eq!(parsed.path(), "/oauth2/v0/authorize");
        assert_eq!(
            query.get("client_id").map(|value| value.as_ref()),
            Some("client-123")
        );
        assert_eq!(
            query.get("redirect_uri").map(|value| value.as_ref()),
            Some(redirect_uri.as_str())
        );
        assert_eq!(
            query.get("scope").map(|value| value.as_ref()),
            Some(BROWSER_OAUTH_SCOPES)
        );
        assert_eq!(
            query.get("state").map(|value| value.as_ref()),
            Some("state-456")
        );
    }

    #[test]
    fn parses_browser_callback_and_rejects_state_mismatch() {
        let target = "/concur/oauth/callback?code=temporary-code&state=expected&geolocation=https%3A%2F%2Fcn.api.concurcdc.cn";
        let callback = parse_browser_callback_target(target, "expected")
            .unwrap()
            .unwrap();
        assert_eq!(callback.code.as_str(), "temporary-code");
        assert_eq!(
            callback.geolocation.as_deref(),
            Some("https://cn.api.concurcdc.cn")
        );
        assert!(parse_browser_callback_target(target, "different").is_err());
        assert!(parse_browser_callback_target("/favicon.ico", "expected")
            .unwrap()
            .is_none());
    }

    #[test]
    fn normalizes_token_geolocation_without_accepting_foreign_hosts() {
        assert_eq!(
            normalize_token_geolocation("https://cn.api.concurcdc.cn/oauth2/v0/token").unwrap(),
            "https://cn.api.concurcdc.cn"
        );
        assert!(normalize_token_geolocation("https://example.com/oauth2/v0/token").is_err());
    }

    #[test]
    fn parses_space_or_comma_separated_oauth_scopes() {
        assert_eq!(
            parse_granted_scopes("EXPRPT IMAGE,openid"),
            vec!["EXPRPT", "IMAGE", "openid"]
        );
    }

    #[test]
    fn extracts_connected_account_from_report_owner() {
        let account = account_from_reports(&json!({
            "Items": [{
                "OwnerLoginID": "alpha@example.test",
                "OwnerName": "Alpha User"
            }]
        }))
        .unwrap();
        assert_eq!(account.login_id.as_deref(), Some("alpha@example.test"));
        assert_eq!(account.display_name.as_deref(), Some("Alpha User"));
        assert!(account_from_reports(&json!({ "Items": [] })).is_none());
    }

    #[test]
    fn maps_frozen_projection_to_expense_v3_without_vendor_claim() {
        let payload = expense_v3_payload(
            "report-1",
            &json!({
                "expense_type_id": "MEAL",
                "payment_type_id": "cash-id",
                "transaction_date": "2026-06-25",
                "amount": "126.00",
                "currency": "CNY",
                "vendor_name": "测试餐厅",
                "business_purpose": "业务拜访餐费",
                "purchase_city_id": "wuhan-id"
            }),
        )
        .unwrap();
        assert_eq!(payload["ReportID"], "report-1");
        assert_eq!(payload["ExpenseTypeCode"], "MEAL");
        assert_eq!(payload["TransactionAmount"], "126.00");
        assert_eq!(payload["LocationID"], "wuhan-id");
        assert!(payload.get("VendorDescription").is_none());
    }

    #[test]
    fn extracts_concur_image_xml_id() {
        assert_eq!(
            xml_tag("<Image><Id>abc$123</Id><Url /></Image>", "Id").as_deref(),
            Some("abc$123")
        );
        assert!(xml_tag("<Image><Url /></Image>", "Id").is_none());
    }

    #[test]
    fn generated_capability_receipt_is_a_valid_pdf() {
        let bytes = build_test_receipt_pdf().unwrap();
        let parsed = printpdf::lopdf::Document::load_mem(&bytes).unwrap();
        assert_eq!(parsed.get_pages().len(), 1);
        assert!(bytes.len() < 10 * 1024 * 1024);
    }

    #[test]
    fn report_payload_rejects_unknown_target_fields_before_write() {
        let error = report_v3_payload(
            "test",
            "2026-09-04",
            Some(&json!({ "comment": "", "costCenter": "CN-SALES" })),
        )
        .unwrap_err();
        assert!(!error.result_unknown);
        assert!(error.message.contains("costCenter"));
    }

    #[test]
    fn expense_payload_rejects_unknown_target_fields_before_write() {
        let error = expense_v3_payload(
            "report-1",
            &json!({
                "expense_type_id": "MEAL",
                "payment_type_id": "cash-id",
                "transaction_date": "2026-09-04",
                "amount": "1.00",
                "currency": "CNY",
                "tenantMysteryField": "value"
            }),
        )
        .unwrap_err();
        assert!(!error.result_unknown);
        assert!(error.message.contains("tenantMysteryField"));
    }

    #[test]
    fn expense_payload_accepts_structured_custom_fields() {
        let payload = expense_v3_payload(
            "report-1",
            &json!({
                "expense_type_id": "MEAL",
                "payment_type_id": "cash-id",
                "transaction_date": "2026-09-04",
                "amount": "1.00",
                "currency": "CNY",
                "Custom1": {"Type": "Text", "Value": "PROJECT-A"}
            }),
        )
        .unwrap();
        assert_eq!(payload["Custom1"]["Value"], "PROJECT-A");
    }
}
