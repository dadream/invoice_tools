# 手动版本检查清单规范

> 状态：schema v1。当前未签名内部 Alpha 未配置正式地址，因此点击检查时会明确显示“未配置”，且不发起网络请求。

## 发布配置

构建正式候选前，由发布负责人把固定 HTTPS 清单地址写入构建环境变量 `INVOICE_UPDATE_MANIFEST_URL`。地址必须：

- 使用 HTTPS，包含有效主机；
- 不包含用户名、密码或 URL fragment；
- 指向最终 JSON，不依赖 HTTP 重定向；
- 与 IT 审核材料、隐私政策和正式下载渠道一致。

应用不会后台检查。只有用户点击“设置 → 版本与支持 → 手动检查更新”时才读取该地址；请求使用系统证书和系统代理，连接超时 5 秒、总超时 10 秒，响应上限 64 KiB，且必须返回 `application/json`。

## schema v1

```json
{
  "schemaVersion": 1,
  "product": "com.dadream.invoiceassistant",
  "channel": "internal-alpha",
  "version": "0.1.1",
  "summary": "安全与稳定性更新",
  "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
  "downloadPageUrl": "https://downloads.example.invalid/invoice-assistant",
  "publishedAtUtc": "2026-08-20T00:00:00Z"
}
```

约束：

- `product` 和 `channel` 必须与应用完全一致；
- `version` 必须是语义版本；
- `summary` 必填、不超过 1000 个字符且不能含控制字符；
- `sha256` 是最终签名 ZIP 的 64 位十六进制 SHA-256；
- `downloadPageUrl` 必须是无凭据、无 fragment 的 HTTPS 页面，不得直接触发静默下载；
- `publishedAtUtc` 必须是 RFC 3339 时间；
- schema v1 拒绝未知字段，字段变化必须升级 schema。

## 安全发布顺序

1. 从指定提交构建所有 PE。
2. 对主程序和 worker 使用同一可信主体签名并加时间戳。
3. 重跑 portable 验证，计算最终 ZIP SHA-256。
4. 发布 HTTPS 下载页、发布说明、签名主体和 SHA-256。
5. 最后原子发布版本清单。
6. 用已发布应用手动检查一次，确认版本、说明、哈希和下载页一致。

版本检查只展示信息并允许用户主动打开系统浏览器；它不会下载、覆盖、降级、安装或请求管理员权限。
