# 企业 IT 审核摘要（内部 Alpha）

| 项目 | 行为 |
|---|---|
| 安装/提权 | 免安装，不申请管理员权限，不写安装注册表项 |
| 主进程 | `InvoiceAssistant.exe`（Windows x64） |
| OCR 子进程 | `invoice-ocr-worker.exe`（Windows x64）；每个 OCR 文件启动一次，最大并发 1，45 秒硬超时后终止 |
| UI 运行时 | 系统 Microsoft Edge WebView2 Evergreen Runtime，可能产生系统 WebView2 子进程 |
| OCR 运行时 | 包内 ONNX Runtime 1.22.0 x64 DLL 和 RapidOCR 模型；仅本地加载，不安装、不注册 COM、不联网下载模型 |
| DLL 加载 | 两个自有 EXE 使用 PE `DependentLoadFlags=0x0800`，静态导入仅从 System32 解析；进程默认搜索仅 System32 和显式目录，排除当前目录、PATH 和未验证应用目录；OCR 目录在运行时校验 DLL/模型哈希后通过 `AddDllDirectory` 登记 |
| 程序目录 | 只读运行；不在程序目录写用户数据 |
| 数据目录 | `%LOCALAPPDATA%\InvoiceAssistant\Data` |
| 临时数据 | `Data\temp`，仅处理用户主动选择的文件或邮箱附件；单文件 25 MiB、展开最多 5000 个文件、总暂存量 500 MiB，目录重解析点拒绝 |
| 注册表读取 | 只读查询 WebView2 Runtime `pv` 版本 |
| 注册表写入 | 无 |
| 清除辅助进程 | 仅用户输入确认短语后，将当前已审核 EXE 复制为 `%TEMP%\InvoiceAssistant-cleanup-<随机值>.exe`；主程序退出后按无通配符清单删除产品文件 |
| 清除残留 | Windows 不允许运行中 EXE 自删，临时清理副本留在 `%TEMP%`，不登记重启删除注册表项 |
| 邮箱网络 | 用户主动触发；`imap.qq.com:993`、`imap.163.com:993`，TLS IMAP |
| Concur SMTP | 内部 Alpha 在后端、构建脚本和 portable 元数据中强制关闭，不连接；未来受控构建仅用户逐次确认后使用 TLS SMTP：QQ/163/126/Gmail 465，Outlook 587 强制 STARTTLS，30 秒超时 |
| 版本检查 | 仅用户点击后读取构建时固定的 HTTPS JSON；系统证书/代理，连接 5 秒/总计 10 秒超时，64 KiB 上限，禁止重定向；当前内部 Alpha 未配置地址，不发起请求 |
| 其他外联 | 内部 Alpha 无遥测、无自动下载/替换、无 Concur 外发 |
| 凭据 | 邮箱授权码仅进程内存，不写数据库、日志和备份 |
| 日志 | 本地 `Data\logs`；无自动上传 |
| 任务停止 | 用户主动点击“安全停止”；等待当前文件结束、保留检查点并标记 interrupted，不终止或删除用户原件 |
| 代码签名 | 当前内部 Alpha 未签名；对外 Beta 前必须 Authenticode 签名并加时间戳 |

公开发布状态为 No-Go：历史票据夹具仍需完成 Git 历史、远端缓存和旧制品的受控清理及干净克隆复验；本内部包附处置记录和开放缺陷清单。

审核包同时提供 EXE/ZIP SHA-256、`version.json`、版本清单 schema、`SBOM.cdx.json`、`THIRD-PARTY-NOTICES.txt`、隐私草案、协议草案和发布说明。

界面与 A4 PDF 所需 Source Han Sans CN、IBM Plex Sans/Mono 已随应用嵌入，不安装系统字体、不联网加载字体；完整 OFL 1.1 文本、官方仓库提交和资产哈希包含在审核资料中。

图片与扫描 PDF 识别使用包内 ONNX Runtime 与 RapidOCR 模型；运行时、模型、许可证、来源 URL、文件大小和 SHA-256 记录在 `LICENSES/OCR/ocr.lock.json`、SBOM 和第三方通知中。主程序只启动同目录固定 worker，按 portable manifest 校验 worker SHA-256；worker 不联网、不安装服务、不持久化输入，完成或超时后退出。

DLL 搜索策略依据 Microsoft 的 [SetDefaultDllDirectories](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-setdefaultdlldirectories) 与 [/DEPENDENTLOADFLAG](https://learn.microsoft.com/en-us/cpp/build/reference/dependentloadflag?view=msvc-170)；构建和 portable 复验会直接读取 PE Load Config，配置缺失时拒绝产物。
