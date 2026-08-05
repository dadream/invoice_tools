# 发票解析能力验证报告

## 通过率

| 格式 | 样本数 | 通过 | 通过率 |
|---|---|---|---|
| image | 6 | 0 | 0.0% |
| ofd | 8 | 0 | 0.0% |
| pdf-flight | 2 | 0 | 0.0% |
| pdf-vat | 41 | 0 | 0.0% |
| xml | 7 | 0 | 0.0% |

合计 0/64（0.0%）

## 字段不匹配

| 样本 | 字段 | 期望 | 实际 |
|---|---|---|---|
| samples/03-unknown-6201d368.xml | invoice_number |  | 26112000002208097411 |
| samples/03-unknown-6201d368.xml | issue_date |  | 2026-06-01 |
| samples/03-unknown-6201d368.xml | total_amount |  | 47.40 |
| samples/04-unknown-6554429d.pdf | invoice_number |  | 26312000003445962271 |
| samples/04-unknown-6554429d.pdf | issue_date |  | 2026-06-03 |
| samples/04-unknown-6554429d.pdf | total_amount |  | 1500.00 |
| samples/05-unknown-b4511bc3.pdf | invoice_number |  | 26112000002267104336 |
| samples/05-unknown-b4511bc3.pdf | issue_date |  | 2026-06-04 |
| samples/05-unknown-b4511bc3.pdf | total_amount |  | 65.40 |
| samples/09-meituan-2f9595e6.pdf | invoice_number |  | 26317000001917172381 |
| samples/09-meituan-2f9595e6.pdf | issue_date |  | 2026-06-08 |
| samples/09-meituan-2f9595e6.pdf | total_amount |  | 0.30 |
| samples/10-meituan-a3662f79.xml | invoice_number |  | 26317000001917172381 |
| samples/10-meituan-a3662f79.xml | issue_date |  | 2026-06-08 |
| samples/10-meituan-a3662f79.xml | total_amount |  | 0.30 |
| samples/27-unknown-08cfe721.pdf | invoice_number |  | 26117000000826606023 |
| samples/27-unknown-08cfe721.pdf | issue_date |  | 2026-06-08 |
| samples/27-unknown-08cfe721.pdf | total_amount |  | 15.00 |
| samples/29-unknown-a926b89d.xml | invoice_number |  | 26117000000826606023 |
| samples/29-unknown-a926b89d.xml | issue_date |  | 2026-06-08 |
| samples/29-unknown-a926b89d.xml | total_amount |  | 15.00 |
| samples/31-unknown-4812770d.pdf | invoice_number |  | 26117000000806259258 |
| samples/31-unknown-4812770d.pdf | issue_date |  | 2026-06-08 |
| samples/31-unknown-4812770d.pdf | total_amount |  | 68.82 |
| samples/32-unknown-9ac1bdc2.xml | invoice_number |  | 26117000000806259258 |
| samples/32-unknown-9ac1bdc2.xml | issue_date |  | 2026-06-08 |
| samples/32-unknown-9ac1bdc2.xml | total_amount |  | 68.82 |
| samples/36-unknown-932166e2.pdf | invoice_number |  | 26112000002354032261 |
| samples/36-unknown-932166e2.pdf | issue_date |  | 2026-06-10 |
| samples/36-unknown-932166e2.pdf | total_amount |  | 692.00 |
| samples/37-unknown-41550a0b.pdf | invoice_number |  | 26112000002354189881 |
| samples/37-unknown-41550a0b.pdf | issue_date |  | 2026-06-10 |
| samples/37-unknown-41550a0b.pdf | total_amount |  | 692.00 |
| samples/38-meituan-d495969a.pdf | invoice_number |  | 26317000002117472528 |
| samples/38-meituan-d495969a.pdf | issue_date |  | 2026-06-17 |
| samples/38-meituan-d495969a.pdf | total_amount |  | 3.20 |
| samples/39-meituan-26e4b7aa.xml | invoice_number |  | 26317000002117472528 |
| samples/39-meituan-26e4b7aa.xml | issue_date |  | 2026-06-17 |
| samples/39-meituan-26e4b7aa.xml | total_amount |  | 3.20 |
| samples/41-unknown-5026a35f.pdf | invoice_number |  | 26142000000876917341 |
| samples/41-unknown-5026a35f.pdf | issue_date |  | 2026-06-18 |
| samples/41-unknown-5026a35f.pdf | total_amount |  | 1850.58 |
| samples/43-unknown-7f0f13a8.pdf | invoice_number |  | 26132000001912746961 |
| samples/43-unknown-7f0f13a8.pdf | issue_date |  | 2026-06-18 |
| samples/43-unknown-7f0f13a8.pdf | total_amount |  | 80.00 |
| samples/44-unknown-72fd9aee.xml | invoice_number |  | 26132000001912746961 |
| samples/44-unknown-72fd9aee.xml | issue_date |  | 2026-06-18 |
| samples/44-unknown-72fd9aee.xml | total_amount |  | 80.00 |
| samples/46-unknown-6e89a590.pdf | invoice_number |  | 26132000001954318426 |
| samples/46-unknown-6e89a590.pdf | issue_date |  | 2026-06-22 |
| samples/46-unknown-6e89a590.pdf | total_amount |  | 74.00 |
| samples/47-unknown-de992fb0.xml | invoice_number |  | 26132000001954318426 |
| samples/47-unknown-de992fb0.xml | issue_date |  | 2026-06-22 |
| samples/47-unknown-de992fb0.xml | total_amount |  | 74.00 |
| samples/64-unknown-e8d72419.pdf | invoice_number |  | 26112000002680730506 |
| samples/64-unknown-e8d72419.pdf | issue_date |  | 2026-06-30 |
| samples/64-unknown-e8d72419.pdf | total_amount |  | 490.00 |

## 解析失败

| 样本 | 错误 |
|---|---|
| samples/01-unknown-1cb9ce98.pdf | PDF 文本提取失败: fixtures/samples/01-unknown-1cb9ce98.pdf 不是有效的 PDF 格式: 文本层提取失败: PDF error: Invalid file trailer |
| samples/02-unknown-f6f7c6b1.ofd | fixtures/samples/02-unknown-f6f7c6b1.ofd 不是有效的 OFD 格式: 不是有效的 ZIP 容器: invalid Zip archive: Invalid CDFH offset in EOCD |
| samples/06-unknown-fbf5dc58.pdf | 解析器崩溃: assertion failed: name == "Identity-H" |
| samples/07-unknown-f1ad9ccb.pdf | 在 fixtures/samples/07-unknown-f1ad9ccb.pdf 中找不到必需字段 invoice_number |
| samples/08-meituan-42f0da2f.pdf | 在 fixtures/samples/08-meituan-42f0da2f.pdf 中找不到必需字段 total_amount |
| samples/11-meituan-34ee412d.ofd | 在 fixtures/samples/11-meituan-34ee412d.ofd 中找不到必需字段 invoice_number |
| samples/12-didi-cc75b327.pdf | 在 fixtures/samples/12-didi-cc75b327.pdf 中找不到必需字段 total_amount |
| samples/13-didi-dc2d7047.pdf | 在 fixtures/samples/13-didi-dc2d7047.pdf 中找不到必需字段 total_amount |
| samples/14-didi-357b5b11.pdf | 在 fixtures/samples/14-didi-357b5b11.pdf 中找不到必需字段 total_amount |
| samples/15-didi-6079fc74.pdf | 在 fixtures/samples/15-didi-6079fc74.pdf 中找不到必需字段 total_amount |
| samples/16-didi-85a0f5d3.pdf | 在 fixtures/samples/16-didi-85a0f5d3.pdf 中找不到必需字段 total_amount |
| samples/17-didi-cb5d20a0.pdf | 在 fixtures/samples/17-didi-cb5d20a0.pdf 中找不到必需字段 total_amount |
| samples/18-didi-968f2b9b.pdf | 在 fixtures/samples/18-didi-968f2b9b.pdf 中找不到必需字段 total_amount |
| samples/19-didi-b7415fa5.pdf | 在 fixtures/samples/19-didi-b7415fa5.pdf 中找不到必需字段 total_amount |
| samples/20-didi-87338868.pdf | 在 fixtures/samples/20-didi-87338868.pdf 中找不到必需字段 total_amount |
| samples/21-didi-173842a5.pdf | 在 fixtures/samples/21-didi-173842a5.pdf 中找不到必需字段 total_amount |
| samples/22-didi-0cd6a9c3.pdf | 在 fixtures/samples/22-didi-0cd6a9c3.pdf 中找不到必需字段 total_amount |
| samples/23-didi-226cbac6.pdf | 在 fixtures/samples/23-didi-226cbac6.pdf 中找不到必需字段 total_amount |
| samples/24-didi-4f308da8.pdf | 在 fixtures/samples/24-didi-4f308da8.pdf 中找不到必需字段 total_amount |
| samples/25-didi-66e3e108.pdf | 在 fixtures/samples/25-didi-66e3e108.pdf 中找不到必需字段 total_amount |
| samples/26-unknown-d3006c0b.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/28-unknown-36c9093e.ofd | 在 fixtures/samples/28-unknown-36c9093e.ofd 中找不到必需字段 invoice_number |
| samples/30-unknown-5be79379.pdf | 在 fixtures/samples/30-unknown-5be79379.pdf 中找不到必需字段 total_amount |
| samples/33-unknown-1f1e61a4.ofd | 在 fixtures/samples/33-unknown-1f1e61a4.ofd 中找不到必需字段 invoice_number |
| samples/34-unknown-993123fa.pdf | 在 fixtures/samples/34-unknown-993123fa.pdf 中找不到必需字段 total_amount |
| samples/35-meituan-21a391a6.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/40-meituan-12f8065e.ofd | 在 fixtures/samples/40-meituan-12f8065e.ofd 中找不到必需字段 invoice_number |
| samples/42-meituan-b6c3341f.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/45-unknown-3ed9ed77.ofd | 在 fixtures/samples/45-unknown-3ed9ed77.ofd 中找不到必需字段 invoice_number |
| samples/48-unknown-cb25d50d.ofd | 在 fixtures/samples/48-unknown-cb25d50d.ofd 中找不到必需字段 invoice_number |
| samples/49-didi-2745a005.pdf | 在 fixtures/samples/49-didi-2745a005.pdf 中找不到必需字段 total_amount |
| samples/50-didi-520ce335.pdf | 在 fixtures/samples/50-didi-520ce335.pdf 中找不到必需字段 total_amount |
| samples/51-didi-2ed3ee1a.pdf | 在 fixtures/samples/51-didi-2ed3ee1a.pdf 中找不到必需字段 total_amount |
| samples/52-didi-89213171.pdf | 在 fixtures/samples/52-didi-89213171.pdf 中找不到必需字段 total_amount |
| samples/53-didi-705a5872.pdf | 在 fixtures/samples/53-didi-705a5872.pdf 中找不到必需字段 invoice_number |
| samples/54-didi-adc92bb1.pdf | 在 fixtures/samples/54-didi-adc92bb1.pdf 中找不到必需字段 invoice_number |
| samples/55-didi-4dc63148.pdf | 在 fixtures/samples/55-didi-4dc63148.pdf 中找不到必需字段 total_amount |
| samples/56-didi-22a68dc7.pdf | 在 fixtures/samples/56-didi-22a68dc7.pdf 中找不到必需字段 total_amount |
| samples/57-didi-4e7f177b.pdf | 在 fixtures/samples/57-didi-4e7f177b.pdf 中找不到必需字段 total_amount |
| samples/58-didi-25f40401.pdf | 在 fixtures/samples/58-didi-25f40401.pdf 中找不到必需字段 total_amount |
| samples/59-didi-c6041246.pdf | 在 fixtures/samples/59-didi-c6041246.pdf 中找不到必需字段 total_amount |
| samples/60-unknown-ccff78f5.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/61-unknown-c27071a2.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/62-unknown-70a24c65.jpg | 图片 OCR 需要 Python sidecar，暂未集成到 verify-all |
| samples/63-unknown-19d988e1.ofd | 在 fixtures/samples/63-unknown-19d988e1.ofd 中找不到必需字段 invoice_number |

---

## 结论（手工填写）

### 纯 Rust 是否可行

- [ ] 可行 —— 全部格式达标，按纯 Rust 推进
- [ ] 部分兜底 —— 以下能力需 Python sidecar：______，预计包体增量 ______ MB
- [ ] 不可行 —— 需重新评估 Tauri vs Electron

### 覆盖缺口

- OCR 置信度是否可用于人工复核路由：______
- 本地验签是否成立：______
- 作废票负例是否已验证：______
- 无内嵌 XML 的 OFD 占比：______

### 安装包体积实测

- ONNX 模型总体积：______ MB
- release 构建后的可执行文件：______ MB
