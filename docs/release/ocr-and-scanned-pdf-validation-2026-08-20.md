# 离线 OCR 与扫描 PDF 验证记录（2026-08-20）

> 结论：Windows 内部 Alpha 的离线图片/扫描 PDF OCR 路径已实现并通过非隐私合成回归；这不是公开发布准确率结论。真实或私有票据集、真实混合批次/最低配置性能和物理机 UI 证据仍待补齐。

## 1. 产品行为

- 支持图片：PNG、JPG/JPEG、WebP、BMP。
- 文本 PDF 保持现有 L1 文本解析；只有定位文本和纯文本解析均失败时，才使用 Windows 原生 `Windows.Data.Pdf` 将页面在内存中渲染并进入 L2 OCR。
- OCR 全程本地运行，不访问网络，不依赖 Python，也不要求安装 OCR 软件。
- OCR 运行在独立 `invoice-ocr-worker.exe`：主程序最多启动 1 个，每文件 45 秒硬超时，完成/崩溃/超时后释放子进程资源；Release 启动前按 portable manifest 校验 worker SHA-256。两个自有 PE 的静态依赖只从 System32 解析；进程运行期只允许 System32 与经哈希验证后显式登记的 OCR 目录。
- L2/L4 结果以及置信度低于 0.90 的 L0/L1 结果一律设置 `requires_review`；审核原因写入流水线 evidence JSON，不允许静默自动通过。
- 邮箱附件与本地来源共用同一格式分类、解析和复核策略。

## 2. 固定依赖与资产

| 资产 | 版本/来源 | SHA-256 |
|---|---|---|
| Paddle OCR Rust | `paddle-ocr-rs = 0.6.1` | 由 Cargo.lock 固定 |
| ONNX Runtime | 1.22.0 Windows x64 | `174C616EFC0271194488642A72F1A514E01487DA4DFE84C49296D66E40EBE0DA`（官方 ZIP） |
| `onnxruntime.dll` | 1.22.0 | `579B636403983254346A5C1D80BD28F1519CD1E284CD204F8D4FF41F8D711559` |
| `onnxruntime_providers_shared.dll` | 1.22.0 | `BA00EA1EF846C9B909C7854BC56C51051A20F9773B3E1153DDA118D4B85D0B93` |
| 文本检测模型 | RapidOCR v3.9.2 | `4D97C44A20D30A81AAD087D6A396B08F786C4635742AFC391F6621F5C6AE78AE` |
| 方向分类模型 | RapidOCR v3.9.2 | `E47ACEDF663230F8863FF1AB0E64DD2D82B838FCEB5957146DAB185A89D6215C` |
| 文本识别模型 | RapidOCR v3.9.2 | `5825FC7EBF84AE7A412BE049820B4D86D77620F204A041697B0494669B1742C5` |

`third_party/ocr/ocr.lock.json` 固定来源 URL、版本、大小和文件哈希；许可证扫描验证模型、运行时和许可证文件。程序启动 OCR 前再次校验运行时和模型哈希，并拒绝符号链接资产。

## 3. 资源与失败边界

- 图片/附件最大 25 MiB。
- 图片最长边最大 12000 像素，总像素最大 5000 万。
- 扫描 PDF 最大 25 MiB、1–5 页，每页内存渲染流最大 25 MiB。
- PDF 页面不落临时图片文件；渲染运行在独立 MTA 线程。
- Windows 显示缩放会使 2000 DIP 的渲染请求在当前 140% 环境得到约 2800 物理像素；渲染结果仍受统一像素和尺寸上限保护。
- OCR 初始化、资产损坏、页面渲染、超限或无法识别时返回可审计错误，不降级为无提示的成功结果。
- worker stdin 请求上限 64 KiB、stdout 上限 1 MiB；并发请求立即拒绝，超时 worker 被实际终止，不在后台继续占用模型内存。

## 4. 非隐私合成 golden

固定样本：

- `fixtures/synthetic/ocr-vat-invoice.png`
- `fixtures/synthetic/ocr-vat-invoice-scanned.pdf`
- 扫描 PDF SHA-256：`4E9455E29F7FE7AEA73A300C5F7B63F53B363616F9679B007D61F7656C89C5F5`

样本仅含虚构信息。图片和扫描 PDF 均成功提取：发票号码 `26112000000000000001`、开票日期 `2026-06-18`、购买方“北京示例科技有限公司”、销售方“上海演示商贸有限公司”、价税合计 `1200.00`、税额 `67.92`。

已发现并修复：

1. 单列图片出现完整“购买方名称/销售方名称”标签时，通用左右位置规则导致购销方错配。
2. 高 DPI 图片中“税额”表头像素宽度过大，旧固定宽度过滤导致税额定位失败；现改用语义字符数。
3. 流水线此前未将解析层级/置信度纳入批次复核判断；现 L2 OCR 和低置信结果强制复核并保留证据原因。

已知限制：可选税率字段在当前合成图中可能识别为 `%9`；该字段不参与本轮核心字段通过判定，L2 状态保证用户必须复核。只有合成样本通过，不能据此声明真实票据准确率达标。

## 5. 自动化结果

- `cargo test --workspace --all-targets --locked`：454 通过、0 失败、10 默认忽略。
- 图片 OCR golden：显式执行通过。
- 扫描 PDF OCR golden：显式执行通过。
- 生产 worker 图片与扫描 PDF 进程端到端 golden：显式执行通过。
- DLL 搜索对抗：主程序/worker PE `DependentLoadFlags=0x0800`；伪造 `onnxruntime.dll`、伪造 provider DLL、恶意工作目录、PATH 与 ORT_DYLIB_PATH 同时存在时，release worker 两项 golden 仍通过。
- 高 DPI 税额表头回归：通过。
- L2/低置信强制复核回归：通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`：通过。
- UI Vitest：14/14 通过。
- 第三方许可证/资产扫描：Cargo 701、npm 191、字体资产 5、OCR 资产 8，全部通过。
- 参考机器生产 worker：图片 5 次均值 3.594 秒、扫描 PDF 3 次均值 4.246 秒；50 张估算 179.7/212.3 秒；峰值约 377/508 MiB。完整范围见 `ocr-performance-2026-08-20.md`。

## 6. 公开发布前仍需补齐

1. 在获得合法、脱敏或私有受控样本后，建立至少 10 张覆盖不同版式、清晰度、DPI 和多页扫描的黄金集，逐字段统计准确率、需复核率和失败率。
2. 当前参考机器合成 worker 基线已完成；仍需在目标最低配置和至少两台物理 Windows 电脑记录真实混合批次耗时、峰值内存和 UI 响应。
3. 完成 100%/125%/150% 缩放下的导入、OCR 进度、错误提示、三栏复核和导出 UI 走查。
4. 决定并实现 OFD 栅格化 OCR 范围，或在正式支持矩阵中明确不支持扫描型 OFD。
5. 真实 QQ 邮箱验证必须取得单独明确授权，并仅执行 IMAP 只读与前后 FLAGS 不变核对；本记录未访问真实邮箱。
6. 完成 Authenticode 签名、干净企业电脑策略验证、外部 UAT 和产品负责人最终 Go/No-Go。