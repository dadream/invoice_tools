# 独立邮件收集工作台实施验证

**验证日期**：2026-08-28
**结论**：代码、自动化回归和 Windows 免安装内部测试包已完成；最终 UI 人工验收由用户执行。

## 已实施

- schema v13、旧邮件台账历史迁移、独立收集任务/邮件/附件/批次来源快照。
- 邮件收集只做 IMAP 只读搜索、来源分类和附件持久化，不调用发票解析、业务去重或归组。
- 同一邮件的发票候选、行程单、明细和其他材料保留在同一材料包。
- 链接交付、需确认、附件失败支持审核和本地文件补充；本地补充不接受 EML。
- 批次新建仅保留名称和系统创建时间；批次内从收集任务快照或本地发票文件开始解析。
- 批次内移除完整邮件台账，只保留来源摘要；支持跨批次使用提示。
- 修复旧 `email-ledger` 检查点读取和解析来源路径规范化问题。
- 收集材料采用可跨电脑恢复的相对定位符；兼容旧绝对路径安全重定位。
- 备份包含 `collection-files`；批次读取前再次校验路径位于受控材料库。
- 单任务上限 500 MiB，持久材料库上限 5 GiB；设置中的受控清理会单列邮件材料库。
- 收集任务详情改为全宽邮件表格，按“需要用户处理 / 待审核 / 已审核”三个互斥分组呈现，每页 25 封。
- 表格字段固定为收件时间、主题、发件人、附件数、处理结果、状态、操作；收件时间只显示 `YYYY-MM-DD`，默认倒序，缺失日期置后。
- 点击邮件进入独立审核页，显示安全纯文本正文、发件人、日期、主题、下载链接摘要和同源附件。
- 正文与链接按需通过 `BODY.PEEK` 获取并复核 FLAGS，不落库；完整下载 URL 不作为可执行参数返回前端、不写日志。
- 下载链接只显示操作标签与域名，用户确认后由后端重新读取并校验 HTTPS 链接，再交给 Windows 系统浏览器。
- PDF、OFD、XML 和图片附件使用按需弹层预览；预览失败保留系统程序打开入口。

## 自动化验证

- `scripts/verify-windows.ps1`：通过。
  - Rust `fmt`、workspace `clippy -D warnings`、workspace `check`、全工作区测试通过。
  - 离线图片 OCR、扫描 PDF OCR、独立 OCR worker 进程金样通过。
  - Svelte 检查 0 错误/0 警告；Vitest 34/34 通过；Vite 生产构建通过。
  - 密钥扫描、私有样本扫描、许可证、更新地址、Concur Alpha 构建门禁通过。
- 定向最终回归：`invoice-assistant` 120/120 通过（1 个显式私有样本测试忽略）；`invoice-store` 78/78、`invoice-collect` 70/70 通过。
- 新增邮件 UI 视图模型测试 4/4 通过：日期仅显示日期部分、倒序且缺失置后、三分组互斥、处理结果独立于状态分组。
- 新增邮件正文/链接安全测试通过：HTML 纯文本化、正文截断、HTTP/带凭据链接拒绝、退订链接过滤、链接摘要不泄露完整 URL。
- 最终便携包静态验证：ZIP/sidecar/manifest/checksum 一致，禁带文件 0，DLL 搜索策略为 System32Only。
- 最终磁盘状态：debug 9.46 GiB、release 7.51 GiB、target 16.96 GiB、artifacts 1.49 GiB、可用 61.51 GiB，均低于 `AGENTS.md` 上限。

## 最终测试包

- 文件：`artifacts/InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA-EMAIL-LEDGER-REVIEW-20260828.zip`
- 大小：55,503,409 bytes
- SHA-256：`A100557A718C2BC5FB0AD0BA53FECA36EF3880F02C42287FF50E29F346D81429`
- 包属性：Windows x64、免安装、未签名内部 Alpha、真实 Concur 发送关闭。
- 静态验证证据：`artifacts/email-ledger-review-portable.validation.json`

## 人工验收边界

遵循用户要求，自动化阶段未启动最终程序、未使用 computer use、未执行 UI 点击。用户应使用最终 ZIP 解压后的 `InvoiceAssistant.exe` 验证页面流程与真实 QQ 邮箱只读收集。
