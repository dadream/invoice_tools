# 数据版本与迁移回退审计（2026-08-20）

> 2026-08-24 增补：当前台账 schema 已升级到 v7，用于补标同一批次内发票号完全一致的后续记录；下文保留 v6 候选包的历史进程级证据，并明确 v7 的新增门禁。

## 1. 结论

M2.4 的本地开发与自动验证范围已完成：数据库、解析结果、归组结果、备份格式和输出均保存明确版本；数据库拒绝未来 schema；解析检查点拒绝未来格式；迁移在单一事务内完成且失败不提交；免安装 EXE 已完成“旧库升级 → 使用迁移前快照回滚 → 再次升级”的进程级验证。

本审计没有访问真实邮箱、真实 Concur 租户或用户数据。验证使用唯一临时数据根和合成哨兵记录，成功后清理临时数据，仅保留不含业务内容的 JSON 证据。

## 2. 版本边界

| 数据边界 | 当前版本 | 保存位置 | 写入/读取约束 | 自动证据 |
|---|---:|---|---|---|
| SQLite 台账 schema | `7` | `ledger.db` 的 `PRAGMA user_version`；单一来源 `LEDGER_SCHEMA_VERSION` | 打开旧库前创建 `Data\migration-backups` 快照；v0→v7 在单一事务中迁移；v7 只补标同批次精确票号的后续记录，不删除原件；大于当前版本时零修改拒绝 | `old_schema_open_creates_verified_snapshot_before_migration`、`migration_failure_rolls_back_original_database_byte_for_byte`、`future_schema_is_rejected_before_any_current_schema_write`、`v7_migration_marks_later_same_batch_numbers_without_overriding_review` |
| 解析结果检查点 | `formatVersion=1`；`parserVersion=0.1.0` | `Data\tasks\...\parsed.json` 的版本包络 | `write_parsed` 只写版本化包络；`load_parsed` 在恢复前拒绝不支持的格式或空解析器版本；旧的裸数组不会被静默解释 | `parsed_checkpoint_saves_versions_and_rejects_future_format` |
| 归组结果 | `deterministic-v1` | `batch_grouping.rule_version`，并进入输出任务键和每条匹配依据 | `GROUPING_RULE_VERSION` 是算法侧单一常量，流水线不再散落硬编码 | 流水线/台账归组回归；固定输入结果可复现测试 |
| 未加密备份 | `formatVersion=1`；`databaseSchemaVersion=6..7`；`productVersion=0.1.0` | ZIP 内 `backup-manifest.json` | 当前程序接受 v6/v7 备份；切换前以只读方式核对清单版本、数据库实际版本、SHA-256 与 SQLite 完整性，不迁移暂存库；v6 只在原子切换成功后由正常启动迁移到 v7 | `import_accepts_v6_backup_for_post_switch_migration_only`、`backup_database_inspection_is_read_only`、备份篡改、未来版本、跨数据根往返和原子导入测试 |
| Excel/CSV/PDF 输出包 | `formatVersion=2`；`generatorVersion=0.1.0` | 输出目录 `job.json` 和 `manifest.json` | 稳定任务键包含版本、批次、审核后发票、归组和原件指纹；完成目录按版本、任务键、文件数量/大小/SHA-256 复验，禁止静默覆盖 | 输出幂等、局部恢复、manifest 篡改和 A4/CSV/Excel 对账测试 |
| portable 发布元数据 | `schemaVersion=1`；产品版本 `0.1.0` | 包内 `version.json` | 构建与复验同时检查产品、通道、签名状态、更新策略、Concur 门禁和 DLL 加固元数据 | `verify-portable.ps1` 与候选包 validation JSON |

## 3. 迁移与回退验证

### 3.1 单元与故障注入

- v0 数据库可连续升级到当前 schema，原记录保留。
- v5→v6 在完成 DDL、提交版本号之前注入失败；事务回滚后原数据库文件逐字节不变，`user_version` 仍为 5，v6 表不存在，原批次、发票和设置保留，`integrity_check=ok`。
- v6→v7 只把同一批次、去除首尾空白后发票号完全一致的较晚记录标为“疑似重复”；首条记录、原件和数据库行均保留。存在有效“人工确认非重复”审核动作的批次不会被回填覆盖。
- v6 备份在导入切换前保持逐字节不变；只读检查确认数据库实际 schema 与清单一致，切换成功后才按正常启动流程生成快照并迁移到 v7。
- 旧 schema 打开前通过 `VACUUM INTO` 生成独立快照，校验快照版本与 `integrity_check` 后再原子发布；当前 schema 不重复生成快照。
- 未来 schema 拒绝打开并保持文件不变。

### 3.2 免安装 EXE 进程级回退

脚本 `scripts/verify-packaged-migration-rollback.ps1` 执行以下闭环：

1. 在唯一临时数据根创建含合成哨兵记录的 v5 台账。
2. 以 `INVOICE_ASSISTANT_HOME` 指向该隔离目录启动候选包 EXE。
3. 验证主库升级到 v6、两个 Concur 表存在、迁移前快照仍为 v5 且完整、哨兵记录保留。
4. 关闭应用，把当前主库移到同一测试目录，并逐字节复制已验证快照为 `ledger.db`。
5. 再次启动同一 EXE，验证回滚后的 v5 主库再次升级到 v6，完整性为 `ok`，哨兵记录仍在，并新增第二份迁移前快照。
6. 核对 EXE 验证前后 SHA-256 不变，删除经边界校验的唯一临时目录。

该段是 v6 候选包的历史进程级证据：`artifacts/packaged-migration-rollback.validation.json`。关键结论为 `rollbackCopyMatchesSnapshot=true`、`remigratedSchemaVersion=6`、`remigratedIntegrity=ok`、`sentinelPreserved=true`、`programFileUnchanged=true`。v7 当前已通过完整 Rust 工作区回归和上述专项迁移/备份测试；真实批次 v6→v7 的可见结果由测试负责人在本次候选包首次启动时确认。

## 4. 用户回退操作约束

回退只能在应用完全退出后进行；用户应先备份当前 `ledger.db`，再从 `Data\migration-backups` 选择目标快照复制为 `ledger.db`。如果升级后已经录入新数据，旧快照不包含这些新数据，不应直接回退。`README-FIRST.txt` 必须随包保留这一限制和操作步骤。

该回退机制只处理本机数据库 schema，不降级程序文件，也不绕过备份导入的格式、哈希和 SQLite 完整性校验。
