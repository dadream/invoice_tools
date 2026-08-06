# 发票解析能力验证报告

## 通过率

| 格式 | 样本数 | 通过 | 通过率 |
|---|---|---|---|
| image | 2 | 2 | 100.0% |
| ofd | 8 | 6 | 75.0% |
| pdf-vat | 27 | 23 | 85.2% |
| xml-vat | 7 | 7 | 100.0% |

合计 38/44（86.4%）

另有 20 个样本经人工确认不是发票，已排除在统计之外。

## 字段不匹配

| 样本 | 字段 | 期望 | 实际 |
|---|---|---|---|
| samples/27-unknown-08cfe721.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/31-unknown-4812770d.pdf | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/48-unknown-cb25d50d.ofd | tax_amount | 8.52 | <缺失> |
| samples/48-unknown-cb25d50d.ofd | buyer_name | 赛比亚医疗诊断器械（上海）有限公司 | <缺失> |
| samples/48-unknown-cb25d50d.ofd | seller_name | 北京京铁列车服务有限公司石家庄分公司 | <缺失> |

## 解析失败

| 样本 | 错误 |
|---|---|
| samples/01-unknown-1cb9ce98.pdf | PDF 文本提取失败: fixtures/samples/01-unknown-1cb9ce98.pdf 不是有效的 PDF 格式: 文本层提取失败: PDF error: Invalid file trailer |
| samples/02-unknown-f6f7c6b1.ofd | fixtures/samples/02-unknown-f6f7c6b1.ofd 不是有效的 OFD 格式: 不是有效的 ZIP 容器: invalid Zip archive: Invalid CDFH offset in EOCD |
| samples/06-unknown-fbf5dc58.pdf | 解析器崩溃: assertion failed: name == "Identity-H" |

## 已排除（非发票）

| 样本 | 原因 |
|---|---|
| samples/07-unknown-f1ad9ccb.pdf | 酒店结账单（非发票），含消费明细但无发票号码，实际发票为样本 06 |
| samples/12-didi-cc75b327.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/14-didi-357b5b11.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/16-didi-85a0f5d3.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/18-didi-968f2b9b.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/20-didi-87338868.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/22-didi-0cd6a9c3.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/24-didi-4f308da8.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/26-unknown-d3006c0b.jpg | 176x64 近空白 PNG（非白像素 0.4%），OCR 检测不到任何文本，邮件装饰图元 |
| samples/30-unknown-5be79379.pdf | 发票运单明细附件（非发票），含运单列表但无价税合计，实际发票为样本 27 |
| samples/34-unknown-993123fa.pdf | 顺丰同城订单详情（非发票），含运单列表但无价税合计，实际发票为样本 31 |
| samples/49-didi-2745a005.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/51-didi-2ed3ee1a.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/53-didi-705a5872.pdf | 曹操出行行程单（非发票），仅含行程明细，无发票号码 |
| samples/55-didi-4dc63148.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/56-didi-22a68dc7.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
| samples/58-didi-25f40401.pdf | 行程报销单（非发票），仅含行程明细，无发票号码 |
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
