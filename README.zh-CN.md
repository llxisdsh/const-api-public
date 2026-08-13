<p align="center">
  <img src="docs/images/logo.png" width="88" alt="CONST API Logo">
</p>

<h1 align="center">CONST API</h1>

<p align="center"><strong>所有 AI 工具，一个 API 就够了。</strong></p>

<p align="center">配置一次，模型随时换。</p>

<p align="center">
  <a href="https://github.com/llxisdsh/const-api-public/releases/latest">下载客户端</a> ·
  <a href="docs/usage.zh-CN.md">使用指南</a> ·
  <a href="README.md">English</a> ·
  <a href="https://github.com/llxisdsh/const-api-public/issues">反馈问题</a>
</p>

CONST API 把 AI 工具连接到你选择的模型。工具只需接入一次，之后换 API Key、订阅、本地模型或平台模型，都不用重新配置。支持 Windows、macOS 和 Linux。

## 开始使用

1. [下载](https://github.com/llxisdsh/const-api-public/releases/latest)并打开 CONST API。
2. 看到**本地 API · 运行中**后，点击工具图标。连接配置由 CONST API 自动完成。
3. 登录后可以使用平台模型；也可以在**供应**页加入自己的 API Key、订阅或本地模型。

需要手动填写地址或使用命令行？请看[使用指南](docs/usage.zh-CN.md)。

在**供应**页添加的内容默认只在这台电脑上使用；要不要共享，由你决定。

## 价格怎么算？

- **自己的 API Key、订阅或本地模型：**按服务商价格、订阅规则或本机成本付费，CONST API 不改变这些价格。
- **平台模型：**使用前打开**价格参考**。CONST API 会优先选择价格低、稳定并且有余量的线路。
- 每次请求开始时固定当次价格，再按实际用量结算。没有明确价格，就不发送请求。
- **使用日志**会显示记录到的 Token 和平台费用（如有）。

## 安全和隐私呢？

- AI 工具只连接本机地址 `127.0.0.1:38787`。
- API Key 和订阅凭据留在运行渠道的电脑上，不会写进公开目录。
- 使用自己的渠道时，请求从这台电脑直接发给模型服务商或本地模型，不经过 CONST API 平台。使用平台模型时，请求会经过平台，再发送给实际供应端和模型服务商。
- 日志不保存提示词、回复正文、API Key、Token、Cookie 或鉴权请求头。
- 修改工具配置前会先备份。取消配置时只撤回 CONST API 的修改，你后来改过的内容会保留。

## 它怎么工作？

```text
AI 工具 → 电脑上的 CONST API → 自己的渠道或平台模型 → 模型服务商
```

工具始终连接同一个本地地址。CONST API 找到你已经启用的模型，再按工具需要的 OpenAI、Anthropic 或 Gemini 格式返回结果。

模型、价格、工具支持和平台地址可以独立更新，不必重新安装客户端。所有更新都会先校验再使用。

## 开源

开源发布正在筹备中。
