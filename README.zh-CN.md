<p align="center">
  <img src="docs/images/logo.png" width="88" alt="CONST API Logo">
</p>

<h1 align="center">CONST API</h1>

<p align="center"><strong>用一个本地 API 接入你常用的 AI 工具。</strong></p>

<p align="center">
  <a href="https://github.com/llxisdsh/const-api-public/releases/latest">下载客户端</a> ·
  <a href="docs/usage.zh-CN.md">使用指南</a> ·
  <a href="README.md">English</a> ·
  <a href="https://github.com/llxisdsh/const-api-public/issues">反馈问题</a>
</p>

CONST API 是适用于 Windows、macOS 和 Linux 的桌面程序。它为 ChatGPT、Codex、Claude Code、Gemini CLI、OpenCode、VS Code、WorkBuddy 及其他兼容工具提供稳定的本地 API，再连接到你选择的模型供应：自己的 API Key、受支持的订阅、本地模型或平台共享供应。

## 开始使用

1. 下载并打开[最新版本](https://github.com/llxisdsh/const-api-public/releases/latest)。
2. 看到**本地 API · 运行中**后，点击工具图标完成配置。需要手动接入或使用命令行时，请查看[使用指南](docs/usage.zh-CN.md)。
3. 需要平台共享供应时登录。只有想添加自己的 API Key、订阅或本地模型时，才需要打开**供应**页。

![CONST API 使用页：AI 工具与正在运行的本地 API](docs/images/use.png)

你添加的渠道默认只在本机使用；是否参与平台共享路由，由你决定。

![CONST API 供应页：API、订阅和本地渠道的演示数据](docs/images/supply.png)

<sub>供应页截图使用的是演示数据，不是真实账户或真实凭据。</sub>

## 模型价格

- **自己的 API Key、订阅或本地模型：**费用遵循对应服务商的价格、订阅规则或本机运行成本。
- **平台共享供应：**打开**价格参考**即可查看模型基准价和当前可用报价。路由优先选择价格较低且可用的供应，同时考虑稳定性和容量。
- 请求开始时会锁定价格快照，并按实际用量结算。没有明确价格规则的共享请求会被拒绝，不会猜价扣费。
- 请求完成后可在**使用日志**中查看记录到的 Token 与平台费用（如有）。价格数据可以独立更新，不需要重新安装客户端。

## 安全与隐私

- AI 工具只需连接 `127.0.0.1:38787`；更换供应商或模型时，不必逐个修改工具里的 API 地址。
- 供应商凭据只保留在实际运行渠道的机器上，不会写入公开目录或渠道注册信息。
- 渠道在本机运行时，请求不经过 CONST API 平台，而是从本机直接连接对应的模型服务商或本地运行时；选择平台共享供应时，请求内容会经平台转发给实际供应端和模型服务商。
- 使用日志只保存运行元信息，不保存提示词、回复正文、API Key、Token、Cookie 或鉴权请求头。
- 托管配置会先创建备份。取消配置时只恢复 CONST API 管理的字段，用户之后的修改优先保留。

## 简要原理

```text
AI 工具 → 本地网关 → 本机渠道或平台共享路由 → 模型服务商
```

本地网关提供原生 OpenAI、Anthropic 和 Gemini 接口。工具始终连接稳定的本地地址；存在完全匹配的本机模型时直接使用，否则转向你启用的供应。模型、价格、兼容、工具和端点目录都带签名，并可独立于桌面程序更新。

## 开源

开源发布正在筹备中。
