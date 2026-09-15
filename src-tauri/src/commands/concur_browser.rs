//! Application composition only: read core snapshot, invoke independent Concur module.
use crate::{
    error::{AppError, AppResult},
    AppState,
};
use invoice_concur::{repository::Repository, Job, Profile};
use invoice_delivery_contract::{Document, Expense, Snapshot};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Mutex},
    time::Duration,
};
use tauri::{Manager, State};

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::validation(e.to_string())
}
fn repository() -> AppResult<Repository> {
    Repository::open(&crate::paths::data_root()?.join("ledger.db")).map_err(err)
}

pub(crate) fn assets() -> AppResult<PathBuf> {
    let packaged = std::env::current_exe()?
        .parent()
        .ok_or_else(|| err("无法定位程序目录"))?
        .join("concur-browser");
    if packaged.join("worker.mjs").is_file() {
        return Ok(packaged);
    }
    #[cfg(any(debug_assertions, test))]
    {
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/concur-browser"))
    }
    #[cfg(not(any(debug_assertions, test)))]
    Err(err("软件包缺少 Concur 浏览器模块，请重新下载完整免安装包"))
}

#[derive(Default)]
pub struct BrowserRuntime {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<mpsc::Receiver<Value>>,
}
impl Drop for BrowserRuntime {
    fn drop(&mut self) {
        self.input.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl BrowserRuntime {
    fn call(&mut self, op: &str, args: Value) -> AppResult<Value> {
        if self
            .child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_some())
        {
            self.child = None;
            self.input = None;
            self.output = None;
        }
        if self.child.is_none() {
            let root = assets()?;
            let node = root.join("node.exe");
            #[cfg(any(debug_assertions, test))]
            let node = if node.is_file() {
                node
            } else {
                PathBuf::from("C:/nvm4w/nodejs/node.exe")
            };
            if !node.is_file() {
                return Err(err("软件包缺少浏览器运行组件，请使用完整免安装包"));
            }
            let mut command = Command::new(node);
            command
                .arg(root.join("worker.mjs"))
                .current_dir(&root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .spawn()
                .map_err(|_| err("无法启动浏览器组件，请检查企业终端限制或软件包完整性"))?;
            self.input = child.stdin.take();
            let out = child.stdout.take().ok_or_else(|| err("浏览器通信未建立"))?;
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if line.len() > 8 * 1024 * 1024 {
                        break;
                    }
                    if let Ok(value) = serde_json::from_str(&line) {
                        if sender.send(value).is_err() {
                            break;
                        }
                    }
                }
            });
            self.child = Some(child);
            self.output = Some(receiver);
        }
        let input = self.input.as_mut().ok_or_else(|| err("浏览器连接已断开"))?;
        writeln!(input, "{}", json!({"op":op,"args":args}))?;
        input.flush()?;
        let result = self
            .output
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(150));
        let result = match result {
            Ok(v) => v,
            Err(_) => {
                self.input.take();
                if let Some(mut c) = self.child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                self.output = None;
                return Err(err(
                    "浏览器响应中断。写入可能已经完成，请重新登录并先核对结果。",
                ));
            }
        };
        if result["ok"] == true {
            Ok(result["data"].clone())
        } else {
            Err(err(result["error"]
                .as_str()
                .unwrap_or("浏览器操作失败，请核对页面")))
        }
    }
}

fn snapshot(state: &AppState, batch_id: i64) -> AppResult<Snapshot> {
    let db = state.ledger_db()?;
    let (review, items) = db.get_active_snapshot_expenses(batch_id).map_err(err)?;
    let groups = db
        .get_active_snapshot_grouping(batch_id)
        .map_err(err)?
        .map(|g| g.groups)
        .unwrap_or_default();
    let batch = db.get_batch(batch_id).map_err(err)?;
    let mut expenses = Vec::new();
    for item in items {
        if item.inclusion_status != "included" {
            continue;
        }
        let group = groups.iter().find(|g| Some(g.id) == item.trip_group_id);
        let mut fields = BTreeMap::from([
            ("category_code".into(), item.category_code),
            ("transaction_date".into(), item.transaction_date.to_string()),
            ("description".into(), item.description),
            ("counterparty_name".into(), item.counterparty_name),
            ("gross_amount".into(), item.gross_amount.to_string()),
            ("currency_code".into(), item.currency_code),
            ("payment_method".into(), item.payment_method),
            (
                "city_name".into(),
                item.location.city_name.unwrap_or_default(),
            ),
            (
                "province_name".into(),
                item.location.province_name.unwrap_or_default(),
            ),
        ]);
        if !item.tax_details.is_empty() {
            fields.insert(
                "tax_amount".into(),
                item.tax_details
                    .iter()
                    .map(|t| t.amount)
                    .sum::<rust_decimal::Decimal>()
                    .to_string(),
            );
        }
        let pdf_sources: Vec<_> = item
            .documents
            .iter()
            .filter(|d| {
                d.file_path.to_ascii_lowercase().ends_with(".pdf") && d.role != "duplicate_copy"
            })
            .filter_map(|d| d.source_invoice_id.map(|id| (id, d.role.clone())))
            .collect();
        let mut documents = Vec::new();
        for d in item.documents {
            if d.role == "duplicate_copy" {
                continue;
            }
            // Only suppress an OFD alternative with the SAME invoice provenance and role.
            if d.file_path.to_ascii_lowercase().ends_with(".ofd")
                && d.source_invoice_id
                    .is_some_and(|id| pdf_sources.contains(&(id, d.role.clone())))
            {
                continue;
            }
            let sha = d
                .sha256
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    std::fs::read(&d.file_path)
                        .ok()
                        .map(|b| format!("{:x}", Sha256::digest(b)))
                })
                .unwrap_or_default();
            documents.push(Document {
                id: d.id,
                role: d.role,
                path: d.file_path,
                name: d.original_name,
                sha256: sha,
            });
        }
        expenses.push(Expense {
            id: item.id,
            group_id: group.map(|g| g.id).unwrap_or(0),
            group_title: group
                .map(|g| g.title.clone())
                .unwrap_or_else(|| "无归组".into()),
            group_kind: group.map(|g| g.kind.clone()).unwrap_or_default(),
            fields,
            documents,
        });
    }
    Ok(Snapshot {
        batch_id,
        snapshot_id: review.id,
        fingerprint: review.content_sha256,
        name: batch.name,
        expenses,
    })
}

fn sample() -> AppResult<Snapshot> {
    let root = assets()?.join("samples");
    let mut snapshot: Snapshot =
        serde_json::from_slice(&std::fs::read(root.join("sample.json"))?).map_err(err)?;
    let root = root.canonicalize()?;
    for expense in &mut snapshot.expenses {
        for doc in &mut expense.documents {
            let path = root.join(&doc.path).canonicalize()?;
            if !path.starts_with(&root) {
                return Err(err("内置样例路径无效"));
            }
            doc.path = path.to_string_lossy().into();
            if format!("{:x}", Sha256::digest(std::fs::read(&path)?)) != doc.sha256 {
                return Err(err("内置测试材料校验失败，请重新下载软件包"));
            }
        }
    }
    Ok(snapshot)
}

#[tauri::command]
pub fn get_concur_browser_workspace(
    batch_id: Option<i64>,
    state: State<Mutex<AppState>>,
) -> AppResult<Value> {
    let repo = repository()?;
    let source = if let Some(id) = batch_id {
        snapshot(&*state.lock().map_err(err)?, id)?
    } else {
        sample()?
    };
    let jobs = repo
        .list()
        .map_err(err)?
        .into_iter()
        .filter(|j| {
            if let Some(id) = batch_id {
                !j.test_mode && j.snapshot.batch_id == id
            } else {
                j.test_mode
            }
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"profile":repo.profile().map_err(err)?.unwrap_or_default(),"snapshot":source,"jobs":jobs}),
    )
}

#[tauri::command]
pub async fn concur_browser_control(
    action: String,
    payload: Value,
    app: tauri::AppHandle,
) -> AppResult<Value> {
    tauri::async_runtime::spawn_blocking(move || {
        if !["launch", "status", "confirm", "discover", "close"].contains(&action.as_str()) {
            return Err(err("不支持的连接操作"));
        }
        let runtime = app.state::<Mutex<BrowserRuntime>>();
        let mut runtime = runtime
            .try_lock()
            .map_err(|_| err("浏览器正在处理，请等待当前步骤结束"))?;
        runtime.call(&action, payload)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn save_concur_browser_profile(profile: Profile) -> AppResult<Profile> {
    if profile.origin.is_empty() || profile.account.is_empty() {
        return Err(err("请先确认已连接账号"));
    }
    repository()?.save_profile(profile).map_err(err)
}

#[tauri::command]
pub fn prepare_concur_browser_job(
    batch_id: Option<i64>,
    profile: Profile,
    save: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<Job> {
    let source = if let Some(id) = batch_id {
        snapshot(&*state.lock().map_err(err)?, id)?
    } else {
        sample()?
    };
    let job = invoice_concur::plan(
        uuid::Uuid::new_v4().to_string(),
        source,
        profile,
        batch_id.is_none(),
        &chrono::Local::now().date_naive().to_string(),
    )
    .map_err(err)?;
    if save {
        if !job.gaps.is_empty() {
            return Err(err("请先补齐缺口再冻结交付计划"));
        }
        if !job.test_mode {
            let tested = repository()?
                .has_completed_test_for(&job.profile)
                .map_err(err)?;
            if !tested {
                return Err(err(
                    "请先使用同一账号及映射完成内置样例测试，再执行真实批次",
                ));
            }
        }
        for step in &job.steps {
            if let Some(doc) = &step.document {
                invoice_concur::check_document(doc).map_err(err)?;
            }
        }
        repository()?
            .create(&job)
            .map_err(|_| err("该审核版本已有交付计划，请在交付记录中恢复，避免重复创建"))?;
    }
    Ok(job)
}

#[tauri::command]
pub async fn run_concur_browser_step(
    job_id: String,
    operation: String,
    app: tauri::AppHandle,
) -> AppResult<Job> {
    tauri::async_runtime::spawn_blocking(move || {
        let runtime = app.state::<Mutex<BrowserRuntime>>();
        let mut runtime = runtime
            .try_lock()
            .map_err(|_| err("已有浏览器步骤正在执行"))?;
        let repo = repository()?;
        let mut job = repo.get(&job_id).map_err(err)?;
        let Some(index) = job.next() else {
            return Ok(job);
        };
        if !["execute", "verify", "retry_confirmed_absent"].contains(&operation.as_str()) {
            return Err(err("不支持的交付操作"));
        }
        // Hold the app's core lock for this single external step; snapshot cannot be reopened midway.
        let core = app.state::<Mutex<AppState>>();
        let core = core.lock().map_err(err)?;
        if !job.test_mode {
            let current = core
                .ledger_db()?
                .get_active_batch_review_snapshot(job.snapshot.batch_id)
                .map_err(err)?;
            if !current.is_some_and(|s| {
                s.id == job.snapshot.snapshot_id && s.content_sha256 == job.snapshot.fingerprint
            }) {
                return Err(err(
                    "该审核版本已失效。旧交付记录保留，请先核对 Concur 草稿，不能继续旧计划。",
                ));
            }
        }
        if operation == "retry_confirmed_absent" {
            if job.steps[index].status != "writing" {
                return Err(err("只有未确定结果的步骤需要人工恢复"));
            }
            job.steps[index].status = "pending".into();
            job.steps[index].message = "用户已确认 Concur 尚未保存该步骤".into();
            repo.update(&mut job).map_err(err)?;
            return Ok(job);
        }
        if operation == "execute" {
            let expense_key = job.steps[index].expense_key.clone();
            if let Some(doc) = &mut job.steps[index].document {
                // Portable restore/reinstall can move files. Rebind by immutable source id+hash,
                // never by an unchecked filename or a user-supplied replacement.
                let current = if job.test_mode {
                    sample()?
                } else {
                    snapshot(&core, job.snapshot.batch_id)?
                };
                let source_expense = job
                    .snapshot
                    .expenses
                    .iter()
                    .find(|e| expense_key.ends_with(&format!("-E{}", e.id)))
                    .map(|e| e.id);
                if let Some(found) = current
                    .expenses
                    .iter()
                    .filter(|e| Some(e.id) == source_expense)
                    .flat_map(|e| &e.documents)
                    .find(|d| d.id == doc.id && d.sha256 == doc.sha256)
                {
                    doc.path = found.path.clone();
                }
                invoice_concur::check_document(doc).map_err(err)?;
            }
            job.begin(index).map_err(err)?;
            repo.update(&mut job).map_err(err)?;
        } else if job.steps[index].status != "writing" {
            return Err(err("该步骤尚未执行，不能标记通过"));
        }
        let result = runtime.call(&operation, json!({"job":job,"index":index}));
        match result {
            Ok(value) => {
                job.steps[index].status = "verified".into();
                job.steps[index].remote_url = value["url"].as_str().unwrap_or_default().into();
                job.steps[index].message = "已保存并回读通过".into();
            }
            Err(e) => {
                job.steps[index].message = e.to_string();
            }
        }
        repo.update(&mut job).map_err(err)?;
        if !job.test_mode && job.completed() {
            let db = core.ledger_db()?;
            let task = db
                .start_delivery_task(job.snapshot.batch_id, "concur")
                .map_err(err)?;
            db.finish_delivery_task(task.id, None, None).map_err(err)?;
        }
        Ok(job)
    })
    .await
    .map_err(err)?
}
