# 安全策略（Security Policy）

## 支持的版本

| 版本 | 是否接收安全修复 |
|---|---|
| 最新 Release（0.1.x） | ✅ |
| 更早版本 | ❌ 建议升级到最新版 |

## 报告安全漏洞

**请勿通过公开 issue 报告安全问题。**

请优先使用 GitHub 的私密渠道：

1. 打开仓库的 **Security → Advisories → Report a vulnerability**
   （即 GitHub Security Advisories，只有维护者可见）；
2. 或在仓库 **Security** 页面确认已启用 Private vulnerability reporting 后提交。

报告请尽量包含：

- 受影响版本与 Windows 版本；
- 复现步骤（表达式、操作序列、是否手工构造了本地文件）；
- 影响评估（崩溃 / 拒绝服务 / 越权读写 / 其他）；
- 如方便，附最小 PoC。

我们会在 7 天内确认收到，并在修复发布后于 Release Notes / Advisory 中致谢（除非你希望匿名）。

## 已知的安全边界

- 本程序是**单机桌面应用**：不联网、不加载远程内容、不写注册表，
  仅使用 `%LOCALAPPDATA%\BIGNUM\` 作为 WebView2 缓存目录。
- 计算引擎对用户输入做了资源上限保护（输出位数、求和项数、积分求值次数与时间预算），
  但极端输入仍可能长时间占用 CPU 或消耗大量内存——这属于设计内行为，不作为安全漏洞处理。
- 便携包内的 `MicrosoftEdgeWebview2Setup.exe` 来自 Microsoft 官方下载地址，
  其完整性由 WebView2 自身的签名保证；本项目不修改该文件。
