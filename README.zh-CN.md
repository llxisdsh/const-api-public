<p align="center"><img src="docs/images/logo.png" width="88" alt="CONST API"></p>

# CONST API

把 AI 工具、API 渠道、账号订阅和本地模型，连接到同一个本地入口。

[English](README.md) · [下载官方客户端](https://github.com/llxisdsh/const-api-public/releases/latest) · [使用指南](docs/usage.zh-CN.md) · [技术说明](docs/technical-overview.zh-CN.md) · [源码构建](SOURCE.zh-CN.md) · [问题反馈](https://github.com/llxisdsh/const-api-public/issues)

工具配置一次，在一个桌面应用中管理模型渠道。CONST API 提供 OpenAI Responses / Chat
Completions、Anthropic Messages 和 Gemini API 接口，支持 Windows、macOS 和 Linux。

## 先选择适合你的版本

这个仓库同时提供**本机版源码**和**官方客户端的下载**，两者不是同一个版本：

| | CONST API Local：从本仓库编译 | 官方客户端：从 Releases 下载 |
| --- | --- | --- |
| 自己的 API、受支持的订阅、本地模型 | 支持 | 支持 |
| 工具自动配置、本机和局域网使用 | 支持 | 支持 |
| CONST 账号、模型市场、远程供应和账单 | 不包含 | 登录 CONST 账号后可用 |
| 默认本地 API 端口 | `38789` | `38787` |
| 配置目录 | `~/.const-api-local` | `~/.const-api` |
| 更新方式 | 获取新源码后重新编译 | 官方更新渠道 |

公开仓库的 CI 只检查本机版源码，**不发布安装包**。官方安装包仍由私有主仓库构建和发布。
公开源码不能复现包含私有平台模块的官方客户端。

## 本机版能做什么

- **统一管理渠道。** 添加 API Key、受支持的账号订阅，或兼容 OpenAI 的本地服务。
  实际可用模型和能力取决于上游。
- **连接常用工具。** 在应用中配置 Codex、Claude Code、OpenCode、WorkBuddy 等支持的工具；
  也可复制本地地址和 Key 手动接入。托管配置会备份，撤销时保留无关的用户修改。
- **使用原生协议。** 同协议请求在上游允许的范围内保留原生字段；跨协议转换处理消息、
  流式响应、工具调用和用量，但不会让模型获得原本不具备的能力。
- **在可信局域网共享。** 选择共享模型，为成员分别生成 Key 并设置用量限制。
  局域网共享不等于加入平台模型市场。
- **排查请求。** 查看渠道健康、可用模型、用量、延迟和错误，确认实际使用了哪个上游。

本机使用不需要 CONST 账号。上游服务商仍可能收取费用。**刷新渠道资料不等于生成测试**；
当前模型测试和完整检查可能消耗上游额度。

Antigravity 的浏览器授权和令牌刷新，需要构建者提供兼容的 OAuth 应用配置；
这些应用凭据不随源码公开。见[源码构建要求](SOURCE.zh-CN.md#可选的-antigravity-oauth-配置)。

## 技术实现概览

网关核心使用 **Rust**，桌面界面使用 **Tauri + React/TypeScript**。
工具共用一条本机请求链路；协议和厂商差异由专门的适配器处理，不在每个工具里重复实现。

| 技术特征 | 实现方式 |
| --- | --- |
| 原生转发与跨协议转换 | 同协议保留原生字段；跨协议使用统一消息和工具结构，并按事件转换流式响应。 |
| HTTP、SSE、WebSocket 与媒体 | 复用 HTTP 客户端，按上游能力管理 Responses WS 连接，上传和实时连接使用专用路径。 |
| API 与账号订阅渠道 | 共用渠道执行器，将厂商鉴权、凭据刷新和必要的订阅字段调整留在对应适配器。 |
| 一致的模型列表 | 短名称与上游 ID 分开；工具和 LAN 接入共用排序、可用性、上下文及兼容信息。 |
| 可恢复的工具配置 | 跟踪字段归属、备份修改，取消时保留用户后续编辑；Codex 切换接入时同步会话索引。 |
| 有界的运行开销 | 增量流式处理、按字节限量的输出缓存、精简模型目录、元数据缓存及日志与历史保留上限。 |

[完整技术说明](docs/technical-overview.zh-CN.md)通过 14 个章节解释这些机制及更多特性，并附代码入口。
其中也单独介绍**官方平台专属**的目录发布和动态参考价格，不会将它们混同为公开本机版服务。
[English technical guide](docs/technical-overview.md)。

## 快速开始

### 使用官方客户端

1. 从 [Releases](https://github.com/llxisdsh/const-api-public/releases/latest) 下载对应系统的安装包。
2. 在**模型**页添加自己的渠道。本机使用可不登录；平台模型市场需要账号。
3. 等待**本地 API · 运行中**，在**使用**页配置工具。

### 从源码运行 CONST API Local

安装与 `client/.node-version` 一致的 Node.js、稳定版 Rust，以及对应系统的
[Tauri 2 构建依赖](https://v2.tauri.app/start/prerequisites/)。

```sh
git clone https://github.com/llxisdsh/const-api-public.git
cd const-api-public/client
npm ci
npm run dev
```

添加并启用自己的渠道，再配置工具即可。不需要部署服务器，也不需要私有仓库。
生成可执行程序使用 `npm run build`。检查命令、输出位置和版本边界见
[源码构建说明](SOURCE.zh-CN.md)。

两个版本都是桌面应用，不提供 headless 或配置检查命令行模式；旧文档中的相关命令已失效。

## 请求会经过哪里

**本机版：** AI 工具 → 本机 CONST 网关 → 你配置的上游。
内置 CONST 平台登录、端点发现、供应连接和平台请求传输不可用；复制官方版账号配置也不能开启。

**官方客户端：** 根据路由设置，使用自己的渠道或平台模型市场。市场请求经过平台、选中的
供应端，再到上游服务商。渠道凭据留在供应端电脑，但请求内容必然会到达实际处理它的系统。

本机版不是离线模型，也不是网络安全沙箱：它会连接你配置的上游 URL。
源码限制不能证明任意修改版客户端的身份；平台服务的权限仍由服务器执行。

## 项目结构

| 路径 | 内容 |
| --- | --- |
| `client/src/` | React / TypeScript 桌面界面 |
| `client/src-tauri/` | Rust 运行时、本地网关、渠道适配和工具配置 |
| `shared/` | 公共 API 契约与内置元数据 |
| `testdata/` | 协议测试数据 |
| `.github/workflows/check-source.yml` | 构建与隔离检查，不执行发布 |

源码快照由主仓库同步。欢迎范围明确的改进；提交时请附复现方式和相关测试。
维护者会先把通过审查的改动合入主仓库，再导出下一份公开快照。

## 帮助和许可

- [使用、手动接入和问题排查](docs/usage.zh-CN.md)
- [技术架构、特性与代码导航](docs/technical-overview.zh-CN.md)
- [源码构建和隔离说明](SOURCE.zh-CN.md)
- [报告问题](https://github.com/llxisdsh/const-api-public/issues)：请注明版本类型、版本号、系统和
  脱敏错误。不要上传 Key、Token、账号文件或私人对话内容。

导出的自有源码采用 [MIT 许可](LICENSE)。第三方依赖和资源保留各自许可，见
[第三方声明](THIRD_PARTY_NOTICES.md)。源码许可不包含平台服务访问权、上游订阅权益或第三方商标权。

社区交流：[LINUX DO](https://linux.do/)。
