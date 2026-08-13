# CONST API 使用指南

[返回 README](../README.zh-CN.md) · [English](usage.md)

本文面向公开发布的桌面客户端，说明安装、本地 API、工具配置与恢复、模型渠道、手动 API 调用和命令行运行。不需要访问私有开发仓库。

## 目录

- [安装](#安装)
- [首次运行](#首次运行)
- [使用本地 API](#使用本地-api)
- [配置托管工具](#配置托管工具)
- [Codex 会话连续性](#codex-会话连续性)
- [手动接入](#手动接入)
- [添加模型渠道](#添加模型渠道)
- [高级本地设置](#高级本地设置)
- [命令行运行](#命令行运行)
- [本地文件与隐私](#本地文件与隐私)
- [问题排查](#问题排查)

## 安装

只从 [CONST API 公开 Releases](https://github.com/llxisdsh/const-api-public/releases/latest)下载安装包。

| 平台 | 安装包 | 说明 |
| --- | --- | --- |
| Windows x64 | `const-api_<version>_windows-x86_64-setup.exe` | 默认按当前用户安装 |
| macOS Apple Silicon | `const-api_<version>_macos-aarch64.dmg` | M 系列 Mac |
| macOS Intel | `const-api_<version>_macos-x86_64.dmg` | Intel Mac |
| Linux x64 | `const-api_<version>_linux-x86_64.AppImage` | 如系统要求，先赋予 AppImage 可执行权限 |

> [!WARNING]
> 当前安装包是供可信测试使用的未签名构建：没有 Apple Developer ID/公证、Windows 发布者签名或 Linux 包签名。macOS 使用 ad-hoc bundle 签名，自动更新资产使用 Tauri updater 签名；两者都不能证明操作系统发布者身份。只有确认文件来自本仓库时才继续。

### macOS 首次打开

1. 从 DMG 把 `CONST API.app` 拖到**应用程序**。
2. 先尝试打开一次。
3. 如果 macOS 拦截，打开**系统设置 → 隐私与安全性**。
4. 找到 CONST API 提示，选择**仍要打开**并再次确认。

升级到后续未签名版本时，系统可能要求重新允许一次。

## 首次运行

1. 正常打开 CONST API。
2. 桌面窗口启动的 Runtime 与托盘、CLI 模式使用的是同一套实现。
3. 打开**使用**页，等待状态变为**本地 API · 运行中**。
4. 工具要求 Key 时，复制此页显示的本地 API Key，不要填供应商 Key。

当暂时没有可用生成路由时，客户端仍可进入 degraded 状态。此时进程、本地健康接口、配置界面与诊断仍能工作，但生成请求会明确返回路由错误，直到存在可用的本地或远程渠道。这不等同于本地监听启动失败。

仅在本机使用时不要求登录。账户功能，以及通过模型市场使用或共享渠道时需要登录。

关闭桌面窗口不会停止 Runtime，只会隐藏到 Windows/Linux 托盘或 macOS 菜单栏。要完全停止，请使用托盘/菜单栏里的**退出**。

## 使用本地 API

**使用**页提供三套原生 Base URL 和一个本地 API Key。

| 协议 | Base URL | 常见路径 |
| --- | --- | --- |
| OpenAI | `http://127.0.0.1:38787/v1` | `/models`、`/responses`、`/chat/completions` |
| Anthropic | `http://127.0.0.1:38787/anthropic` | `/v1/models`、`/v1/messages` |
| Gemini | `http://127.0.0.1:38787/gemini` | `/v1beta/models`、`/v1beta/models/{model}:generateContent` |

下拉菜单只改变要复制的地址，不会把三套协议合并成一种。工具支持哪种原生协议，就优先使用哪种。有些 OpenAI 兼容工具要求完整 Chat Completions URL，而不是 Base URL，此时填写：

```text
http://127.0.0.1:38787/v1/chat/completions
```

如果工具要求手填模型列表，应先读取对应接口的 `/models`。托管配置 OpenCode、Claude Desktop、VS Code 与 WorkBuddy 时，客户端会自动完成所需的实时模型投影。

使用页其它入口：

- **高级设置**：本地监听、局域网访问与兼容模型路由策略。
- **接入说明**：可直接复制的各协议参数和手动配置提示。
- **模型市场价格**：当前目录价格与可用的市场报价。
- **使用日志**：经过隐私过滤的请求元信息与路由结果。

## 配置托管工具

当前图标行管理 ChatGPT/Codex、Claude Code、Claude Desktop、Gemini CLI、OpenCode、OpenClaw、Hermes、VS Code 与 WorkBuddy。

### 统一流程

1. 点击工具图标打开菜单。
2. 如果自动发现的程序不正确或不存在，选择**定位程序**，指定真实可执行文件或应用 bundle。
3. 工具确实支持多种协议时，先选择要应用的协议。
4. 选择**配置并重启**。如果工具正在运行，确认 CONST API 发现的具体进程并允许关闭。
5. 等待配置写入、回读和启动确认完成。
6. 以后不再使用 CONST API 时选择**取消配置**：仍由 CONST API 拥有的字段会恢复，用户之后的修改会保留。

**启动**只启动程序，不改配置。工具图标与菜单使用同一个启动器。**配置并重启**是一个完整后端 operation，并不是互不协调的“先写文件，再单独启动”。

关闭确认会绑定到一次性的进程快照。如果对话框打开后进程已经变化，系统会重新确认，而不是关闭后来出现的无关进程。如果自动关闭无法确认，请手动关闭工具后继续；系统不会把无法验证的关闭伪装成成功。

### 管理的文件与协议

| 工具 | 默认管理位置 | 协议 |
| --- | --- | --- |
| ChatGPT / Codex | `~/.codex/auth.json`、`~/.codex/config.toml` | OpenAI Responses |
| Claude Code | `~/.claude/settings.json`、`~/.claude.json` | Anthropic Messages |
| Claude Desktop | 各平台 Claude / Claude-3p 配置库 | Anthropic Messages |
| Gemini CLI | `~/.gemini/.env`、`~/.gemini/settings.json` | Gemini 原生协议 |
| OpenCode | `~/.config/opencode/opencode.json` | Responses 或 Chat，默认 Responses |
| OpenClaw | `~/.openclaw/openclaw.json` | Responses、Chat、Messages 或 Gemini，默认 Responses |
| Hermes | `~/.hermes/config.yaml`；Windows 还检查已配置位置和本地应用目录 | OpenAI Chat |
| VS Code | 各平台 VS Code User 目录中的 `chatLanguageModels.json` 与 `settings.json` | Responses 或 Chat，默认 Responses |
| WorkBuddy | 默认 `~/.workbuddy/models.json` | OpenAI Chat |

Windows 中的 `~` 表示当前用户 Profile 目录；平台特有的应用配置位置由客户端发现。

### 托管写入的保证

- 修改前先解析 JSON、JSON5、YAML 或 TOML；已有文件无法解析时不会覆盖。
- 只修改归属明确的字段和命名 provider 项；无关 provider、MCP、扩展与界面设置保持不变。
- 发生修改的文件会在 `~/.const-api/backups` 留下备份。
- 字段所有权、接管前的值与最后应用值记录在 `~/.const-api/tool-config-manifest.json`。
- 文件经原子替换，并在报告成功前回读验证。
- 重复应用完全相同的配置不会制造无意义写入或备份。

**取消配置**执行字段级三方恢复：

| 当前状态 | 取消时的结果 |
| --- | --- |
| 字段仍等于 CONST API 最后应用值 | 恢复原值；原来不存在则删除 |
| 用户后来修改了托管字段 | 保留用户当前值并解除所有权 |
| 用户修改了无关字段 | 保留无关修改，同时恢复仍由 CONST API 拥有的字段 |
| CONST API 新建了命名 provider 项 | 只删除该逻辑项，保留兄弟项 |

Claude Code 与 Gemini CLI 还有一层运行时处理：由 CONST API 启动时，只向该子进程注入必要网关变量，并移除已知 provider 冲突变量；不会修改系统环境、shell profile 或父进程。

VS Code 只管理内置 BYOK provider 与必要的 utility-model 设置，不会安装、启用、禁用或升级扩展。

WorkBuddy 会写入当前模型列表、上下文/输出上限、视觉/工具/思考能力元信息，以及目录有依据时的思考强度选项；不会为缺少元信息的模型凭空生成选项。

## Codex 会话连续性

Codex 会把 session 与 `model_provider` 关联。只改 `config.toml` 就可能让历史 session 在界面里看似消失，即便 JSONL 文件仍然存在。

CONST API 把配置和 session provider 迁移作为同一个原子工具 operation：

- 配置时，把 `~/.codex/sessions`、`~/.codex/archived_sessions` 和 Codex SQLite thread 索引中的 provider 元数据迁移到 `CONST_API`。
- 取消配置时，先恢复有效 Codex provider，再把所有 CONST session——包括配置期间新建的 session——迁移到恢复后的 provider。
- 不修改会话正文、工具调用、JSONL 的其它行、换行风格和官方 OpenAI 登录缓存。
- JSONL 原子替换后逐字节回读；验证失败会恢复原文。SQLite 状态及 sidecar 会保留有界备份。

使用托管配置时，不要手动把 session 移到另一个 `CODEX_HOME`。CONST API 刻意保留官方 home 和登录缓存，让同一台机器只有一套会话历史。

## 手动接入

使用**使用**页当前显示的 Key。以下命令只使用占位符。

### 获取 OpenAI 兼容模型

```bash
curl http://127.0.0.1:38787/v1/models \
  -H "Authorization: Bearer <local-api-key>"
```

### OpenAI Responses

```bash
curl http://127.0.0.1:38787/v1/responses \
  -H "Authorization: Bearer <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","input":"只回复：连接成功"}'
```

### OpenAI Chat Completions

```bash
curl http://127.0.0.1:38787/v1/chat/completions \
  -H "Authorization: Bearer <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","messages":[{"role":"user","content":"只回复：连接成功"}]}'
```

### Anthropic Messages

```bash
curl http://127.0.0.1:38787/anthropic/v1/messages \
  -H "x-api-key: <local-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","max_tokens":64,"messages":[{"role":"user","content":"只回复：连接成功"}]}'
```

### Gemini 原生接口

```bash
curl "http://127.0.0.1:38787/gemini/v1beta/models/<model-id>:generateContent" \
  -H "x-goog-api-key: <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"contents":[{"role":"user","parts":[{"text":"只回复：连接成功"}]}]}'
```

PowerShell 中如果 `curl` 被映射为其它命令，请使用 `curl.exe`，并按 shell 规则调整引号。不要把真实 Key 写进要提交的脚本、Issue、截图或准备分享的终端记录。

## 添加模型渠道

打开**模型**页，选择**添加渠道**，再选择来源类型。

### API 凭据

来源驱动覆盖 OpenAI、OpenRouter、Azure OpenAI、Anthropic、Gemini、通过 Mantle 兼容接口接入的 Amazon Bedrock、自定义端点，以及本地 OpenAI 兼容运行时。表单只展示该来源鉴权和发现契约需要的字段。

1. 按需填写上游 URL 和凭据。
2. 启用前先运行检测。
3. 检查发现到的接口、真实模型 ID、上下文/输出上限、能力证据和额度信息。
4. 在本机启用渠道。
5. 如果已登录并希望参与远程路由，再显式启用平台供应。

### 订阅适配器

受支持的订阅适配器使用自己的本地 OAuth/导入流程。账户 token 留在客户端并在本机刷新，不会复制到供应注册 payload；如果上游提供真实信息，模型发现与额度窗口也来自真实上游。

### 本地模型

Ollama、LM Studio、vLLM 和兼容本地端点可以作为本地渠道。已启用且模型完全匹配的本地渠道会在平台路由前检查，无需登录或连接服务器即可完成请求。Local-only 渠道不会被宣传成共享供应。

### 状态含义

- **本地可用**：只在本机使用，不共享。
- **平台在线**：渠道已通过当前供应连接注册，并在限额允许时参与候选。
- **冷却/鉴权/额度/风险**：具体安全状态；重试前应先打开渠道详情。
- **可用模型**：通过来源发现与本地验证的模型，不是手工维护的宣传列表。

即使启用多个渠道，每个服务器端点也只维护一条供应 Agent 连接；它优先使用 QUIC，并自动回退到 WebSocket。

## 高级本地设置

安全默认值是仅监听 `127.0.0.1:38787`。

要让可信局域网内的其它设备访问：

1. 打开**高级设置**，显式启用局域网监听。
2. 优先绑定指定私有网卡，确有需要时才使用 `0.0.0.0`。
3. 只为可信子网放行主机防火墙中的对应 TCP 端口。
4. 其它设备使用 `http://<主机局域网IP>:38787/...`，并妥善保护本地 API Key。

同机托管工具仍然使用回环地址。不要把 `38787` 端口直接暴露到公网。本地 API 的 WebSocket 操作与 HTTP 共用同一个监听端口，不需要单独开放端口。

兼容模型路由也是显式高级选项。当请求必须保持精确模型身份时应关闭。启用后，服务器也只能从当前签名兼容组中选择，不会把任意名字相似的模型当作兼容模型。

## 命令行运行

发布包只有一个可执行程序，并且一次最多接受一个公开参数。

| 命令 | 行为 |
| --- | --- |
| `const-api-client` | 桌面模式；打开窗口，关闭后继续驻留托盘/菜单栏 |
| `const-api-client --tray` | 隐藏窗口启动桌面宿主并创建托盘/菜单栏入口；不会 fork 成 daemon |
| `const-api-client --headless` | 使用同一份已保存配置在当前终端运行；无 UI、无托盘；按 Ctrl+C 停止 |
| `const-api-client --check` | 验证已保存配置，安全查询现有 Runtime，打印结果后退出；不启动任何服务 |
| `const-api-client --help` | 打印支持的语法 |
| `const-api-client --version` | 打印客户端版本 |

### Windows PowerShell

默认按用户安装时，路径通常为：

```powershell
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --help
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --check
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --headless
```

如果安装时选择了其它位置，请相应调整路径。

### macOS

要获得真正的前台 headless 进程，应直接执行 app bundle 内的程序：

```bash
/Applications/CONST\ API.app/Contents/MacOS/const-api-client --check
/Applications/CONST\ API.app/Contents/MacOS/const-api-client --headless
```

不要用 `open -a "CONST API" --args --headless` 代替文档中的前台用法；`open` 会脱离终端，Ctrl+C 与终端日志行为也会改变。

### Linux

直接运行解压后的程序或 AppImage，例如：

```bash
chmod +x ./const-api_*_linux-x86_64.AppImage
./const-api_*_linux-x86_64.AppImage --check
./const-api_*_linux-x86_64.AppImage --headless
```

headless 模式不依赖 `DISPLAY` 或 `WAYLAND_DISPLAY`；tray 模式仍然需要图形会话。

### 单 Active Runtime

同一配置目录最多只有一个 Active Runtime：

| 新启动方式 | 已有 desktop/tray | 已有 headless |
| --- | --- | --- |
| 无参数桌面启动 | 唤醒现有窗口后退出 | 报告冲突并以 `2` 退出 |
| `--tray` | 不重复启动，以 `0` 退出 | 报告冲突并以 `2` 退出 |
| `--headless` | 报告冲突并以 `2` 退出 | 报告冲突并以 `2` 退出 |
| `--check` | 安全检查后退出 | 安全检查后退出 |

退出码：

- `0`：成功或正常退出；
- `1`：配置、监听或 Runtime 失败；
- `2`：参数错误或 Active Runtime 模式冲突。

“登录时启动”使用 `--tray`，不是 `--headless`。headless 刻意保持前台运行，也不通过命令行接收任何凭据。

## 本地文件与隐私

| 路径或存储 | 用途 |
| --- | --- |
| `~/.const-api/client.json` | 监听设置、已验证端点缓存投影、渠道配置、账户平台身份与本机设备 API Key |
| 操作系统凭证库（`CONST API account`） | 账户 refresh token；access token 只留在进程内存 |
| `~/.const-api/tool-config-manifest.json` | 安全取消配置所需的字段级所有权 |
| `~/.const-api/backups` | 已修改工具配置与相关状态数据库的有界备份 |
| `~/.const-api/state` | Active Runtime 锁与带认证的回环控制元数据，不保存供应商凭据 |
| `~/.const-api/logs` | 滚动运行日志 |

Renderer 读取配置时会过滤敏感字段。正式版调用日志只包含路由、协议、模型、状态、字节/Token、延迟、成本、错误和 stream 形状等元信息，不包含请求/响应正文、工具参数、Cookie、Authorization header 或供应商 Key。Release 构建不提供原始协议日志。

请求仍然必须到达被选中的供应商或供应端才能执行。这里的隐私保证是配置交换、渠道注册、诊断与持久化中的数据最小化，并不声称模型推理无需把输入发送给被选中的执行端。

## 问题排查

### 首次打开被系统拦截

先确认资产来自本仓库，再按[安装](#安装)里的可信测试流程操作；不要全局关闭系统安全能力。

### 本地 API 没有运行

1. 在第二个终端运行 `const-api-client --check`。
2. 查看 `~/.const-api/logs`；或在没有 desktop/tray Runtime 时用 `--headless` 启动并观察日志。
3. 检查 `38787` 端口冲突、无效监听地址或无法读取的 `client.json`。
4. 没有模型路由与监听失败不同：degraded 状态仍可正常启动本地 API。

### 提示 `invalid API key`

工具里应填写**使用页的本地 API Key**，供应商 Key 应放在模型渠道中。如果本地 Key 已改变，重新应用托管配置，或更新手动配置的客户端。

### 工具没有模型，或要求手填模型列表

访问该工具原生接口的 `/models`，确认目标 ID 确实存在。要求显式模型的托管工具会在配置时读取当前已验证快照；如果当时目录或端点不可用，恢复网络后重新配置。不要从显示名猜测模型 ID。

### 配置并重启无法确认程序已关闭

手动关闭实际工具及其 session 写入辅助进程，再继续。如果发现了错误程序，请取消并使用**定位程序**。有界关闭检查是为了避免工具仍在保存配置或 session 时写入。

### 工具配置文件格式错误

先修复原始 JSON/JSON5/YAML/TOML 语法。CONST API 不会覆盖无法解析的已有文件；手工替换前先查看 `~/.const-api/backups`。

### Codex session 看似消失

不要删除 JSONL，也不要切换到第二个 `CODEX_HOME`。打开 ChatGPT/Codex 工具详情重新应用目标 provider，或用**取消配置**恢复之前的 provider。托管迁移会同时更新 session provider 元数据与 thread 索引。

### 请求没有合格路由

按顺序检查：

1. 是否启用了完全匹配模型的本地渠道；
2. API/订阅渠道是否健康且额度充足；
3. 平台路由需要的账户与公网端点是否可达；
4. 请求是否需要当前供应端未声明的能力；
5. 该兼容组是否确实需要、且已经显式启用兼容路由；
6. 价格上限是否排除了所有候选。

使用**使用日志**、**供应日志**和 route trace 定位具体拒绝请求的层级。

## 当前发布边界

当前公开仓库提供桌面安装包、updater 元数据和签名端点/模型目录。源码许可证与贡献流程仍在准备中。在假设源码可用或拥有再分发权利之前，请先阅读[开源](../README.zh-CN.md#开源)。
