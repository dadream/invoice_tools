# 邮件处理台账设计

## 信息架构

批次详情新增“邮件台账”视图。左侧显示邮件处理结果，右侧显示选中邮件及附件材料包。邮件是来源容器，附件是处理对象，发票和待处理材料是附件的后续产物。

## 状态模型

### 邮件状态

- `imported`：至少一个附件成功成为发票。
- `needs_attachment_review`：只有待挂载材料或需要审核的附件。
- `manual_download`：正文提示通过链接交付，软件未自动访问。
- `needs_confirmation`：疑似发票通知，但没有可取得附件或明确下载链接。
- `not_invoice`：没有发票、相关材料或发票语义。
- `failed`：获取或 MIME 解析失败。
- `processing`：导入尚未形成终态，仅用于可恢复任务。
- `ignored`：用户明确确认无需处理。

### 附件状态

- `invoice`、`supporting`、`duplicate`、`not_invoice`、`unsupported`、`failed`。

附件同时保存 `role_hint`（`invoice`、`itinerary`、`detail`、`supporting`、`unknown`）和稳定 `reason`，避免把业务类别与技术错误混成一个字段。

## 数据模型

### `email_import_messages`

- 本地 ID、批次 ID、流水线 ID、账号 ID（可空）、邮箱文件夹、UID。
- Message-ID 哈希、发件人、主题、服务器收件时间。
- 邮件状态、动作状态、附件数量、错误类别、创建/更新时间。
- 唯一约束：同一流水线内 folder + UID 唯一。

### `email_import_attachments`

- 本地 ID、邮件 ID、内容哈希、原附件名、MIME、字节数。
- ZIP 展开时记录父附件名和逻辑序号。
- 状态、角色提示、判定原因。
- 关联 `reported_invoice_id` 或 `pending_document_id`。
- 补充导入记录 `manual_import` 标记。

## 流水线

1. IMAP 搜索后先建立邮件采集结果；成功解析时记录脱敏后的必要头字段。
2. 每个逻辑附件先写入检查点台账，之后执行分类、大小校验、内容去重和暂存。
3. 解析阶段回写发票/材料结果；存储阶段把检查点 ID 映射到数据库产物。
4. 邮件终态根据附件终态和正文布尔提示归并。
5. 本地补充导入可携带 `target_email_id`，新文件仍走正常解析，但附件台账挂回目标邮件。

## API

- `list_email_import_ledger(batch_id)`：返回邮件摘要、附件和汇总。
- `resolve_email_import_message(message_id, action)`：标记已确认或重新打开。
- `start_pipeline` 本地来源增加可选 `target_email_id`。

所有写操作校验批次仍处于可编辑状态；所有 ID 必须属于同一批次。

## UI 设计

- 批次主导航新增“邮件台账”，不放在费用核对三栏中。
- 顶部汇总：全部、已导入、待确认、需下载、失败。
- 左侧列表显示状态、主题、发件人、收件日期和附件统计。
- 右侧按同一邮件展示附件，显示角色、结果、是否关联费用。
- “下载后导入”调用现有本地文件选择器并绑定邮件 ID。
- 历史批次没有记录时显示解释性空状态。

## 测试策略

- v11 到新版本迁移、回滚原子性和外键完整性。
- 69封邮件终态守恒、115份附件终态守恒的合成测试。
- 同邮件发票和行程单的关联测试。
- 链接/通知/非发票/失败邮件状态测试。
- Svelte 类型检查、组件状态与操作测试、Windows免安装构建。
