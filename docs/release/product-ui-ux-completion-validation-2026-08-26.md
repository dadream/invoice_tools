# 产品功能与 UI/UX 收口验证记录（2026-08-26）

## 结论

本轮已完成已确认的本地产品功能和 UI/UX 缺口实现，并生成 Windows x64 免安装内部 Alpha 候选。统一自动编译与回归通过；按产品负责人要求，未启动程序、未使用 GUI 自动操作，真实批次 4 的可见界面验收由产品负责人执行。

该结论不等于公开发布 Go：Git 历史隐私治理、真实 Concur 租户适配、代码签名、正式更新地址和完整 Windows 人工矩阵仍由发布缺陷清单跟踪。

## 候选产物

- ZIP：`artifacts/InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA.zip`
- 主程序：解压后的 `InvoiceAssistant.exe`
- ZIP 字节数：54,699,375
- SHA-256：`2527F7DDB982A8F7ABB457907B1F564D199051904A0F4BCB78BA397D7D92414C`
- 签名状态：`NotSigned`（仅供内部 Alpha 验证）
- Concur 真实发送/写入：关闭

## 本轮产品边界

1. 新建批次只填写批次名称，创建时间由系统记录；来源和日期范围仅在批次内导入时出现。
2. 批次一级视图严格为“费用清单”和“归组”。
3. 本地导入面向发票和配套材料文件/文件夹，不把 EML 暴露为用户入口。
4. 主发票、行程单、消费明细、其他材料和重复副本挂到同一稳定费用项；材料不单独计费。
5. 疑似重复默认不计入总额，只有用户明确判断“不是重复”后才恢复计入。
6. 差旅行程必须有铁路、航空或行程单锚点；重叠行程不会重复挂载同一费用；移动后空组自动清除。
7. 单条核对使用软件自有稳定费用字段；票据信息和费用信息分别保存，均不依赖 Concur 字段。
8. 完成审核后冻结版本化快照，再进入批次外的交付选择；Excel 与 Concur 是两个独立入口。
9. Excel 使用稳定费用快照并由用户选择保存位置；Concur 本地映射、缺口预检和可恢复状态机已完成，真实适配器保持能力门禁关闭。
10. 原件预览使用 Windows 原生 PDF 渲染；原路径失效时可受控重新关联并复制到稳定数据目录。

## 自动验证结果

- `scripts/verify-windows.ps1`：通过。
- Rust：`fmt`、严格 Clippy、全工作区所有目标 `check/test` 通过，0 失败。
- `invoice-store`：72/72 通过，覆盖 v11 迁移、迁移失败回滚、稳定费用字段独立、重复、材料、归组、撤销和原件重关联。
- `invoice-grouping` 合成场景：22/22 通过，包含重叠行程费用不重复挂载。
- OCR：合成图片、扫描 PDF 和独立 worker 进程金样均通过。
- 前端：Svelte 0 错误/0 警告；Vitest 5 个文件、24/24 通过；Vite 生产构建通过。
- 安全与发布门禁：秘密扫描、私有夹具扫描、许可证、更新地址策略、Concur Alpha 关闭门禁、DLL 搜索路径加固均通过。
- 免安装包结构验证：33 个 ZIP 条目、31 个 manifest 条目、32 个校验和条目；禁止文件 0；中文空格临时路径解压校验通过；未启动 GUI。
- 磁盘后检：`target/debug` 8.37 GiB、`target/release` 7.32 GiB、`target` 15.69 GiB、`artifacts` 0.67 GiB、剩余空间 72.85 GiB，全部低于硬限制。

## 产品负责人本轮人工验收重点

1. 解压 ZIP，直接运行 `InvoiceAssistant.exe`，确认无需安装和管理员权限。
2. 打开真实批次 4（test），在“费用清单”核对原件不闪烁；旧路径失效时使用“重新关联原件”。
3. 核对重复记录默认未计入总额，张数与未计入金额提示一致；把一条明确标记为“不是重复”后再核对总额变化。
4. 在“归组”执行“重新分析归组”，核对铁路/航空路线、差旅锚点、市内消费、待确认项和空组清理。
5. 核对单条费用的“费用信息/票据信息/问题与依据”，确认页面中没有 Concur 企业字段。
6. 阻断项清零并确认归组后点击“完成审核”，确认进入独立交付选择，而不是在批次页出现输出/上传标签。
7. 选择“导出 Excel”，核对保存位置、实际金额、重复未计入、材料挂载和分组；“上传到 Concur”应明确显示真实适配器尚未启用，而不是伪造成功。

## 证据

- `artifacts/portable-verification-product-ui-ux-completion-2026-08-26.validation.json`
- `artifacts/build-storage-preflight.validation.json`
- `artifacts/build-storage-postflight.validation.json`
- `artifacts/build-storage-portable-preflight.validation.json`
- `artifacts/build-storage-portable-postflight.validation.json`
- `docs/release/open-defects.md`
