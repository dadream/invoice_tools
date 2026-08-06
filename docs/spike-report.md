# 发票解析能力验证报告

## 通过率

| 格式 | 样本数 | 通过 | 通过率 |
|---|---|---|---|
| image | 2 | 2 | 100.0% |
| ofd | 8 | 4 | 50.0% |
| pdf-flight | 2 | 0 | 0.0% |
| pdf-vat | 41 | 12 | 29.3% |
| xml-vat | 7 | 7 | 100.0% |

合计 25/60（41.7%）

另有 4 个样本经人工确认不是发票，已排除在统计之外。

## 字段不匹配

| 样本 | 字段 | 期望 | 实际 |
|---|---|---|---|
| samples/04-unknown-6554429d.pdf | tax_amount | 84.91 | <缺失> |
| samples/04-unknown-6554429d.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/04-unknown-6554429d.pdf | seller_name | 上海中核科创园发展有限公司 | <缺失> |
| samples/05-unknown-b4511bc3.pdf | tax_amount | 0.65 | <缺失> |
| samples/05-unknown-b4511bc3.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | 地址:-;    电话:15313153611 |
| samples/05-unknown-b4511bc3.pdf | seller_name | 河北融元商贸有限公司北京第三分公司 | <缺失> |
| samples/09-meituan-2f9595e6.pdf | tax_amount | 0.02 | <缺失> |
| samples/09-meituan-2f9595e6.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/09-meituan-2f9595e6.pdf | seller_name | 上海三快智送科技有限公司 | <缺失> |
| samples/27-unknown-08cfe721.pdf | tax_amount | 0.85 | <缺失> |
| samples/27-unknown-08cfe721.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/27-unknown-08cfe721.pdf | seller_name | 北京顺丰速运有限公司 | <缺失> |
| samples/31-unknown-4812770d.pdf | tax_amount | 3.90 | <缺失> |
| samples/31-unknown-4812770d.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/31-unknown-4812770d.pdf | seller_name | 深圳市顺丰同城物流有限公司北京分公司 | <缺失> |
| samples/36-unknown-932166e2.pdf | tax_amount | 79.61 | <缺失> |
| samples/36-unknown-932166e2.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/36-unknown-932166e2.pdf | seller_name | 北京赛特奥特莱斯商贸有限公司 | <缺失> |
| samples/37-unknown-41550a0b.pdf | tax_amount | 79.61 | <缺失> |
| samples/37-unknown-41550a0b.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/37-unknown-41550a0b.pdf | seller_name | 北京赛特奥特莱斯商贸有限公司 | <缺失> |
| samples/38-meituan-d495969a.pdf | tax_amount | 0.18 | <缺失> |
| samples/38-meituan-d495969a.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/38-meituan-d495969a.pdf | seller_name | 上海三快智送科技有限公司 | <缺失> |
| samples/41-unknown-5026a35f.pdf | tax_amount | 104.75 | <缺失> |
| samples/41-unknown-5026a35f.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/41-unknown-5026a35f.pdf | seller_name | 山西晋泽昌文旅酒店有限公司 | <缺失> |
| samples/43-unknown-7f0f13a8.pdf | tax_amount | 9.20 | <缺失> |
| samples/43-unknown-7f0f13a8.pdf | buyer_name | 赛比亚医疗诊断器械(上海)有限公司 | <缺失> |
| samples/43-unknown-7f0f13a8.pdf | seller_name | 北京京铁列车服务有限公司石家庄分公司 | <缺失> |
| samples/46-unknown-6e89a590.pdf | tax_amount | 8.52 | <缺失> |
| samples/46-unknown-6e89a590.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/46-unknown-6e89a590.pdf | seller_name | 北京京铁列车服务有限公司石家庄分公司 | <缺失> |

## 解析失败

| 样本 | 错误 |
|---|---|
| samples/01-unknown-1cb9ce98.pdf | PDF 文本提取失败: fixtures/samples/01-unknown-1cb9ce98.pdf 不是有效的 PDF 格式: 文本层提取失败: PDF error: Invalid file trailer |
| samples/02-unknown-f6f7c6b1.ofd | fixtures/samples/02-unknown-f6f7c6b1.ofd 不是有效的 OFD 格式: 不是有效的 ZIP 容器: invalid Zip archive: Invalid CDFH offset in EOCD |
| samples/06-unknown-fbf5dc58.pdf | 解析器崩溃: assertion failed: name == "Identity-H" |
| samples/07-unknown-f1ad9ccb.pdf | 在 fixtures/samples/07-unknown-f1ad9ccb.pdf 中找不到必需字段 invoice_number |
| samples/08-meituan-42f0da2f.pdf | 在 fixtures/samples/08-meituan-42f0da2f.pdf 中找不到必需字段 total_amount |
| samples/12-didi-cc75b327.pdf | 在 fixtures/samples/12-didi-cc75b327.pdf 中找不到必需字段 total_amount |
| samples/14-didi-357b5b11.pdf | 在 fixtures/samples/14-didi-357b5b11.pdf 中找不到必需字段 total_amount |
| samples/16-didi-85a0f5d3.pdf | 在 fixtures/samples/16-didi-85a0f5d3.pdf 中找不到必需字段 total_amount |
| samples/18-didi-968f2b9b.pdf | 在 fixtures/samples/18-didi-968f2b9b.pdf 中找不到必需字段 total_amount |
| samples/20-didi-87338868.pdf | 在 fixtures/samples/20-didi-87338868.pdf 中找不到必需字段 total_amount |
| samples/22-didi-0cd6a9c3.pdf | 在 fixtures/samples/22-didi-0cd6a9c3.pdf 中找不到必需字段 total_amount |
| samples/24-didi-4f308da8.pdf | 在 fixtures/samples/24-didi-4f308da8.pdf 中找不到必需字段 total_amount |
| samples/30-unknown-5be79379.pdf | 在 fixtures/samples/30-unknown-5be79379.pdf 中找不到必需字段 total_amount |
| samples/34-unknown-993123fa.pdf | 在 fixtures/samples/34-unknown-993123fa.pdf 中找不到必需字段 total_amount |
| samples/45-unknown-3ed9ed77.ofd | 在 fixtures/samples/45-unknown-3ed9ed77.ofd 中找不到必需字段 invoice_number |
| samples/48-unknown-cb25d50d.ofd | 在 fixtures/samples/48-unknown-cb25d50d.ofd 中找不到必需字段 invoice_number |
| samples/49-didi-2745a005.pdf | 在 fixtures/samples/49-didi-2745a005.pdf 中找不到必需字段 total_amount |
| samples/51-didi-2ed3ee1a.pdf | 在 fixtures/samples/51-didi-2ed3ee1a.pdf 中找不到必需字段 total_amount |
| samples/53-didi-705a5872.pdf | 在 fixtures/samples/53-didi-705a5872.pdf 中找不到必需字段 invoice_number |
| samples/54-didi-adc92bb1.pdf | 在 fixtures/samples/54-didi-adc92bb1.pdf 中找不到必需字段 invoice_number |
| samples/55-didi-4dc63148.pdf | 在 fixtures/samples/55-didi-4dc63148.pdf 中找不到必需字段 total_amount |
| samples/56-didi-22a68dc7.pdf | 在 fixtures/samples/56-didi-22a68dc7.pdf 中找不到必需字段 total_amount |
| samples/58-didi-25f40401.pdf | 在 fixtures/samples/58-didi-25f40401.pdf 中找不到必需字段 total_amount |
| samples/63-unknown-19d988e1.ofd | 在 fixtures/samples/63-unknown-19d988e1.ofd 中找不到必需字段 invoice_number |

## 已排除（非发票）

| 样本 | 原因 |
|---|---|
| samples/26-unknown-d3006c0b.jpg | 176x64 近空白 PNG（非白像素 0.4%），OCR 检测不到任何文本，邮件装饰图元 |
| samples/60-unknown-ccff78f5.jpg | 1200x240 邮件横幅图，仅含「数字化税票服务平台」标题文字 |
| samples/61-unknown-c27071a2.jpg | 350x43「点击查看发票」按钮图 |
| samples/62-unknown-70a24c65.jpg | 665x108「恭喜您获得开票奖励」营销横幅 |

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
