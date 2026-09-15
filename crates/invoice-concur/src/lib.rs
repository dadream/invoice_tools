//! Concur-owned planning, mappings and delivery journal. Core never imports this crate.
pub mod repository;
use anyhow::{bail, ensure, Result};
use invoice_delivery_contract::{Document, Snapshot};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub required: bool,
    pub control: String,
    pub options: Vec<String>,
    pub source: String,
    pub constant: String,
    pub option_map: BTreeMap<String, String>,
    #[serde(default)]
    pub format: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Form {
    pub signature: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub version: u64,
    pub origin: String,
    pub account: String,
    pub report: Form,
    pub expenses: BTreeMap<String, Form>,
    pub expense_types: BTreeMap<String, String>,
    /// Delivery-only routing for groups other than local_month/business_trip.
    pub group_targets: BTreeMap<String, String>,
}

impl Profile {
    /// Database revision is bookkeeping, not a change in the tested mapping.
    /// Keep every other field (including account and tenant) in the comparison.
    pub fn mapping_digest(&self) -> String {
        let mut content = self.clone();
        content.version = 0;
        digest(&content)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Value {
    pub label: String,
    pub value: String,
    pub control: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub key: String,
    pub kind: String,
    pub label: String,
    pub report_key: String,
    pub expense_key: String,
    pub category: String,
    pub values: Vec<Value>,
    pub signature: String,
    pub document: Option<Document>,
    /// pending -> writing -> verified. writing after failure is uncertain, never blind retry.
    pub status: String,
    pub remote_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub revision: u64,
    pub test_mode: bool,
    pub snapshot: Snapshot,
    pub profile: Profile,
    pub steps: Vec<Step>,
    pub gaps: Vec<String>,
}

impl Job {
    pub fn completed(&self) -> bool {
        !self.steps.is_empty()
            && self.gaps.is_empty()
            && self.steps.iter().all(|s| s.status == "verified")
    }
    pub fn next(&self) -> Option<usize> {
        self.steps.iter().position(|s| s.status != "verified")
    }
    pub fn begin(&mut self, index: usize) -> Result<()> {
        ensure!(self.gaps.is_empty(), "请先补齐映射和材料缺口");
        ensure!(self.next() == Some(index), "只能执行当前未完成步骤");
        ensure!(
            self.steps[index].status == "pending",
            "上次写入结果不确定，请先核对 Concur 页面，不能直接重试"
        );
        self.steps[index].status = "writing".into();
        self.steps[index].message = "正在执行，若中断需回读核对".into();
        Ok(())
    }
}

pub fn digest<T: Serialize>(value: &T) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable model"))
    )
}

fn project(
    form: &Form,
    fields: &BTreeMap<String, String>,
    context: &str,
    gaps: &mut Vec<String>,
) -> Vec<Value> {
    if form.signature.is_empty() {
        gaps.push(format!("{context}：请读取目标表单并保存映射"));
    }
    let mut out = Vec::new();
    for f in &form.fields {
        let raw = if f.source == "constant" {
            f.constant.clone()
        } else {
            fields.get(&f.source).cloned().unwrap_or_default()
        };
        let mut value = if f.option_map.is_empty() {
            raw
        } else {
            f.option_map.get(&raw).cloned().unwrap_or_default()
        };
        if f.format == "MM/DD/YYYY"
            && f.control != "date"
            && matches!(f.source.as_str(), "transaction_date" | "report_date")
            && value.len() == 10
        {
            let parts: Vec<_> = value.split('-').collect();
            if parts.len() == 3 {
                value = format!("{}/{}/{}", parts[1], parts[2], parts[0]);
            }
        }
        if value.trim().is_empty() {
            if f.required {
                gaps.push(format!("{context}：缺少{}", f.label));
            }
            continue;
        }
        if !f.options.is_empty() && !f.options.contains(&value) {
            gaps.push(format!("{context}：{}不在当前选项中", f.label));
        }
        out.push(Value {
            label: f.label.clone(),
            value,
            control: f.control.clone(),
            source: f.source.clone(),
        });
    }
    out
}

/// One local report and one trip report, irrespective of month or trip count.
pub fn plan(
    id: String,
    snapshot: Snapshot,
    profile: Profile,
    test_mode: bool,
    date: &str,
) -> Result<Job> {
    ensure!(!snapshot.expenses.is_empty(), "没有已计入的费用");
    ensure!(
        !profile.origin.is_empty() && !profile.account.is_empty(),
        "请先连接并确认 Concur 账号"
    );
    let mut gaps = Vec::new();
    let mut routed: BTreeMap<String, Vec<_>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for expense in &snapshot.expenses {
        ensure!(seen.insert(expense.id), "快照中出现重复费用");
        let target = match expense.group_kind.as_str() {
            "business_trip" => "trip",
            "local_month" => "local",
            _ => profile
                .group_targets
                .get(&expense.group_id.to_string())
                .map(String::as_str)
                .unwrap_or(""),
        };
        if !matches!(target, "local" | "trip") {
            gaps.push(format!("{}：请选择本地或差旅报销单", expense.group_title));
            continue;
        }
        routed.entry(target.into()).or_default().push(expense);
    }
    let prefix = if test_mode {
        "[测试-请勿提交] "
    } else {
        ""
    };
    let mut steps = Vec::new();
    for (target, expenses) in routed {
        let report_key = format!("IA-{}-{}", &digest(&id)[..10], target);
        let title = format!(
            "{prefix}{}-{} [{report_key}]",
            snapshot.name,
            if target == "local" {
                "本地费用"
            } else {
                "差旅费用"
            }
        );
        let fields = BTreeMap::from([
            ("report_name".into(), title.clone()),
            ("report_date".into(), date.into()),
            ("comment".into(), report_key.clone()),
        ]);
        if !profile
            .report
            .fields
            .iter()
            .any(|f| f.source == "report_name")
        {
            gaps.push("报销单必须映射名称，以便恢复和防重".into());
        }
        let values = project(&profile.report, &fields, &title, &mut gaps);
        steps.push(Step {
            key: report_key.clone(),
            kind: "report".into(),
            label: title,
            report_key: report_key.clone(),
            expense_key: String::new(),
            category: String::new(),
            values,
            signature: profile.report.signature.clone(),
            document: None,
            status: "pending".into(),
            remote_url: String::new(),
            message: String::new(),
        });
        for expense in expenses {
            let category = expense
                .fields
                .get("category_code")
                .cloned()
                .unwrap_or_default();
            let mut fields = expense.fields.clone();
            let amount = fields
                .get("gross_amount")
                .and_then(|s| Decimal::from_str(s).ok());
            if amount.is_none() {
                gaps.push(format!("费用 #{}：金额无效", expense.id));
            }
            let expense_key = format!("{report_key}-E{}", expense.id);
            fields.insert("comment".into(), expense_key.clone());
            let label = format!(
                "{} · {} {}",
                fields.get("transaction_date").cloned().unwrap_or_default(),
                fields.get("description").cloned().unwrap_or_default(),
                fields.get("gross_amount").cloned().unwrap_or_default()
            );
            let form = profile.expenses.get(&category).cloned().unwrap_or_default();
            for source in ["gross_amount", "transaction_date", "comment"] {
                if !form
                    .fields
                    .iter()
                    .any(|f| f.source == source && f.option_map.is_empty())
                {
                    gaps.push(format!(
                        "{label}：必须直接映射{source}，保证金额、日期和恢复标记可靠"
                    ));
                }
            }
            if profile
                .expense_types
                .get(&category)
                .map_or(true, |s| s.is_empty())
            {
                gaps.push(format!("{label}：请选择 Concur 费用类型"));
            }
            let values = project(&form, &fields, &label, &mut gaps);
            steps.push(Step {
                key: expense_key.clone(),
                kind: "expense".into(),
                label,
                report_key: report_key.clone(),
                expense_key: expense_key.clone(),
                category,
                values,
                signature: form.signature,
                document: None,
                status: "pending".into(),
                remote_url: String::new(),
                message: String::new(),
            });
            let mut hashes = BTreeSet::new();
            let mut file_names = BTreeSet::new();
            let docs: Vec<_> = expense
                .documents
                .iter()
                .filter(|d| d.role != "duplicate_copy")
                .collect();
            if docs.is_empty() {
                gaps.push(format!("费用 #{}：没有可上传材料", expense.id));
            }
            for doc in docs {
                if doc.sha256.is_empty() {
                    gaps.push(format!("{}：材料尚未校验", doc.name));
                    continue;
                }
                if !hashes.insert(&doc.sha256) {
                    continue;
                }
                if !file_names.insert(doc.name.to_ascii_lowercase()) {
                    gaps.push(format!(
                        "{}：同一费用有不同内容的同名文件，请在原始材料中确认名称后再上传",
                        doc.name
                    ));
                }
                let ext = std::path::Path::new(&doc.path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !matches!(
                    ext.as_str(),
                    "pdf" | "png" | "jpg" | "jpeg" | "tif" | "tiff"
                ) {
                    gaps.push(format!("{}：请提供该材料独立的 PDF 或图片版本", doc.name));
                }
                steps.push(Step {
                    key: format!("{expense_key}-D{}", doc.id),
                    kind: "attachment".into(),
                    label: doc.name.clone(),
                    report_key: report_key.clone(),
                    expense_key: expense_key.clone(),
                    category: String::new(),
                    values: Vec::new(),
                    signature: String::new(),
                    document: Some(doc.clone()),
                    status: "pending".into(),
                    remote_url: String::new(),
                    message: String::new(),
                });
            }
        }
    }
    gaps.sort();
    gaps.dedup();
    Ok(Job {
        id,
        revision: 0,
        test_mode,
        snapshot,
        profile,
        steps,
        gaps,
    })
}

pub fn check_document(doc: &Document) -> Result<()> {
    let metadata = std::fs::metadata(&doc.path)?;
    ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= 5 * 1024 * 1024,
        "{}：材料为空或超过 5 MB，请先处理单份文件",
        doc.name
    );
    let bytes = std::fs::read(&doc.path)?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == doc.sha256,
        "{}：材料已变化，请重新生成交付计划",
        doc.name
    );
    if doc.path.to_lowercase().ends_with(".pdf") && !bytes.starts_with(b"%PDF-") {
        bail!("{}：不是有效的 PDF 文件", doc.name);
    }
    Ok(())
}
