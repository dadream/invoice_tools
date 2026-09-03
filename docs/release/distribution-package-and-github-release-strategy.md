# Windows 最小分发包与 GitHub 版本发布方案

> 状态：内部Alpha最小包与标签流水线已实施；`v0.1.0`远端演练待完成
> 日期：2026-09-03
> 对应决策/需求：D32、R25、R26、R28、R41

## 1. 审查结论

当前便携包可以运行，但不适合作为长期用户分发结构。问题不是 Markdown 导致包体过大，而是用户运行包、公开发布附件和内部验证证据没有分层。

当前 `InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA` 解压后共有33个文件、102,129,013 B；压缩ZIP为58,796,257 B。程序、OCR worker、DLL和模型等7个运行文件合计101,326,862 B；11个内部报告类文件仅74,987 B，约占解压体积0.07%。因此：

- 删除内部 Markdown 几乎不会改变下载大小，但会显著改善目录可读性、发布边界和用户信任；
- OCR模型和运行库才是包体主体，它们是当前离线识别能力所必需，不能为了“看起来小”而删除；
- SBOM、发布说明、IT审核材料和验证证据仍有价值，但应作为发布旁挂附件或受控证据，不应散落在用户程序目录。

## 2. 当前实现问题

### 2.1 用户包混入内部材料

`scripts/build-portable.ps1` 当前把以下类型直接复制进用户ZIP：开放缺陷、Windows/OCR验证、性能报告、数据迁移审计、夹具清单、历史隐私处置、Concur设计验证、IT审核和版本schema。这些材料面向研发、测试、IT或发布负责人，不是终端用户启动软件所需内容。

风险包括：

- 用户不知道哪些文件需要阅读，首次使用入口不突出；
- 历史缺陷或阶段性验证结论容易被误解为当前产品状态；
- 内部文件名、验证结构和过程信息被不必要地公开；
- 每次报告新增都可能无意扩大用户包内容，缺少稳定白名单。

### 2.2 发布版本有三个手工来源

当前版本 `0.1.0` 同时存在于：

- `src-tauri/tauri.conf.json`；
- `src-tauri/Cargo.toml`；
- `ui/package.json`。

构建脚本只读取Tauri版本，没有在构建前验证三者一致。手工升级到`0.2.0`时，只漏改一个文件就可能出现界面、二进制元数据、包名或依赖元数据不一致。

### 2.3 GitHub只有质量检查，没有发布流水线

`.github/workflows/windows-quality.yml` 会在push、pull request和手工触发时运行Windows质量门禁，但不会：

- 校验发布标签与三个版本文件；
- 构建最小便携包；
- 执行签名阶段；
- 生成Release、独立哈希、SBOM或构建来源证明；
- 区分内部Alpha、公开Beta和稳定版；
- 要求发布负责人批准。

## 3. 三层产物模型

### 3.1 A层：终端用户 portable ZIP

只允许以下白名单：

```text
InvoiceAssistant-<version>-windows-x64-portable/
├─ InvoiceAssistant.exe
├─ invoice-ocr-worker.exe
├─ ocr/
│  ├─ onnxruntime.dll
│  ├─ onnxruntime_providers_shared.dll
│  └─ models/*.onnx
├─ LICENSES/
│  ├─ FONTS/*
│  └─ OCR/*
├─ README-FIRST.txt
├─ PRIVACY.md
├─ USER-AGREEMENT.md
├─ THIRD-PARTY-NOTICES.txt
├─ version.json
├─ manifest.json
└─ SHA256SUMS.txt
```

说明：

- 内部Alpha可使用明确写明“仅内部验证”的隐私/协议草案；公开Beta和正式版必须替换为定稿文本；
- 字体和OCR许可证归入`LICENSES`，根目录只保留用户真正需要看到的入口；
- `manifest.json`覆盖包内有效载荷，`SHA256SUMS.txt`用于解压后校验；
- 不携带WebView2 Fixed Runtime，继续使用系统Evergreen Runtime和既有缺失提示。

明确禁止进入用户ZIP：

- `OPEN-DEFECTS*`、`VALIDATION*`、`OCR-PERFORMANCE*`；
- `FIXTURE*`、`PRIVATE-FIXTURE*`、迁移/架构/设计审计；
- `IT-REVIEW*`、`VERSION-MANIFEST-SCHEMA*`；
- 源码、测试、日志、数据库、EML、环境文件和秘密；
- `SBOM.cdx.json`和长发布说明（它们进入B层）。

### 3.2 B层：GitHub Release公开附件

每个版本只发布：

```text
InvoiceAssistant-<version>-windows-x64-portable[-UNSIGNED-INTERNAL-ALPHA].zip
InvoiceAssistant-<version>-windows-x64-portable....zip.sha256
InvoiceAssistant-<version>-SBOM.cdx.json
```

发布说明使用GitHub Release正文；如确有离线留档要求，再额外提供单独`RELEASE-NOTES-<version>.md`，不塞回程序ZIP。公开Beta/正式版还应显示签名主体、时间戳状态、支持/隐私入口和回退说明。

### 3.3 C层：内部构建与验证证据

以下只保存在受控Actions artifact或内部存储：

- 质量门禁和portable复验JSON；
- IT审核包、开放缺陷快照、性能报告；
- 签名/时间戳验证、恶意软件扫描和审批记录；
- 构建日志、runner镜像版本、源提交和依赖锁摘要。

不得把真实邮件、完整发票、数据库、授权码或用户DataRoot放入任何构建证据。证据设置有限保留期；需长期保留的Go/No-Go摘要只存脱敏结论和哈希。

## 4. 本地 artifacts 目录规划

后续生成内容采用：

```text
artifacts/
├─ releases/<version>/<run-id>/     # 当前候选ZIP及B层附件
├─ evidence/<run-id>/               # C层机器证据
└─ archive/                          # 受保留策略管理的旧候选
```

约束：

- `artifacts`继续保持Git忽略和2 GiB硬门禁；
- 默认不覆盖同版本同run-id产物；重复构建生成新run-id；
- 现有`prune-build-archives.ps1`只在预览后清理历史候选，保留当前候选和必要证据；
- 本次方案不自动删除现有artifacts，迁移和清理由发布负责人确认清单后单独执行。

## 5. 版本策略

### 5.1 版本格式

应用和标签使用完整SemVer：

| 场景 | 应用版本 | Git标签 | GitHub Release |
|---|---|---|---|
| 第一轮内部验证 | `0.1.0-alpha.1` | `v0.1.0-alpha.1` | prerelease、可未签名且明确标记 |
| 第一轮稳定内部候选 | `0.1.0` | `v0.1.0` | 仍可按内部策略标记prerelease |
| 第二轮功能迭代 | `0.2.0-alpha.1` | `v0.2.0-alpha.1` | prerelease |
| 第二轮公开Beta | `0.2.0-beta.1` | `v0.2.0-beta.1` | prerelease、必须签名 |
| 第二轮稳定版 | `0.2.0` | `v0.2.0` | 正式Release、必须签名 |

不使用`0.1`、`0.2`，因为它们没有patch位，无法清晰表达`0.1.1`修复版。0.x阶段建议：

- patch：兼容性修复、识别规则修正、小型UI修复，不改变数据/交互承诺；
- minor：新增用户可见能力或显著流程变化，例如从0.1.x进入0.2.0；
- major：1.0.0用于产品和兼容承诺稳定后的首个正式大版本。

应用版本与`database schema version`、`backup format version`、`parser version`、`grouping rule version`独立。发布0.2.0不等于数据库必须升一版；只有真实格式变化才升级对应内部版本并提供迁移。

### 5.2 单一操作入口

新增两个脚本：

- `scripts/set-version.ps1 -Version 0.2.0`：验证SemVer并原子更新三个版本文件，刷新作为派生文件的`Cargo.lock`，随后校验；
- `scripts/assert-release-version.ps1 -Tag v0.2.0`：只读检查标签、三个版本文件、包名和生成元数据一致。

版本脚本不得自动创建Git标签或发布，避免一次命令把未审核代码推向用户。

## 6. GitHub Actions策略

### 6.1 保留日常质量流水线

`windows-quality.yml`继续处理push/PR，只做验证，不生成正式Release。改进时应：

- 固定Windows runner系列并在证据中记录实际镜像版本；
- 第三方Action使用完整commit SHA而非仅主版本标签；
- 继续使用`npm ci`、`Cargo.lock`和固定Rust工具链；
- PR工作流不读取签名或发布秘密。

### 6.2 新增标签发布流水线

新增`.github/workflows/windows-release.yml`，触发条件：

```yaml
on:
  push:
    tags:
      - 'v*.*.*'
  workflow_dispatch:
```

实际工作流还必须在脚本层严格解析SemVer，不能只依赖glob。

建议阶段：

1. **source**：检出精确标签提交，确认标签指向受允许分支/提交并记录SHA；
2. **version**：校验标签与Tauri/Rust/UI版本一致，拒绝脏来源和重复Release；
3. **quality**：运行`verify-windows.ps1`及秘密、许可、磁盘门禁；
4. **build**：release构建两个自有EXE；
5. **sign**：内部Alpha显式跳过并命名；公开Beta/正式版必须从受保护环境取得签名秘密，对两个EXE使用同一主体签名并加时间戳；
6. **package**：只按A层白名单封装，生成manifest、包内校验、ZIP；
7. **verify**：解压到含中文和空格的临时目录，验证哈希、包内容、签名策略、OCR worker和主程序启动；
8. **metadata**：在最终ZIP上生成独立SHA-256、SBOM和构建来源证明；
9. **publish**：经过`release`环境人工批准后创建GitHub draft/prerelease或正式Release；
10. **post-check**：重新下载Release资产复核大小、哈希和签名，并在正式版本清单最后原子发布。

GitHub官方支持按标签过滤工作流、使用Environment required reviewers保护发布job，以及为二进制生成artifact attestation。仓库计划启用这些能力；若当前私有仓库套餐不支持required reviewers，则使用“只生成draft + 发布负责人在GitHub UI手工Publish”的等价门禁。

### 6.3 最小权限与秘密

- 默认`permissions: contents: read`；
- 只有publish job取得`contents: write`；
- 来源证明job按需取得`id-token: write`和`attestations: write`；
- 签名证书/口令不得存储在仓库、脚本、artifact或日志中；
- fork/PR构建永远不能接触签名秘密；
- 同一版本使用concurrency锁，避免并发生成两个Release。

## 7. 从0.1.0发布到0.2.0的操作流程

### 7.1 准备0.2.0

1. 确认0.2范围和迁移影响，关闭该版本P0/P1；
2. 运行`set-version.ps1 -Version 0.2.0`；
3. 更新用户可读Release Notes，不把内部报告加入程序包；
4. 在PR中通过Windows质量门禁并合入发布基线分支；
5. 从已审核提交创建带说明的`v0.2.0`标签并推送；
6. GitHub标签工作流自动构建，不上传本地`artifacts`里的旧ZIP；
7. 审核最小包清单、签名、哈希、SBOM、来源证明和回退方案；
8. 产品负责人批准GitHub Environment或手工发布draft；
9. 发布后重新下载复验，最后更新应用读取的HTTPS版本清单。

### 7.2 修复0.2.1

从0.2.0对应代码修复并验证，版本改为0.2.1，创建`v0.2.1`。不得覆盖或移动`v0.2.0`标签，也不得替换已经发布的同名资产；每个用户下载包必须能唯一追溯到一个提交和一次流水线。

## 8. 工程实施清单

### P0：下一次候选包前

- 修改`build-portable.ps1`，把用户ZIP改为A层显式白名单；
- 修改`verify-portable.ps1`，增加允许路径/禁止内部文档的双向断言；
- 把SBOM和Release Notes移到B层，不再进入stage目录；
- 新增版本设置和一致性校验脚本；
- 新增`windows-release.yml`，至少完成未签名内部Alpha的tag→draft prerelease演练；
- 验证`v0.1.0`和`v0.2.0`两次演练产生不同且可追溯的资产。

### P0：公开Beta前

- 接入Authenticode证书与可信时间戳，两个自有PE签名主体一致；
- 使用受保护发布环境或等价双人/人工发布门禁；
- 定稿隐私政策、用户协议、发布主体、支持和安全联系方式；
- 关闭Git历史隐私处置阻断，并从干净克隆生成最终候选；
- 对最终签名ZIP重跑完整门禁和Release下载后复验。

### P1：发布工程完善

- 第三方Actions固定到完整commit SHA；
- 生成GitHub artifact attestation并记录验证命令；
- 为A/B/C三层产物设置明确保留期和磁盘告警；
- 为包内容生成快照测试，新增文件默认失败而不是静默进入用户包；
- 将正式下载页与手动版本清单接到最终签名Release资产。

## 9. 验收标准

- 用户ZIP根目录没有内部报告、开放缺陷、性能/夹具/IT/schema文档；
- 包内每个文件都属于显式白名单，缺必需文件或多一个未知文件都构建失败；
- 版本标签、三个版本文件、`version.json`、`manifest.json`、ZIP名和Release名完全一致；
- `v0.1.0`与`v0.2.0`能从各自干净提交独立重建，旧标签和资产不可覆盖；
- Release只包含约定的ZIP、独立哈希和SBOM；内部证据不在用户下载目录；
- Alpha未签名状态清晰；公开Beta/正式版缺签名、时间戳、批准或下载后复验时无法发布；
- 自动构建遵守`artifacts <= 2 GiB`及保留策略，不触碰用户数据和测试输入。

## 10. 2026-09-03 实施结果

- `build-portable.ps1`已改为A层显式白名单，SBOM移出用户ZIP；
- `verify-portable.ps1`已同时验证关键必需文件、禁止项和白名单外文件，并在`SkipLaunch`时仍运行OCR worker金样；
- 已新增`set-version.ps1`、`assert-release-version.ps1`和可重复的版本自检，`0.1.0`→`0.2.0`及错误标签拒绝均通过；
- 日常Windows CI已加入版本一致性门禁；新增标签流水线把只读构建与持写权限的发布job分开，发布job创建draft prerelease且拒绝覆盖既有版本；
- 最新本地release候选为19个文件、58,221,433 B；内部报告0、禁止文件0、未知分发文件0；主程序/worker签名状态均为`NotSigned`，符合内部Alpha命名，OCR worker两项金样和DLL搜索加固通过；
- 完整Rust/OCR门禁通过；本机受限沙箱中的前端统一脚本因esbuild `spawn EPERM`中止，随后在允许子进程的同一锁定依赖环境中重跑，Svelte 0错误/0警告、Vitest 58/58和Vite生产构建均通过。

## 11. GitHub能力依据

- [按分支或标签筛选工作流触发](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)
- [使用Environment、required reviewers和环境秘密保护发布](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
- [为二进制构建生成artifact attestation](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
- [第三方Action固定到完整commit SHA的安全建议](https://docs.github.com/en/packages/managing-github-packages-using-github-actions-workflows/publishing-and-installing-a-package-with-github-actions)
