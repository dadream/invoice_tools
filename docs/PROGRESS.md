> **历史进度记录，禁止作为当前单一真实来源。** 当前执行清单为 `specs/mvp-release-baseline/tasks.md`；当前候选机器事实为 `artifacts/final-internal-alpha-candidate.validation.json`；发布结论与缺陷为 `docs/release/open-defects.md`；完整 Windows/QQ 验证为 `docs/release/windows-validation-2026-08-19.md`。下文 2026-08-11 的模块表只保留历史。

# 实施进度追踪

> 此文件不再更新，也不再作为发布验收依据。

**历史快照日期**：2026-08-11（commit pending - S0.6）
**当前指针更新**：2026-08-24

---

## 快速验证命令

```bash
# 一键验证当前真实状态
bash scripts/verify-progress.sh

# 或手动检查
cargo test --workspace          # 测试状态
git log --oneline -20            # 最近工作
ls docs/tasks/*.md               # 实施报告
```

---

## 模块完成状态

### ✅ 已完成（有代码 + 测试 + 验证）

| 模块 | Crate | 测试 | 验证状态 | 关键提交 |
|---|---|---|---|---|
| **M1 邮件采集** | `invoice-collect` | 49/51 通过 | ✅ 已用真实邮件（6.1-6.30）验证 | - |
| **M2 解析 L0/L1** | `invoice-parse` | 69/69 通过 | ✅ 64 样本回归，35.9% 全字段通过 | f13bb27 |
| **M2 字段提取** | `invoice-parse` | 11/11 通过 | ✅ city/departure_time/checkin_date | 6f6d71e |
| **M4 归组引擎** | `invoice-grouping` | 21/21 通过 | ✅ 20 合成场景 + 5 类歧义 | 831b07f |
| **S0.4 加密存储** | `invoice-store` | 49/49 通过 | ✅ 双库 + Keychain 集成 | pending |
| **S0.5 批次状态机** | `invoice-store` | 20/20 通过 | ✅ 完整状态转换验证 | pending |
| **S0.6 批次 CRUD UI** | `invoice-assistant` | 208 通过 | ✅ 5命令+3组件，手动测试通过 | pending |

**已知失败测试**（环境问题，非代码缺陷）：
- `invoice-collect::config::tests::account_password_shape_triggers_warning`
- `invoice-collect::config::tests::missing_env_var_mentions_authorization_code`
- 原因：需要 `INVOICE_IMAP_PASSWORD` 环境变量

---

### ⬜ 未开始

| 模块 | 依赖 | 优先级 | 说明 |
|---|---|---|---|
| **M3 校验与去重** | M2 ✅ | **P0** | 流水线必经环节，含跨月台账 |
| **S0.7 发票添加流程** | S0.6 ✅ | **P0** | 批次内发票关联与金额计算 |
| **M6 输出** | M2 ✅ | P1 | Excel/打印件/PDF 渲染 |
| **F2 GenericAdapter** | M6 | P1 | 纯本地模式 |
| **M5 审核界面** | M2 ✅ + M4 ✅ | P1 | 需 Tauri 骨架 |
| **M7-B Concur 邮件收单** | M6 | P2 | 方案 B |
| **E 计费** | S0.4 ✅ | P2 | 账号 + 扣费 + 试运行 |
| **H1 流水线串联** | 全部 | P3 | 端到端集成 |

---

## 当前解析准确率（commit 21f3875 基线）

| 格式 | 样本数 | 通过率 | 状态 |
|---|---|---|---|
| XML-VAT | 7 | 100% | ✅ 生产就绪 |
| OFD | 8 | 50% | ⚠️ 部分可用 |
| PDF-VAT | 41 | 29.3% | ⚠️ 核心字段可用 |
| PDF-Flight | - | - | 待验证 |
| 图片 (L2 OCR) | 6 | 未验证 | ❌ 需 Python sidecar |

**注**：PDF-VAT 29.3% 是"全字段通过"，核心字段（发票号/日期/金额）准确率 94.4%。

---

## 文档可信度分层

规划任务时按此顺序采信：

| 层级 | 来源 | 可信度 |
|---|---|---|
| 1 | Git 提交历史 | 最高（不可篡改） |
| 2 | 代码实际状态（测试/CLI/API） | 高 |
| 3 | `docs/tasks/*-implementation-report.md` | 中（有日期） |
| 4 | `docs/superpowers/plans/*.md` | 低（仅意图） |
| 5 | `HANDOFF.md`、产品 spec | 参考（设计产物） |

**⚠️ 已知过期文档**：
- `docs/HANDOFF.md`（2026-08-03）：声称"尚未写任何代码"，实际已完成 3 个模块
- 使用前务必核对本文件的更新日期

---

## 更新本文件的时机

- 完成一个模块/任务后
- 测试通过率发生变化后
- 发现文档与实际不符时

**更新方法**：修改对应表格 + 更新顶部"最后更新"日期和 commit hash
