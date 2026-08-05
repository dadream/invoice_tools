# MVP架构分析：本地优先 vs 云端服务

**日期：** 2026-08-05  
**目标：** 明确MVP功能的本地/云端边界，确保正确性和可信度

---

## 核心原则

1. **本地优先（Local-First）：** 核心功能在用户设备本地运行，无网络依赖
2. **云端仅必要：** 只有技术上必须云端或成本收益明显的功能才上云
3. **渐进增强：** 离线可用基础功能，联网增强体验

---

## 功能边界分析

### ✅ 必须本地（Local-Only）

| 功能 | 理由 | 技术实现 |
|------|------|----------|
| **文件读取** | 隐私，无需网络 | Rust `std::fs` |
| **XML解析** | 100%成功，纯计算 | Rust `quick-xml` |
| **PDF文本层提取** | 66.7%成功，纯计算 | Rust `pdf-extract` |
| **字段比对验证** | 逻辑简单，无外部依赖 | Rust `rust_decimal` |
| **本地数据存储** | 用户数据不上传 | SQLite本地数据库 |
| **历史记录查询** | 本地数据，即时响应 | SQLite查询 |
| **UI渲染** | 桌面应用或本地web | Tauri/Leptos/Yew |

**价值：** 零延迟、零成本、隐私保护

---

### ⚠️ 可选云端（Cloud-Optional）

| 功能 | 本地方案 | 云端方案 | 推荐 |
|------|----------|----------|------|
| **OCR（PDF无文本层）** | Python PaddleOCR sidecar | 百度OCR API | 本地 |
| **OCR（纸质发票照片）** | 同上 | 腾讯OCR API | 本地 |
| **数据备份** | 手动导出JSON/CSV | 自动云同步 | 本地（v0.5云端） |
| **多设备同步** | 不支持 | 云端数据库 | 本地（v1.0云端） |
| **批量处理** | 本地队列 | 云端worker | 本地 |

**理由：**
- **OCR本地方案已验证**（80%准确率），无需云端
- **用户数据敏感**（财务发票），本地存储更安全
- **成本优势**（本地OCR免费，云端API按次计费）

**v0.5云端增强考虑：**
- 如果用户需要多设备访问 → 可选云同步
- 如果OCR准确率不够 → 提供云端高精度OCR选项

---

### ❌ 必须云端（Cloud-Required）

| 功能 | 本地不可行原因 | 云端方案 | 成本 |
|------|---------------|----------|------|
| **发票真伪验证** | 需查询税局数据库 | 付费验签API | ¥0.30/次 |
| **作废/红冲检测** | 需实时税局状态 | 同上 | 包含在验签 |
| **Concur上传** | 外部系统集成 | Concur Expense API | 免费（用户账号） |
| **发票平台下载链接跟踪** | 邮件链接需联网下载 | 云端agent | 按需（v0.5） |

**理由：**
- **验签必须联网**：税局数字签名和发票状态都在云端
- **Concur集成必须联网**：外部SaaS平台
- **成本可接受**：验签¥0.30/张，Concur免费

**优化策略：**
- 批量验签：积攒10-20张一次性验证，降低API调用次数
- 缓存验签结果：已验证的发票不重复查验
- 离线模式：验签失败不阻塞解析，标记"待验证"状态

---

## MVP架构设计

### 本地优先架构（推荐）

```
┌─────────────────────────────────────────────┐
│           用户设备（本地运行）                │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │   Tauri桌面应用 (Rust + Web前端)    │   │
│  └──────────────┬──────────────────────┘   │
│                 │                            │
│  ┌──────────────▼──────────────────────┐   │
│  │      invoice-parse (Rust)           │   │
│  │  • XML解析 (L0)                     │   │
│  │  • PDF文本提取 (L1)                 │   │
│  │  • 字段验证                         │   │
│  └──────────────┬──────────────────────┘   │
│                 │                            │
│  ┌──────────────▼──────────────────────┐   │
│  │  Python OCR Sidecar (本地进程)      │   │
│  │  • PaddleOCR (L2)                   │   │
│  │  • 80%准确率已验证                  │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │   SQLite 本地数据库                 │   │
│  │  • 发票原始文件 (BLOB)              │   │
│  │  • 解析结果                         │   │
│  │  • 历史记录                         │   │
│  └─────────────────────────────────────┘   │
│                                             │
└────────────┬────────────────────────────────┘
             │
             │ 仅必要时联网
             ▼
┌─────────────────────────────────────────────┐
│              云端服务（按需调用）            │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │   付费验签API (¥0.30/次)            │   │
│  │  • 税局数字签名验证                 │   │
│  │  • 作废/红冲状态查询                │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │   Concur Expense API (免费)         │   │
│  │  • 上传发票到费控系统               │   │
│  └─────────────────────────────────────┘   │
│                                             │
└─────────────────────────────────────────────┘
```

**数据流：**
1. 用户拖拽发票文件到应用窗口
2. 本地解析：XML → 100%成功，PDF文本 → 66.7%成功
3. 如需OCR：调用本地Python sidecar（80%成功）
4. 解析结果保存到本地SQLite
5. **用户选择"验证真伪"** → 调用云端验签API（可选，联网）
6. **用户点击"上传到Concur"** → 调用Concur API（可选，联网）

**离线能力：**
- ✅ 解析发票提取字段（XML/PDF全功能）
- ✅ OCR识别（本地PaddleOCR）
- ✅ 查看历史记录
- ❌ 验证真伪（需联网）
- ❌ 上传Concur（需联网）

---

## 技术栈选型

### 桌面应用框架

**推荐：Tauri**

| 方案 | 优势 | 劣势 |
|------|------|------|
| **Tauri** | Rust后端+Web前端，包体小(<5MB)，跨平台 | 生态较新 |
| Electron | 生态成熟，开发快 | 包体大(50-100MB)，内存占用高 |
| 纯Rust GUI (egui) | 原生性能，无依赖 | UI开发效率低，不够现代 |

**选择理由：**
- Tauri与spike代码（Rust）无缝集成
- 包体小，用户下载快
- 前端可用React/Vue，开发效率高

### 本地数据库

**推荐：SQLite**

**理由：**
- 单文件，无服务器，零配置
- Rust生态成熟（`rusqlite` crate）
- 适合本地应用，支持并发读

**Schema设计：**
```sql
CREATE TABLE invoices (
    id INTEGER PRIMARY KEY,
    file_path TEXT NOT NULL,
    file_hash TEXT NOT NULL UNIQUE,  -- SHA256去重
    file_blob BLOB,                   -- 原始文件
    format TEXT NOT NULL,             -- xml/ofd/pdf-vat/pdf-rail/image
    parse_level TEXT NOT NULL,        -- L0/L1/L2/L4
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE parsed_fields (
    id INTEGER PRIMARY KEY,
    invoice_id INTEGER NOT NULL,
    invoice_number TEXT,
    issue_date TEXT,
    total_amount REAL,
    tax_amount REAL,
    tax_rate REAL,
    buyer_name TEXT,
    seller_name TEXT,
    confidence REAL,
    FOREIGN KEY (invoice_id) REFERENCES invoices(id)
);

CREATE TABLE verification_status (
    id INTEGER PRIMARY KEY,
    invoice_id INTEGER NOT NULL,
    verified_at TIMESTAMP,
    status TEXT,  -- valid/invalid/not_verified/pending
    api_response TEXT,  -- JSON from API
    FOREIGN KEY (invoice_id) REFERENCES invoices(id)
);

CREATE TABLE concur_uploads (
    id INTEGER PRIMARY KEY,
    invoice_id INTEGER NOT NULL,
    uploaded_at TIMESTAMP,
    concur_receipt_id TEXT,
    status TEXT,  -- success/failed/pending
    error_message TEXT,
    FOREIGN KEY (invoice_id) REFERENCES invoices(id)
);
```

### OCR Sidecar集成

**已验证方案：** Python subprocess

```rust
// crates/invoice-parse/src/ocr.rs (已实现)
pub fn recognize_via_sidecar(image_path: &Path) -> anyhow::Result<Vec<TextBox>> {
    let output = std::process::Command::new("python3")
        .arg("tools/ocr_sidecar.py")
        .arg(image_path)
        .output()?;
    
    let json = String::from_utf8(output.stdout)?;
    let boxes: Vec<TextBox> = serde_json::from_str(&json)?;
    Ok(boxes)
}
```

**部署策略：**
- Tauri打包时包含`tools/ocr_sidecar.py`
- 检测系统是否有Python3 + PaddleOCR
- 如无，提供一键安装脚本或内置Python环境（PyOxidizer）

---

## 云端API集成设计

### 1. 付费验签API

**服务商选择：**
- **航天信息（官方）：** ¥0.30/次，准确率最高
- **百望云：** ¥0.20/次，速度快
- **诺诺网：** ¥0.25/次，API友好

**集成方式：**
```rust
// crates/invoice-verify/src/api.rs (新建)
pub async fn verify_invoice_online(
    invoice_number: &str,
    invoice_code: &str,
    amount: Decimal,
    date: NaiveDate,
) -> Result<VerificationResult, ApiError> {
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.baiwang.com/verify")
        .header("Authorization", format!("Bearer {}", API_KEY))
        .json(&json!({
            "invoice_number": invoice_number,
            "invoice_code": invoice_code,
            "amount": amount.to_string(),
            "date": date.to_string(),
        }))
        .send()
        .await?;
    
    // 解析响应，返回 valid/invalid/cancelled
    Ok(VerificationResult::from_api_response(response))
}
```

**成本优化：**
- 批量验证折扣（积攒10张 → 一次API调用）
- 缓存90天（税局规定发票有效期内不会改变状态）
- 仅用户主动触发（不自动验证所有发票）

### 2. Concur集成

**API文档：** https://developer.concur.com/api-reference/expense/

**关键endpoint：**
- `POST /api/v3.0/expense/receipts` - 上传收据图片
- `POST /api/v3.0/expense/entries` - 创建费用条目
- `POST /api/v3.0/expense/reports` - 创建报销单

**数据映射：**
```rust
// crates/concur-integration/src/mapper.rs (新建)
pub fn to_concur_receipt(parsed: &ParsedInvoice) -> ConcurReceipt {
    ConcurReceipt {
        transaction_date: parsed.issue_date,
        transaction_amount: parsed.total_amount,
        currency_code: "CNY",
        vendor_name: parsed.seller_name.clone(),
        receipt_image: base64_encode(&parsed.source_file),
        custom_fields: vec![
            ("invoice_number", parsed.invoice_number.clone()),
            ("tax_amount", parsed.tax_amount.to_string()),
        ],
    }
}
```

**认证方式：**
- OAuth 2.0（用户授权一次，token有效期180天）
- Refresh token自动续期

---

## MVP功能边界

### ✅ MVP必须包含

| 功能 | 本地/云端 | 优先级 | 验证状态 |
|------|----------|--------|----------|
| 拖拽上传发票文件 | 本地 | P0 | - |
| XML解析（L0） | 本地 | P0 | ✅ 100%成功 |
| PDF文本提取（L1） | 本地 | P0 | ✅ 66.7%成功 |
| PDF OCR（L2） | 本地 | P0 | ✅ 80%成功 |
| 文档分类（过滤非发票） | 本地 | P0 | ⏸️ 待实现 |
| 字段展示和编辑 | 本地 | P0 | - |
| 本地数据库存储 | 本地 | P0 | - |
| 历史记录查询 | 本地 | P0 | - |
| 发票真伪验证 | 云端 | P1 | ⚠️ 需API对接 |
| 上传到Concur | 云端 | P0 | ⏸️ 需集成 |

**MVP范围：**
- 用户可以本地解析发票，编辑字段，查看历史
- 点击"上传到Concur"一键提交（联网）
- 可选"验证真伪"（联网，付费）

### ⏸️ v0.5延后

| 功能 | 理由 |
|------|------|
| OFD支持 | 渲染问题未解决，23个样本无法处理 |
| 图片OCR | 样本不足（仅6个装饰图），需真实纸质发票 |
| 云端数据同步 | MVP单机够用，多设备需求待验证 |
| 批量导入邮件 | spike已完成采集器，但MVP用户手动拖拽即可 |
| 自动分类规则配置 | MVP用内置规则，v0.5开放自定义 |

### ❌ 不在路线图

| 功能 | 理由 |
|------|------|
| 移动端App | MVP聚焦桌面报销场景 |
| 发票代开 | 不是我们的业务 |
| 在线协作 | 个人工具，无需多人编辑 |

---

## 部署和分发

### 桌面应用打包

**Tauri打包产物：**
- Windows: `.msi` 安装包（~10MB）
- macOS: `.dmg` 磁盘镜像（~8MB）
- Linux: `.AppImage` 或 `.deb`（~12MB）

**依赖处理：**
1. **Rust二进制：** Tauri自动打包
2. **Python OCR sidecar：** 
   - 选项A：要求用户自行安装Python + PaddleOCR（提供安装脚本）
   - 选项B：打包内置Python环境（PyOxidizer，增加30-50MB）
   - **推荐选项A**（MVP快速迭代，v0.5优化体验）

### 更新机制

**Tauri内置updater：**
- 检查GitHub Releases
- 后台下载新版本
- 提示用户重启更新

---

## 成本估算（MVP阶段）

### 开发成本

| 项目 | 工作量 | 成本 |
|------|--------|------|
| Tauri桌面应用框架 | 3天 | - |
| UI设计与实现 | 5天 | - |
| SQLite集成 | 2天 | - |
| Concur API对接 | 5天 | - |
| 测试和bug修复 | 5天 | - |
| **总计** | **20天** | **人力成本** |

### 运营成本（每月）

| 项目 | 单价 | 用量 | 月成本 |
|------|------|------|--------|
| 付费验签API | ¥0.30/次 | 100张（假设20%验证率，500张/月发票） | ¥30 |
| Concur API | 免费 | - | ¥0 |
| 云端服务器 | - | - | ¥0 |
| **总计** | - | - | **¥30/月** |

**结论：** MVP阶段几乎零运营成本（本地优先策略）

---

## 风险评估

### 高风险

1. **OFD渲染未解决** → 23个样本无法处理（25%）
   - 缓解：MVP不支持OFD，用户手动输入或v0.5修复

2. **Python环境依赖** → 用户可能没有Python/PaddleOCR
   - 缓解：提供一键安装脚本，或v0.5内置Python

3. **Concur API变更** → 官方API可能调整
   - 缓解：关注官方文档，设计适配层

### 中风险

4. **OCR准确率波动** → 80%平均，个别发票可能失败
   - 缓解：允许用户手动编辑，标注"低置信度"

5. **付费验签API稳定性** → 第三方服务可用性
   - 缓解：设计重试机制，支持多服务商切换

### 低风险

6. **跨平台兼容性** → Tauri可能在某些系统版本有问题
   - 缓解：优先支持Windows 10+，macOS 11+

---

## 下一步行动

### 立即启动（本周）

1. **创建Plan 1.5文档** - 选项B任务详细分解
2. **创建Plan 2文档** - MVP开发路线图
3. **技术验证：**
   - Tauri + Rust集成spike代码（1天）
   - SQLite Schema设计和测试（1天）
   - Concur API沙盒测试（2天）

### 第一个里程碑（2周后）

- 文档分类器完成（Plan 1.5 Task 2）
- Tauri桌面应用可以加载spike解析器
- 本地数据库存储和查询可用
- UI可以展示解析结果

---

**总结：**
- ✅ **本地优先**：核心解析、OCR、数据存储全在本地
- ✅ **云端仅必要**：验签API、Concur集成需联网
- ✅ **成本可控**：MVP月成本<¥50
- ✅ **隐私保护**：用户数据不上传云端
- ✅ **离线可用**：无网络环境可解析和查看历史

**推荐架构：Tauri桌面应用 + SQLite + Python OCR sidecar + 可选云端API**
