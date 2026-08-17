# 合规文档

本目录包含发票助手（Invoice Assistant）的法律和合规文档。

---

## 📄 文档列表

| 文档 | 用途 | 目标读者 |
|------|------|---------|
| [隐私政策](./privacy-policy.md) | 说明数据收集、使用和保护方式 | 最终用户 |
| [用户协议](./terms-of-service.md) | 软件使用条款和免责声明 | 最终用户 |
| [开源许可证](../../LICENSE) | 软件授权条款 | 开发者、分发者 |

---

## 🎯 关键原则

### 本地优先（Local-First）
- ✅ 所有用户数据存储在本地设备
- ✅ 核心功能离线可用
- ✅ 永不上传发票内容到云端

### 用户控制（User Control）
- ✅ 用户完全拥有自己的数据
- ✅ 可随时导出或删除数据
- ✅ 匿名遥测默认关闭且可禁用

### 透明开源（Transparent & Open）
- ✅ 源代码公开可审查
- ✅ 隐私和安全实践透明
- ✅ 社区驱动开发

---

## 📋 合规检查清单

### 首次发布前（MVP）

- [x] 隐私政策撰写完成
- [x] 用户协议撰写完成
- [ ] 开源许可证确认（MIT 或 Apache 2.0）
- [ ] 第三方依赖许可证审查
- [ ] 应用内显示隐私政策和用户协议链接
- [ ] 首次运行时显示协议接受界面

### GDPR 合规（如面向欧盟用户）

- [x] 数据最小化原则（仅收集必要数据）
- [x] 数据可携权（Excel 导出）
- [x] 删除权（本地数据，用户可随时删除）
- [x] 透明度要求（隐私政策详细说明）
- [ ] 同意机制（首次运行时请求同意）
- [ ] 数据保护影响评估（DPIA，如适用）

### CCPA 合规（如面向加州用户）

- [x] 隐私政策披露收集的数据类型
- [x] 不出售用户数据承诺
- [x] 用户有权访问和删除数据
- [ ] 提供"不出售我的信息"链接（如有销售行为）

### 中国网络安全法

- [x] 用户数据存储在中国境内（本地存储）
- [x] 加密存储敏感信息（密码）
- [x] 不违法收集、使用个人信息
- [ ] 网络安全等级保护备案（如商业运营）

---

## 🔐 数据安全措施

### 已实施

| 措施 | 实现位置 | 说明 |
|------|---------|------|
| **密码加密** | `crates/invoice-store/src/crypto.rs` | AES-256-GCM + AEAD |
| **密钥管理** | `crates/invoice-store/src/keychain.rs` | 系统 Keychain 或文件（0600） |
| **TLS 加密** | IMAP 连接 | 邮箱通信加密 |
| **本地存储** | `~/.invoice-assistant/` | 数据不上传云端 |
| **权限控制** | 文件系统权限 | 数据库和密钥文件仅当前用户可读 |

### 计划改进

- [ ] 数据库加密（整个 SQLite 数据库加密）
- [ ] 安全日志（记录敏感操作）
- [ ] 自动备份提醒
- [ ] 密码强度检查

---

## 📝 应用内集成

### 必须显示的位置

1. **首次运行向导**
   - 显示隐私政策和用户协议摘要
   - 要求用户勾选"我已阅读并同意"
   - 提供完整文档链接

2. **设置界面**
   - 隐私政策链接
   - 用户协议链接
   - 开源许可证链接
   - 第三方库许可证列表

3. **关于页面**
   - 版本信息
   - 版权声明
   - 开源许可证
   - 联系方式

### 实现示例

```typescript
// ui/src/lib/compliance.ts
export const PRIVACY_POLICY_URL = 'https://example.com/privacy'
export const TERMS_OF_SERVICE_URL = 'https://example.com/terms'
export const GITHUB_REPO = 'https://github.com/yourorg/invoice-assistant'

export function showPrivacyPolicy() {
  // 在浏览器中打开隐私政策
  window.open(PRIVACY_POLICY_URL, '_blank')
}
```

```svelte
<!-- ui/src/routes/welcome/+page.svelte -->
<label>
  <input type="checkbox" bind:checked={agreedToTerms} />
  我已阅读并同意
  <a href={PRIVACY_POLICY_URL} target="_blank">隐私政策</a>
  和
  <a href={TERMS_OF_SERVICE_URL} target="_blank">用户协议</a>
</label>
```

---

## 🌍 国际化注意事项

### 当前状态
- 文档语言：中文
- 目标用户：中国大陆用户

### 未来扩展
如果支持国际用户，需要：
- [ ] 翻译隐私政策和用户协议（英文、繁体中文等）
- [ ] 根据不同地区调整合规要求
- [ ] 提供语言切换功能

---

## 📞 法律事务联系

如有法律相关问题：
- **隐私问题**: privacy@invoice-assistant.example
- **许可证问题**: legal@invoice-assistant.example
- **安全问题**: security@invoice-assistant.example

---

## 🔄 文档更新流程

1. **提议变更**: 在 GitHub Issues 中讨论
2. **起草修订**: 更新相关 Markdown 文件
3. **社区审查**: Pull Request 接受社区反馈
4. **版本控制**: Git 记录所有历史版本
5. **用户通知**: 应用内显示变更通知

---

## ⚖️ 法律声明

本目录的文档为模板示例，**不构成法律建议**。在正式发布前，建议：

1. 咨询专业律师审查
2. 根据实际运营模式调整
3. 确保符合目标地区法律
4. 定期审查和更新

---

**最后更新**: 2026-08-17  
**下次审查**: 2026-09-17（建议每季度审查一次）
