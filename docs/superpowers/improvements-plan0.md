# Invoice Collector Product Improvements

**Status:** Designed, not yet implemented  
**Created:** 2026-08-04  
**Context:** Based on Plan 0 audit findings (100% email recall, 98.9% attachment precision)

## Improvement A: Manual Download Link Support

### Problem
3 invoice notification emails (4.3% of total) contain download links instead of attachments. Currently silently skipped, causing user confusion about missing invoices.

**Affected emails:**
- UID 572: 诺诺网 (nuonuo.com) - restaurant invoice
- UID 578: 百望云 (baiwang.com) - "电子发票下载"
- UID 629: 百望云 (baiwang.com) - "电子发票下载"

### Solution Design

**Phase 1: Detection & Notification**
- Detect emails with subjects containing "发票" but no attachments
- Parse HTML body to extract download URLs (regex: `https?://[^"'\s]+` near "下载"/"发票" keywords)
- Generate `fixtures/manual-downloads.txt` with:
  ```
  需要手动下载的发票通知
  ======================
  
  1. 发件人: invoice@info.nuonuo.com
     主题: 您收到一张【很久以前餐饮管理（上海）有限公司】开具的发票
     日期: 2026-06-03
     下载链接: https://nnfp.jss.com.cn/download?id=xxx
     操作: 打开邮件点击链接，下载后放入 fixtures/samples/
  
  [重复2-3条]
  ```
- Terminal output: `⚠️  3 封发票需要手动下载（详见 fixtures/manual-downloads.txt）`

**Phase 2: Manual Addition Workflow**
- New subcommand: `cargo run -- add-manual <file-path> --platform <name> --format <format>`
- Validates file format, computes SHA256, moves to `fixtures/samples/` with correct naming
- Appends to `fixtures/manifest.toml`
- User workflow:
  ```bash
  # User downloads from email link
  $ cargo run -- add-manual ~/Downloads/invoice.pdf --platform nuonuo --format pdf-vat
  ✓ 已添加: 01-nuonuo-a3f9c1d2.pdf
  ✓ manifest.toml 已更新
  ```

**Phase 3 (Future): Automated Link Following**
- Requires HTTP client, HTML parsing, anti-bot handling
- Security review needed (SSRF risk, expired links, captcha)
- Only implement if >20% of emails have this pattern

### Implementation Files
- `src/main.rs`: add `add-manual` subcommand
- `src/extract.rs`: add `extract_links_from_html(body: &str) -> Vec<String>`
- `src/collect.rs`: detect notification emails, generate manual-downloads.txt

---

## Improvement B: Collection Quality Reports

### Problem
Users don't know collection quality (recall, precision, format gaps) without running manual audit.

### Solution Design

**Feature 1: Auto-generate collection summary**

After `collect` finishes, write `fixtures/collection-report.txt`:

```
发票采集报告
============
采集时间: 2026-08-04 15:30
邮箱地址: 879***187@qq.com（完整地址仅保存在本机测试配置中）
日期范围: 2026-06-01 至 2026-06-30

处理结果
--------
✓ 已采集: 91 个发票文件（来自 69 封邮件）
  
  格式分布:
  • 增值税发票 (pdf-vat): 45
  • 火车票 (pdf-rail): 14  
  • OFD电子发票: 23
  • XML发票: 9
  • 航班行程单: 0 ⚠️
  • 纸质发票扫描件 (image): 0 ⚠️
  
  平台分布:
  • 滴滴出行: 25
  • 美团: 14
  • 12306: 14
  • 其他: 38

⚠️ 需要手动下载: 3 封（详见 manual-downloads.txt）
⚠️ 跳过（无附件）: 3 封（非发票通知）

质量指标
--------
邮件召回率: 100% (所有发票邮件均已处理)
附件准确率: 98.9% (1个可能的误判: header.jpg)

格式缺口
--------
✗ 缺少航班行程单样本（目标: 3个）
✗ 缺少纸质发票扫描件（目标: 10个）

建议操作
--------
1. 扩大日期范围到 2026-01-01 或使用其他邮箱账号
2. 拍摄 10 张纸质增值税发票上传到 fixtures/samples/
3. 检查 fixtures/samples/ 中的 header.jpg 是否为误判

详细日志: fixtures/collection-log.tsv
```

**Feature 2: New `report` subcommand**

```bash
$ cargo run -- report fixtures/samples/

样本质量分析
============
样本路径: fixtures/samples/
文件总数: 91

格式完整性
----------
✓ pdf-vat: 45/3 (超额)
✓ pdf-rail: 14/3 (超额)
✓ xml: 9/5 (超额)
✓ ofd: 23/5 (超额)
✗ pdf-flight: 0/3 (缺失)
✗ image: 0/10 (缺失)

文件完整性
----------
✓ 所有文件可读
✓ 文件大小正常 (3KB - 890KB)
✗ 潜在问题: 
  - fixtures/samples/01-unknown-a3f9c1d2.jpg (66KB, 可能是装饰图)

Manifest状态
------------
✓ 91/91 文件已记录在 manifest.toml
⚠️ 91 个条目等待填写 expected 字段

推荐操作
--------
1. 补充 3 个航班行程单样本
2. 补充 10 个纸质发票扫描样本
3. 填写 manifest.toml 的 expected 字段后执行 Plan 1 spike 验证
```

**Feature 3: Enhanced terminal output**

During `collect`, show progress:
```
连接 IMAP... ✓
搜索 2026-06 邮件... 找到 69 封

采集进度: [████████████████████] 69/69
  ✓ 提取附件: 101 个
  ✓ 分类为发票: 91 个
  ✓ 去重后保存: 91 个
  ⚠️  发现 3 封需要手动下载

生成 manifest.toml... ✓
生成采集报告... ✓

完成！采集了 91 个发票文件
详细报告: fixtures/collection-report.txt
```

### Implementation Files
- `src/main.rs`: add `report` subcommand, enhance `collect` output
- `src/report.rs`: new module for generating collection-report.txt
- `src/stats.rs`: helper functions for format distribution, quality metrics

---

## Improvement C: Documentation Updates

### Plan 0 Documentation
Add to `docs/superpowers/plans/2026-08-03-invoice-email-collector.md`:

**Appendix A: Audit Results (2026-08-04)**
- Date range tested: 2026-06-01 to 2026-06-30
- Email account: 879***187@qq.com (QQ Mail; full address kept in local test configuration only)
- Emails processed: 69
- Accuracy achieved:
  - Email recall: 100% (43/43 invoice emails with attachments captured)
  - Attachment precision: 98.9% (90/91 correct, 1 false positive: header.jpg 66KB)
- Known issues resolved:
  - 12306 railway invoices (14 ZIPs containing 28 files) now fully supported
  - Decorative images filtered (3/4 caught)
  - Ride-hailing misclassification fixed (2 errors → 0)
- Remaining gaps:
  - Manual download links (3 emails, 4.3%) require user intervention
  - Flight itineraries: 0 samples (need date range expansion or different email account)

**Appendix B: User Guide - Manual Download Links**
```markdown
## Handling Invoice Notification Emails

Some invoice platforms (诺诺网, 百望云) send notification emails with download links instead of attachments.

### Workflow
1. Run `collect` - it generates `fixtures/manual-downloads.txt`
2. Open your email client, find the listed emails
3. Click the download link in each email
4. Save the invoice file to your Downloads folder
5. Add to the collection:
   ```bash
   cargo run -- add-manual ~/Downloads/invoice-20260603.pdf \
     --platform nuonuo --format pdf-vat
   ```
6. The file is automatically renamed, moved to `fixtures/samples/`, and added to `manifest.toml`
```

### Files to Update
- `docs/superpowers/plans/2026-08-03-invoice-email-collector.md` (add appendices)
- `README.md` (add user guide link)
- `.superpowers/sdd/progress.md` (record audit baseline)

---

## Priority & Timeline

| Task | Priority | Effort | Blocking |
|------|----------|--------|----------|
| A: Manual download links | Medium | 2-3 hours | Plan 2+ (production use) |
| B: Quality reports | Low | 3-4 hours | Nice-to-have |
| C: Documentation | High | 30 min | Plan 1 (reference) |

**Recommendation:** Complete C (documentation) now, defer A and B until after Plan 1 spike validation succeeds.

---

## Related
- Plan 0 audit report: `audit-report-fixed.tsv`
- Accuracy analysis: (conversation history 2026-08-04)
- Original design: [[plan0-email-collector]]
