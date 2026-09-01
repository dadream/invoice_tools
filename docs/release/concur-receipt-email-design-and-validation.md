# Concur 收据邮件设计与验证记录

> 版本：0.1，2026-08-20
> 实施状态：代码、UI、模拟测试和内部 Alpha 关闭门禁已完成；真实 Concur 外发未授权、未执行。

## 1. 产品边界

本功能只把用户已经审核的收据附件，经用户当前会话中的邮箱发送到用户明确填写的 Concur 收件地址。软件不会猜测收件地址，也不会创建费用条目、填写报销单、关联收据或提交报销单。

内部 Alpha 的 `INVOICE_ENABLE_CONCUR_SEND` 必须关闭。用户可以建立和检查本地发送计划，但后端会在读取附件和连接 SMTP 之前拒绝真实发送。任何真实试发仍需产品负责人针对该次测试单独批准。

## 2. 用户流程

1. 批次必须处于“已审批”或“已完成”，排除项不进入计划。
2. 用户在设置中输入发件邮箱及本次会话授权码；授权码只驻留进程内存。
3. 用户粘贴 Concur 收件地址，选择一张代表性收据，并确认发件邮箱已在 Concur 验证。
4. “建立试发计划”只在本地校验文件、生成结构化附件名、计算 SHA-256 和幂等键，不联网。
5. 启用真实发送的受控构建中，用户再次勾选确认后，只发送这一张试发收据。
6. 用户到 Concur 核对，选择实际行为：“只进入 Available Receipts”或“ExpenseIt 生成了费用条目”。确认前不能批量发送。
7. 用户确认剩余数量后批量发送；每封 1–5 张、附件合计不超过 20 MiB。成功项不重发，明确的发送前失败项可重试。
8. SMTP 发送阶段发生连接异常时状态记为 `unknown`，禁止自动重试。用户必须先到 Concur 核对，再选择“已送达”或“未送达”。

## 3. 数据与状态

ledger schema v6 新增：

- `concur_send_sessions`：批次、固定发件地址、固定收件地址、试发收据、试发状态和用户确认的租户行为；
- `concur_send_items`：每张收据的幂等键、结构化附件名、SHA-256、尝试次数、发送状态、脱敏错误类别、Message-ID 和发送时间。

项目状态为 `pending → sending → sent | failed | unknown`。应用异常退出后，遗留的 `sending` 自动恢复为 `unknown`，不会按失败项直接重试。已存在计划的发件地址、收件地址、试发项和完整附件集合不可静默修改。

这些状态属于用户业务数据，会进入未加密备份并可跨电脑恢复；邮箱授权码不进入数据库、日志、备份或 portable 包。

## 4. 附件规则

- 支持 PDF、PNG、JPG/JPEG、TIF/TIFF；XML/OFD 必须先生成受支持的可视文件。
- 单件必须大于 0 字节且不超过 15 MiB；单封附件合计不超过 20 MiB。
- 附件名使用 `{日期}_{类型}_{金额}_{票号后六位}_{内容哈希前八位}.{扩展名}`，避免 Available Receipts 全局库中的批次内序号冲突。
- 建立计划和发送前都会重新计算 SHA-256；审核后文件改变、同大小替换或读取中变化均阻止发送。
- 幂等键由版本域、发件地址、收件地址、发票号码和附件内容哈希计算；同组幂等键生成稳定 Message-ID。

SAP Concur 2026 年帮助文档说明 Available Receipts 支持通过已验证邮箱发送收据，并支持 PDF/PNG/JPEG 等格式：[Available Receipts](https://help.sap.com/docs/concur-expense/concur-expense-standard-edition-tools-guides/understand-available-receipts-line-item-receipt-image-attachment-feature?locale=en-US&state=PRODUCTION&version=2026_03)、[文件格式与限制](https://help.sap.com/docs/CONCUR_EXPENSE/f45ee181c99e4d93afbab48a5b75ea50/53f85f8c54554ff392824191318fdb26.html?locale=en-GB)。2026-02 起费用条目上传单件上限提高到 15 MB；本产品据此采用 15 MiB 单件上限，并额外采用保守的 20 MiB 单封上限：[SAP 发布说明](https://help.sap.com/docs/concur-expense/concur-expense-standard-edition-release-notes/increased-per-file-size-limit-for-expense-entry-uploads)。租户配置可能更严格，最终以用户所在公司的 Concur 页面和管理员要求为准。

## 5. SMTP 与凭据安全

| 邮箱域名 | 主机与端口 | TLS | 发布状态 |
|---|---|---|---|
| QQ / vip.qq.com / Foxmail | `smtp.qq.com:465` | 连接开始即 TLS | Alpha 代码支持；真实发送未验证 |
| 163 | `smtp.163.com:465` | 连接开始即 TLS | Beta 前需受控账号验证 |
| 126 | `smtp.126.com:465` | 连接开始即 TLS | 实验性 |
| Gmail | `smtp.gmail.com:465` | 连接开始即 TLS | 实验性 |
| Outlook / Hotmail | `smtp.office365.com:587` | 强制 STARTTLS | 实验性 |

SMTP 总操作超时为 30 秒，不允许明文或 opportunistic TLS。发件地址必须与当前会话邮箱匹配；授权码仅在调用期间传给本地 SMTP 库，不记录完整服务端响应。应用不后台连接、不自动重试、不保存授权码。

## 6. 已完成自动验证

- schema v0→v6 连续迁移、未来 schema 零修改拒绝；
- 试发未确认时批量门禁；试发行为确认后才允许批量；
- 1–5 张批量项目原子预留；并发或状态变化时整组不发送；
- 成功项再次请求返回 `AlreadySent`，尝试次数不增加；
- 应用中断后 `sending → unknown`，人工确认未送达后才可重试；
- 计划配置和审核附件集合不可变；排除项及草稿批次被拒绝；
- 支持格式、文件大小、邮件附件数、结构化文件名、哈希和稳定 Message-ID；
- 模拟 SMTP 成功返回稳定 Message-ID；模拟传输中断归类为 `OutcomeUnknown`；
- 内部 Alpha 构建脚本拒绝 `INVOICE_ENABLE_CONCUR_SEND=1`，`version.json` 和 portable 验证证据必须显示关闭；
- UI 提供建立计划、一次试发、租户行为确认、剩余发送、逐项状态和未知结果人工解锁；Svelte 检查 0 错误/0 警告。

全部自动测试使用 `example.test`、`concur.example` 和合成文件；未读取 `.env.local`，未连接真实 SMTP 或 Concur。

## 7. 仍需真实验证与发布门禁

公开 Beta 前由产品负责人提供测试租户、该租户要求的收件地址、已验证发件邮箱和每次外发批准，并按以下顺序执行：

1. 记录候选包哈希、构建开关、测试批次脱敏清单和批准人；
2. 只发送一张合法、非敏感或经授权的代表性收据；
3. 在 Concur 人工确认收据库/ExpenseIt 行为；
4. 发送受控剩余集合，制造一次发送前失败和一次连接中断，验证 failed/unknown 分流；
5. 重复点击和重启后确认成功项不重发；
6. 核对 Concur 数量、附件名、文件内容哈希与本地状态；
7. 报告仅保留掩码邮箱、数量、状态、哈希、候选包版本和批准记录，不包含授权码、完整票号或原始票据。

真实验证未完成前保持 `CONCUR-001` 开放，内部 Alpha 不得用于真实外发。
