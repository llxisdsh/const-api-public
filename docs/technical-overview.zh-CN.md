# 技术架构与特性

[English](technical-overview.md) · [返回 README](../README.zh-CN.md) · [使用指南](usage.zh-CN.md) · [源码构建](../SOURCE.zh-CN.md)

CONST API 是一个桌面 AI 网关：工具连接本地入口，网关选择渠道，把请求交给上游，再按工具理解的格式返回结果。它不运行大模型，也不代替 Codex、Claude Code 等工具执行代码。

本文面向想了解实现原理、评估接入或参与开发的读者。内容按本仓库的源码整理，每章附有代码入口。**没有特别标注时，介绍的是公开的 CONST API Local。** 官方平台专属的目录发布、动态价格、远程供应等会单独说明；这些服务端实现不包含在本仓库中。

## 目录

1. [整体架构：Rust 核心与桌面界面](#architecture)
2. [API 接口：三个入口，四种生成协议](#api-surfaces)
3. [协议转换：能直通就直通，需要转换时解释差异](#conversion)
4. [工具调用与多轮状态](#tools-and-state)
5. [HTTP、SSE 与 WebSocket 长连接](#transports)
6. [图片、音视频、上传与资源接口](#media)
7. [渠道、订阅账号与凭据](#channels)
8. [模型名称、排序、上下文与兼容列表](#models)
9. [模型目录与价格更新](#catalog-and-pricing)
10. [检查、路由、额度与用量](#routing-and-usage)
11. [工具配置、启动与恢复](#tool-configuration)
12. [局域网共享](#lan)
13. [运行时、性能与诊断](#runtime)
14. [扩展方式、验证与能力边界](#extension)

<a id="architecture"></a>
## 1. 整体架构：Rust 核心与桌面界面

### Rust 负责真正的请求处理

本地 HTTP 服务、上游连接、协议转换、订阅适配、流式处理和工具配置都在 Rust 运行时中。Tokio 调度网络任务，Warp 提供本地接口，Reqwest 和 WebSocket 客户端连接上游，Serde 处理结构化数据。

这些工作不依赖界面中的 JavaScript 来转发，也不需要为每一种渠道再启动一个 Python 或 Node.js 代理服务。

### Tauri 连接界面与运行时

界面使用 React / TypeScript，通过 Tauri 命令读取状态和执行配置操作。桌面窗口使用系统 WebView。因此，准确说法是 **Rust 网关核心 + Tauri 桌面应用**，不是整个项目及依赖“纯 Rust”。

### 一条共用的本机调用链

```text
Codex / Claude / 其他工具 / LAN 成员
                  │
          本地 API：鉴权、识别操作
                  │
          模型解析、渠道选择、额度检查
                  │
        ┌─────────┴──────────┐
    同协议直接转发      跨协议转换为目标格式
        └─────────┬──────────┘
             渠道执行器
        ┌─────────┴──────────┐
     通用 HTTP 渠道       订阅专用适配器
        └─────────┬──────────┘
                 上游
                  │
       原生返回 / 转换返回，同时观察用量与错误
```

渠道执行器是统一入口。厂商差异留在对应适配器里，不要求每个工具分别实现一套 OpenAI、Claude 或 Gemini 接入。

代码：[运行时](../client/src-tauri/src/runtime.rs)、[渠道执行器](../client/src-tauri/src/channel_executor.rs)、[依赖与构建特性](../client/src-tauri/Cargo.toml)。

<a id="api-surfaces"></a>
## 2. API 接口：三个入口，四种生成协议

### 工具按自己熟悉的协议接入

本机版默认地址为 `http://127.0.0.1:38789`。同一个监听端口提供三套 API 路径；OpenAI 入口同时支持两种生成协议。

| API 入口 | 生成接口示例 | 生成协议 |
| --- | --- | --- |
| `/v1` | `/v1/responses` | OpenAI Responses |
| `/v1` | `/v1/chat/completions` | OpenAI Chat Completions |
| `/anthropic` | `/anthropic/v1/messages` | Anthropic Messages |
| `/gemini` | `/gemini/v1beta/models/{model}:generateContent` | Gemini 原生协议 |

这避免要求所有工具改用 Chat Completions。支持 Responses 的工具可以继续使用 Responses；Gemini 工具也可以保留原生路径和流式操作。

### 接口不只包含聊天

操作表还识别模型查询、token 计数、Responses 压缩、文件、分片上传、图片、音频、视频、embedding、批处理、会话资源、Gemini 缓存和实时连接等接口。

**识别一个接口不等于任意渠道都能执行它。** 非聊天操作通常需要对应的原生上游，不能把一个只会聊天的渠道变成图片或文件服务。

### 路径、请求头和二进制内容分别处理

请求封装区分路径与查询参数，支持重复请求头和二进制内容，不把所有请求强行解析成聊天 JSON。错误会按调用方所用 API 的格式返回，方便原有工具识别。

`/.well-known/const-api` 提供网关接口发现信息；具体路径和操作定义集中在共用契约中。

代码：[API 契约](../shared/api-surface-contract.json)、[操作识别](../client/src-tauri/src/surface.rs)、[请求封装](../client/src-tauri/src/surface_wire.rs)。

<a id="conversion"></a>
## 3. 协议转换：能直通就直通，需要转换时解释差异

### 同协议优先保留原生内容

当入口和目标协议相同，且不需要解析 CONST 的跨协议续接状态时，请求走直通分支，不先拆成通用消息再重新拼装。只需替换模型时，使用局部改写，保留其余未知字段以及原始 JSON 格式。

这里的“保留”不是所有字节绝对不变：上游鉴权、必要的传输头处理、模型 ID 替换、订阅接口要求的字段调整仍然存在。无签名的 Chat 隐藏推理历史也有专门的过滤规则。订阅差异由订阅适配器负责，不能推广成所有渠道的清洗规则。

### 跨协议通过统一中间表示转换

Responses、Chat Completions、Messages、Gemini 各有自己的解码器和编码器。跨协议请求先转成统一结构，再编码为目标格式。

中间结构不只有 `role` 和 `content`，还描述：

- 系统指令、消息轮次、文本与拒绝信息。
- 推理内容、工具定义、工具调用和工具结果。
- 图片、音频、视频、文件引用。
- 缓存提示、用量、结束原因及厂商扩展。

这样，新增一类通用内容时可以在中间层统一处理，不必为每一对协议都写独立转换器。

### 转换会记录能力差异

转换计划区分 `native`、`lossless`、`compatible`、`lossy`、`blocked`，并记录涉及的字段、调整动作和原因。这些等级用于描述转换，不是“所有有损转换都会执行”的承诺。

能合理表达的内容尽量保留；关键能力无法表达时，返回明确错误。目标模型不支持工具、某种媒体或厂商专属状态时，格式转换不能凭空补出能力。

### 流式转换按事件进行

SSE 解码器先处理任意网络分片，再转换成统一的开始、内容增量、工具增量、用量、结束和错误事件，最后编码成调用方协议。它不会把“一次收到的 TCP 数据”误当成“一条完整 SSE 消息”，也不要求先等完整回复再返回。

代码：[转换入口](../client/src-tauri/src/protocol/mod.rs)、[中间结构](../client/src-tauri/src/protocol/ir/)、[转换计划与报告](../client/src-tauri/src/protocol/conversion/)、[流式处理](../client/src-tauri/src/protocol/stream/)。

<a id="tools-and-state"></a>
## 4. 工具调用与多轮状态

### 保留调用与结果之间的对应关系

工具调用不仅是一个函数名，还包含调用 ID、参数、命名空间及结果引用。转换层维护这些关系，处理函数参数的流式增量，并校验工具历史结构，减少“结果找不到对应调用”的问题。

CONST 只传递工具定义、调用和结果。读文件、执行命令、修改代码仍由 Codex 等实际工具完成。

### 兼容 Responses 的附加工具与自由文本工具

- 除顶层 `tools` 外，也读取 `input[]` 中 `type: "additional_tools"` 条目内的 `tools`。原生路径保留原位置，跨协议时合并到目标协议的工具声明。
- 目标协议没有 namespace 时，生成可区分的函数名称，再按请求内映射还原返回的工具身份。
- 自由文本工具可包装成带 `input` 字符串的 JSON 函数，并在返回时解包。grammar 会保留为格式说明，但不等于目标模型具备原生的语法约束采样。

这些映射只属于当前请求，不是把所有用户的工具名称塞进一个共享映射表。

### 厂商状态不能随意跨账号、跨后端搬运

推理签名、加密内容、文件 ID 和续接状态可能只对生成它们的上游有效。转换层记录这类内容的来源与回放约束；不把私有状态当普通文本随意发给另一个后端。

普通可见消息和工具历史，与这些不透明的厂商状态分开处理。CONST 的续接封装也有专门解析路径，不依赖工具界面显示隐藏字段。

### 压缩能力有独立边界

Responses compact 与 Claude 服务端压缩不是同一个操作。Claude compaction 的压缩块、结束原因和用量有专门处理，并要求原生 Anthropic 目标；不会将其静默转成普通 Chat 请求。是否可用还取决于模型和上游能力。

代码：[工具桥接](../client/src-tauri/src/protocol/conversion/tool_bridge.rs)、[Responses 适配器](../client/src-tauri/src/protocol/adapters/openai_responses.rs)、[状态回放策略](../client/src-tauri/src/protocol/conversion/artifact_policy.rs)、[续接封装](../client/src-tauri/src/protocol/continuation.rs)、[请求转换](../client/src-tauri/src/proxy/request_conversion.rs)。

<a id="transports"></a>
## 5. HTTP、SSE 与 WebSocket 长连接

### HTTP 连接与流式响应

运行时复用 HTTP 客户端和连接池，避免每次生成都重新创建整套网络客户端。普通 JSON、SSE 和二进制响应按各自方式转发；下游断开时，相应任务和连接租约会释放。

### HTTP/3 是按上游条件启用的优化

通用上游传输层可以按目标站点探测 HTTP/3，并缓存结果与失败冷却。系统代理命中、请求不可安全复制等情况继续使用普通 HTTP；原生订阅端点保留其正常传输方式，不强行增加 HTTP/3 探测。

切换传输不是任意重试的理由。对于可能已经执行的生成请求，不会仅因一次传输错误就自动重发，避免重复消耗额度。

### Responses WebSocket 有分层复用

连接池按渠道、凭据引用、端点、握手参数和协议模式分组。不同身份或不兼容握手不会混用一条连接。

- 同一个会话可以连续完成多轮调用，并保持续接所需的连接关联。
- 支持 `stream_id` 多路复用的上游，可以在一条物理连接上区分多个逻辑流。
- OpenAI 公共 API 与订阅端点的模式分开选择。订阅多路复用受单独探测开关控制，不能假设所有账号都支持。
- 未建立多路复用能力时使用独占模式；独占连接有空闲回收，老连接有退出复用和关闭机制。

因此，“支持 WebSocket”不等于“所有 session 都共享一条 WebSocket”，也不等于 HTTP 请求一定被改成 WebSocket。

### 接收队列按字节限量

响应缓存按需分配，使用字节预算和背压：消费者慢时，上游读取也会减速，而不是无限堆积消息。当前通用输出预算为 32 MiB，按 HTTP 响应或物理 WS 连接共享，不是每个 token 都预分配一块大缓存，也不是整个进程的内存上限。

已解码的大帧仍受传输层大小限制；终止错误有独立的结束通道，避免队列满了反而看不到失败原因。

### 平台隧道不是本机上游连接

官方版还使用 WebSocket / QUIC 连接平台与供应端。这是另一段网络链路。公开本机版的这些平台连接入口不可用，但自己的上游 HTTP、SSE、WebSocket 接入仍然保留。

代码：[上游传输选择](../client/src-tauri/src/upstream_transport.rs)、[Responses WS 连接池](../client/src-tauri/src/proxy/responses_ws_pool.rs)、[WS 转发](../client/src-tauri/src/proxy/websocket.rs)、[输出背压](../client/src-tauri/src/output_buffer.rs)。

<a id="media"></a>
## 6. 图片、音视频、上传与资源接口

### 媒体输入不只是一段 URL

统一内容结构区分内联 Base64、远程 URL 和上游文件 ID，并保留媒体类型及图片清晰度等信息。是否能跨协议使用，取决于目标协议和模型对该输入的支持。

### 原生媒体接口保留自身语义

图片生成与编辑、语音合成与转录、视频、embedding 等有独立操作识别。原生 API 渠道可以转发相应接口和返回的二进制内容；订阅渠道只允许适配器明确支持的操作，不假定订阅具备同厂商全部 API 能力。

### 大文件不必全部读入内存

文件和 multipart 上传有专用流式路径，按接口限制体积。Gemini 的续传上传还维护上传会话映射，后续分片继续发往对应上游。

### 后续资源请求回到创建它的渠道

文件、Responses、会话等资源 ID 会记录所属渠道。后续读取、取消或删除优先根据归属定位，而不是重新随机挑一个账号。

归属表使用内存查询，并通过本地 SQLite 保存，支持重启后继续定位；条目数量受限。无法确认归属时不能假装其他渠道一定能访问该资源。

### 实时连接与普通 SSE 分开处理

实时语音连接可涉及 WebSocket 或 SDP/WebRTC 建连，不是把音频塞进普通聊天文本。代码中有 OpenAI Realtime 的本地建连适配；具体入口仍受渠道契约限制。

官方平台另外有 Opus 与数据通道帧的媒体转发实现：两端分别处理 WebRTC，隧道传输已编码帧，不负责录音、转码或重播历史音频。**这段平台媒体隧道不属于公开本机版可用服务。**

代码：[媒体内容结构](../client/src-tauri/src/protocol/ir/content.rs)、[流式上传](../client/src-tauri/src/proxy/streaming_upload.rs)、[Gemini 上传会话](../client/src-tauri/src/proxy/gemini_upload_session.rs)、[资源归属](../client/src-tauri/src/proxy/resource_owner.rs)、[本地 Realtime](../client/src-tauri/src/proxy/local_openai_realtime.rs)。

<a id="channels"></a>
## 7. 渠道、订阅账号与凭据

### 渠道类型决定执行规则

驱动描述文件集中定义认证方式、模型发现、API 入口和执行适配器。常见 API 渠道与自定义渠道复用 HTTP 执行器；只有需要特别处理的订阅或聚合服务才进入专用适配。

| 类别 | 已有驱动示例 | 主要差异 |
| --- | --- | --- |
| 账号订阅 | OpenAI/Codex、Claude、Antigravity、Grok | 账号凭据、专用端点、刷新与额度 |
| 厂商 API | OpenAI、Anthropic、Gemini、xAI、Mistral、DeepSeek、通义、Moonshot、智谱、MiniMax、阶跃 | API 认证和原生协议 |
| 云与推理平台 | Azure OpenAI、Bedrock Mantle、Groq、Together、Fireworks、Hugging Face、NVIDIA、SiliconFlow、火山、百度、腾讯 | 地址格式、认证或模型目录 |
| 聚合与编码计划 | OpenRouter、OpenCode Go/Zen、Kilo、Cline、Command Code、Kimi Code、GLM Coding Plan、MiniMax Token Plan、Ollama Cloud、OmniRoute | 固定服务契约、目录及模型级入口 |
| 本地与通用入口 | Ollama、LM Studio、vLLM、自定义端点、LAN 共享 | 本地服务或兼容 API |

这是驱动范围，不表示每一项都拥有独立 OAuth 流程，也不表示已对每种付费套餐完成真实账号测试。有些“订阅计划”仍使用服务商提供的 API Key。

### 订阅适配器负责不可避免的差异

同一厂商的订阅入口和标准 API 可能使用不同鉴权、客户端身份、请求字段和模型清单。专用适配器负责这些差异，通用协议层不需要知道每个套餐的细节。

支持的授权方式包括浏览器 OAuth、回调接收及凭据导入，具体由渠道决定。浏览器回调无法到达本机端口时，支持的流程可通过手动粘贴回调地址完成，不要求关闭其他正在使用该端口的程序。

### 凭据刷新有同步和持久化

刷新按凭据路径加锁；部分刷新令牌还按身份合并并发操作，减少同时刷新导致令牌轮换冲突。成功后原子更新凭据文件，再供后续调用读取。

网络失败与明确的授权失效分别处理。旧账号文件的兼容读取，不等于忽略上游已经撤销的令牌。

Antigravity 是公开构建的一个特殊要求：浏览器授权和刷新需要构建者提供兼容 OAuth 应用配置，详见 [SOURCE](../SOURCE.zh-CN.md#可选的-antigravity-oauth-配置)。

### User-Agent 覆盖只放在需要它的边界

通用 HTTP/API 渠道提供可选的 Claude Code、Codex、OpenCode 身份配置，用于上游要求特定客户端标识的情况。留空不主动覆盖调用方身份；部分固定网关有自己的默认值。账号订阅继续由对应适配器管理身份。

更换 User-Agent 不会创造套餐权限，也不能保证上游接受请求。

### OpenRouter 使用账号范围目录

OpenRouter 的发现走账号模型目录并请求全部模态，解析分页、上下文、参数和端点能力。文本检查不会误选语音或图片输出模型。

授权失败或账号明确返回空目录时，不偷偷用公共全量目录冒充成功。目录中的模型仍需经过本地可用性判断；“列得出来”不等于当前地区或账号一定能调用。

代码：[驱动目录](../client/source-drivers.json)、[执行器](../client/src-tauri/src/channel_executor.rs)、[凭据与检测](../client/src-tauri/src/detection.rs)、[订阅凭据导入](../client/src-tauri/src/detection/subscription_credentials.rs)、[User-Agent](../client/src-tauri/src/channel_user_agent.rs)、[OpenRouter 目录](../client/src-tauri/src/openrouter/catalog.rs)。

<a id="models"></a>
## 8. 模型名称、排序、上下文与兼容列表

### 展示短名称，调用原始 ID

模型身份分开保存，不靠一个字符串包办所有用途：

| 用途 | 例子 | 规则 |
| --- | --- | --- |
| 展示和工具选择 | `model-a` | 路径尾部、小写 |
| 上游调用 | `vendor/model-a` | 保留渠道实际提供的 ID |
| 渠道路由 | 某个渠道中的 `vendor/model-a` | 不因显示同名就合并账号 |
| 平台计价 | 目录中的规范模型及计价规则 | 通过明确身份或别名匹配，不改上游 ID |

归一化在模型处理层完成，供模型接口、工具注入和 LAN 列表共用，不是只修改某一个下拉框。

短名称冲突时，一个渠道的列表保留先出现的项，不随机切换。不同渠道仍可提供同一公开名称；短名展示不承诺暴露所有同名厂商变体。动态价格目录另有确定性的来源合并规则。

### 排序是共用展示策略

模型展示策略按有序名称/前缀规则分组，组内按模型 ID 排序，也支持隐藏。工具目录和 LAN 列表复用这套优先级；官方平台可下发展示规则，公开版使用随源码提供的规则。

隐藏只影响列表呈现，不等于撤销模型调用权限。第三方工具如果自行重新排序，最终界面仍由工具决定。

### 上下文容量优先采用渠道观测值

模型目录可以提供上下文窗口、输出上限、输入输出模态、支持参数和协议端点。工具元数据优先使用有效的渠道观测值，内置目录作为补充，而不是只按模型名字猜。

同一模型可能落到多条兼容路由时，容量合并取保守值；无法确认时，不因为其中一条路由容量大就宣称全部可用路由都支持大窗口。

### `[1m]` 是上下文提示，不是一套重复模型

普通模型列表不为 `[1m]` 再复制一份条目。支持这一约定的工具可以在有能力依据时获得对应提示或配置，转发时仍需保留实际调用所需的信息。

字面上游 ID 与工具上下文别名分别解析。加后缀不代表所有 Claude 模型都会变成 1M，也不会绕过上游上下文上限。

### 兼容模型替代需要明确开启

兼容组定义哪些模型可以作为替代候选。只有启用替代后，路由才扩大候选范围；不会把显示名称相似当作自动等价。模型目录、兼容组和能力数据分别管理，便于单独更新。

代码：[名称处理](../client/src-tauri/src/config.rs)、[目录元数据](../client/src-tauri/src/model_catalog.rs)、[展示排序](../client/src-tauri/src/model_discovery.rs)、[工具模型信息](../client/src-tauri/src/tool_model_metadata.rs)、[兼容模型](../client/src-tauri/src/model_compatibility.rs)、[内置排序规则](../shared/tool_model_presentation_defaults.json)。

<a id="catalog-and-pricing"></a>
## 9. 模型目录与价格更新

### 三种信息不要混在一起

- **账号发现目录：** 这个渠道上游当前公布了什么模型。
- **产品模型目录：** 名称、能力、上下文、兼容分组等公共元数据。
- **参考价格目录：** 用于平台报价和计费的价格规则，不是调用权限证明。

公开版会查询自己的上游，并携带内置的工具模型元数据；不包含官方市场价格页或平台结算服务。它也不会通过复制官方配置来启用平台目录自动更新。更新内置元数据的正常方式是获取新的源码快照并重新构建。

### 官方平台：应用版本与目录版本分开发布

官方系统的模型和价格目录可以独立发布，不必每加一个模型就升级客户端程序。目录带版本、摘要和签名校验；运行时使用已通过校验的快照。公开源码保留了部分共用目录结构与校验代码，但官方目录刷新任务在 Local 版中关闭。

### 官方平台：公开价格 API 作为动态来源

已实现的价格源包括 OpenRouter、Kilo、Cline。服务器统一读取这些可信来源的公开目录，转换为现有价格规则，再与发布基线合并；没有价格的模型列表不能充当价格源，自定义供应者也不能直接决定全平台参考价。

正常成功刷新约每 6 小时一次，失败来源独立延后重试。支持条件请求、并发请求合并和每来源的最新成功缓存；模型请求、页面翻页和心跳不会触发查价。失败保留上次有效价格，没有动态数据时使用发布基线。

### 官方平台：统一单位，保留计费维度

输入、输出、缓存读写、长上下文阶梯等分别处理。缺失价格不是零价格；无法明确单位的新媒体价格不会仅凭相似字段自动加入。`:free` 遵循明确的零价规则，`[1m]` 本身不固定加价，长上下文按实际价格规则计算。

同一模型存在多个可信报价时，按计费维度采取较高参考值，原有倍率继续生效。这是统一参考价，不是供应者的实际采购价，也不是利润保证。

### 官方平台：请求开始时固定价格

模型准入和价格使用同一份运行时快照。已开始的请求保留其价格和阶梯规则，后续动态更新只影响新请求，不会让长流式请求执行到一半突然换价，也不重算历史账单。

公开代码入口：[内置工具目录](../client/src-tauri/resources/tool-model-metadata.json)、[目录校验组件](../client/src-tauri/src/catalog_registry.rs)、[Local 版本边界](../client/src-tauri/src/local_policy.rs)。本节标注“官方平台”的动态价格与结算实现位于非公开服务端，不能从本仓库独立启动。

<a id="routing-and-usage"></a>
## 10. 检查、路由、额度与用量

### 刷新不等于付费测试

刷新渠道信息读取模型、额度和协议元数据，不发送模型生成请求。当前模型测试与完整检查才会实际验证调用，可能消耗上游 token。

检测结果区分目录声明、已经验证和未知能力。原生 Responses 接口存在，不自动等于支持 Codex 的全部工具或扩展能力。

### 分组检查减少全量扫描

完整检查按 Anthropic、Google、OpenAI 和其他模型分组，每组选择一个代表，并在渠道并发限制内并行执行。失败后的尝试有预算，包括适配器内部重试，不会无限追加生成请求。

明确的地区、条款或账号限制可影响对应分组；模型自身问题、网络抖动和检查未执行则分别处理。分组结论不是组内每一个模型都已逐个实测的证明。

后台恢复由失败状态、到期时间和实际需求驱动，并限制尝试次数；已经确认的硬限制不进入高频付费轮询。旧渠道一次检查无结论，不应被当成所有模型都不可用。

### 模型发现失败不会一律清空可用列表

短暂网络故障、限流或上游服务故障，可以保留此前成功确认的目录。明确鉴权失败、解析错误与有效的空账号目录会被区分并显示，不能全部伪装成刷新成功。

### 路由同时考虑身份、健康和能力

路由解析请求模型、当前渠道模型、可用性、额度及可选兼容候选。运行时按模型观察失败与恢复，避免一个模型额度不足就把不相关模型全部判死；确实影响整个渠道的授权错误则按渠道处理。

公开本机版只选择自己的渠道，不会自动跑到官方付费市场。官方版的“本机优先”等平台回退策略属于另一层选择。

### 错误根据结构识别，不扫描模型回答

错误观察器读取 HTTP 状态、协议错误对象、流式错误事件和 `Retry-After` 等证据，区分授权、额度、容量、限流、模型不支持和网络问题。不会因为模型在正常回答中提到 “rate limit” 就判断请求失败。

错误保留作用范围、是否适合重试、等待时间和原始原因摘要。传输失败不等于确认上游没有执行；换渠道或恢复连接仍要遵守各调用路径的重试条件。

### 用量与缓存统计保持各厂商的口径

- 输入、输出、缓存读取、缓存写入分别记录；Claude 的 5 分钟和 1 小时写入可分别保留。
- Anthropic 输入统计会考虑其顶层 input 不包含缓存 token 的口径；compaction 的迭代用量单独累计，后续 SSE 增量不会把已记录的压缩用量覆盖掉。
- “上游没提供缓存 token”与“明确返回零”不同。回复变快不能单独证明缓存命中。
- 保留可用的缓存身份和缓存提示，有助于上游复用前缀；CONST 不把不同用户的模型答案做成共享结果缓存，也不保证固定命中率。

### 额度窗口与请求限制分开

渠道保留有来源和周期的额度窗口。主要账号窗口可以限制供应，辅助功能桶不自动变成全渠道限制；Antigravity 等按模型计额度的来源单独处理。配置的并发、请求频率、每日限制与用量保留也有独立检查。

本机转发不会因为绕过平台就绕过渠道自身限制。界面显示的额度、实际请求用量和平台价格也是不同数据，不互相冒充。

代码：[分组检查](../client/src-tauri/src/supplier/availability.rs)、[本机路由](../client/src-tauri/src/proxy/runtime.rs)、[模型结果观察](../client/src-tauri/src/proxy/model_health.rs)、[错误分类](../client/src-tauri/src/upstream_failure.rs)、[上游用量](../client/src-tauri/src/supplier/upstream_usage.rs)、[额度窗口](../client/src-tauri/src/supplier/quota.rs)、[订阅限制](../client/src-tauri/src/supplier/safety.rs)。

<a id="tool-configuration"></a>
## 11. 工具配置、启动与恢复

### 工具描述与配置写入分离

工具目录定义可用协议、模型同步方式、程序位置与启动方式；对应写入器处理 JSON、JSON5、TOML、YAML、环境文件或应用数据库。增加工具不需要复制整套渠道执行逻辑。

当前目录包括 Codex、Claude Code、Claude Desktop、Claude Science、Gemini CLI、GitHub Copilot CLI、Cline CLI、OpenCode、OpenClaw、Goose、Raven、DeepSeek Reasonix、DeepSeek Harness、Pi、Hermes、VS Code、WorkBuddy、Kimi Code、MiMo Code、Qwen Code、Open Science、Open Interpreter、AnythingLLM、Mistral Vibe、Open Design、Vibe-Trading 和 ZCode。具体操作取决于工具版本及其开放的配置能力，不是统一修改所有 IDE 扩展。

### 只接管自己负责的字段

配置管理记录字段的原值、最后写入值和所属工具。内容已经一致时不重复改写；需要修改时备份，并使用原子写入与操作锁协调。

取消配置采用三方比较：当前值仍等于 CONST 写入值，就恢复原值；用户后来改过，就保留用户的新值。命名 provider 或带管理标记的条目按自身归属清理，无关 provider、MCP 和其他配置继续保留。

这比把整个旧配置文件覆盖回去更适合长期使用，也用于避免正式版与 dev 切换后残留重复模型。

### 模型注入不只写名字

工具支持时，会写入上下文、输出上限、输入模态、工具调用、推理能力和展示优先级。只接受选定模型的工具不强塞完整目录；支持完整目录的工具使用归一化后的可用列表。

能力和容量来自共用模型元数据，不为每个工具维护一份互相矛盾的模型表。外部工具是否显示自定义名称、是否重新排序，仍由其自身配置格式决定。

### Codex：配置切换与历史可见性一起处理

Codex 配置维护专用 provider，而不是创建另一个 `CODEX_HOME` 把历史隔开。切入或取消 CONST 时，同步会话的 provider 归属，让当前接入方式能看到相关历史。

新版会话优先修改当前 SQLite 索引中的必要字段，不重写已索引的对话 JSONL；旧式会话只调整开头的 provider 元数据，保留正文。取消时同步到恢复后实际生效的 provider，包括 CONST 使用期间新建的会话，而不是用旧历史覆盖新对话。

JSONL 修改有原子替换与回读检查，数据库更新使用各自事务；这不等于多个数据库之间存在一个总事务。模型目录注入是单独选项，不要求所有 Codex 用户改变原有模型列表方式。

### Claude 与 Gemini：使用各自的配置约定

Claude Code 管理网关环境变量、必要初始化状态和主模型/角色模型设置；Claude Desktop 使用其配置 profile。Claude 上下文提示按工具约定处理，不把所有工具都当 OpenAI SDK。

Gemini CLI 使用原生 Gemini Base URL 和认证模式。主动启动工具时，可给子进程注入所需环境并清除已知冲突项；不会为此修改系统全局环境或用户的 shell 启动文件。

代码：[工具目录](../client/src-tauri/src/tool_config/production/catalog.rs)、[配置存储与恢复](../client/src-tauri/src/tool_config/production/storage.rs)、[操作生命周期](../client/src-tauri/src/tool_config/production/operation.rs)、[配置写入器](../client/src-tauri/src/tool_config/production/tool_writers.rs)、[Codex 会话](../client/src-tauri/src/tool_config/production/codex.rs)、[Codex 模型目录](../client/src-tauri/src/tool_config/production/codex_catalog.rs)、[启动器](../client/src-tauri/src/tool_config/production/program_runtime.rs)。

<a id="lan"></a>
## 12. 局域网共享

### 一个网关，多名成员

主机选择允许共享的模型，为成员分配各自的 Key、启用状态和每周额度。成员使用标准 API 入口，不需要拿到上游订阅文件或供应者 API Key。

模型列表沿用短名称和共用排序；请求还会再次检查允许范围，不只靠界面隐藏。

### 成员用量本地记录

用量按成员和每周窗口保存在本地 SQLite。额度以加权 token 计数，当前规则为输入 × 1 + 输出 × 4，优先用上游返回值，缺失时才估算；这不是人民币或美元账单。

额度控制不能提前知道一个正在生成的回答最终有多长，不应把它理解成逐 token 的严格金融余额锁。

### 防止共享链路循环

共享路径携带经过的节点信息，并检查重复与最大跳数，避免两台机器相互设为上游后无限转发。

### 默认只适合可信网络

局域网共享不等于公网服务部署。默认本地 HTTP 不提供链路加密，成员 Key 也不是上游账号隔离沙箱。公开到不可信网络需要额外的 TLS、网络访问控制与部署设计。

代码：[LAN 成员、权限与用量](../client/src-tauri/src/lan_share.rs)。

<a id="runtime"></a>
## 13. 运行时、性能与诊断

### 同一配置目录只有一个运行时

进程通过文件锁确认当前目录的运行时所有者，第二次启动可以唤起已有窗口，而不是再绑定一套本地端口。控制通信使用本机连接和校验信息；关闭由运行时统一协调。

Local 版使用独立应用标识、配置目录和默认端口，减少与官方客户端冲突。但两者仍可能修改同一个外部工具配置，不应同时接管同一工具。

### 热路径避免重复大对象和外网查询

- 模型元数据、展示策略和能力摘要复用缓存；工具发现可以请求精简目录，不必读取完整证据。
- 资源归属优先查内存，持久化工作与查询分开。
- SSE 增量处理，上传和输出有流式路径及限量机制。
- 用量记录由有界队列交给写入线程，SQLite 使用 WAL；请求处理不为每条日志新建数据库连接。

这些是具体的开销控制方式，不是未经测量的吞吐量或内存占用保证。

### 本地数据有保留上限

调用明细与聚合统计分开保存。当前明细保留量为 500 条，日统计有 90 天保留窗口；日志按大小、时间和文件数量轮转。工具备份也有按文件系列的去重与保留规则，避免每次打开界面都累计一份完整备份。

### 观察真实的延迟与失败

日志和状态区分连接、上游首个有意义事件、用量及最终错误。SSE 在 HTTP 200 之后仍可能失败，错误观察不会只看响应头。

重复底层网络细节默认收敛，保留请求摘要和实际故障。错误摘要会脱敏，但仍应在提交问题前检查日志，不能把任何诊断输出都当作可直接公开的数据。

### 调试能力显式开启

调试台可预览协议转换与实验功能；实验按钮不等于默认请求路径。原始流量记录要求开发构建、危险调试构建特性和运行时开关同时允许，普通开关不足以把正常版本变成明文会话记录器。内存诊断也要求开发构建并显式启用对应特性。

代码：[单实例](../client/src-tauri/src/instance.rs)、[目录摘要](../client/src-tauri/src/model_discovery.rs)、[用量数据库](../client/src-tauri/src/supplier/usage_db.rs)、[日志](../client/src-tauri/src/logging.rs)、[调试台](../client/src-tauri/src/debug_console.rs)、[构建特性](../client/src-tauri/Cargo.toml)。

<a id="extension"></a>
## 14. 扩展方式、验证与能力边界

### 修改应放在对应层

| 要增加什么 | 主要位置 | 不应混入的地方 |
| --- | --- | --- |
| API 渠道 | 驱动目录、发现逻辑、共用 HTTP 执行器 | 每个工具的独立转发分支 |
| 订阅特性 | 对应订阅适配器与凭据逻辑 | 所有同协议请求的全局清洗 |
| 协议内容 | 中间结构、适配器、转换计划和流式编码 | UI 字符串替换 |
| 工具 | 工具目录、配置写入器、恢复规则 | 渠道路由与平台账务 |
| 模型信息 | 观测元数据、内置目录、兼容分组 | 上游 wire ID 的随意改名 |
| 展示排序 | 共用展示策略 | 逐个下拉框打补丁 |

### 测试关注完整对话行为

仓库包含协议矩阵、流式分片、工具往返、资源归属、配置恢复、Codex 索引迁移、LAN 权限及 Local 平台隔离测试。重点不只是“JSON 能解析”，还包括多轮状态、取消、错误尾帧和取消配置后的数据完整性。

Mock 测试证明的是代码行为，不代表所有上游账号都已经实测。真实生成可能收费，不能把普通刷新或 CI 变成隐含的付费测试。构建与推荐检查见 [SOURCE](../SOURCE.zh-CN.md)。

### 公开范围是明确的

Local 版保留自己的上游连接、协议转换、工具管理和 LAN 共享。官方账号、市场、远程供应、结算和平台请求通道不可用；配置加载/保存与实际连接入口都有版本边界，不只是隐藏几个按钮。

本仓库不包含私有服务器、官方发布密钥或平台风控实现。官方安装包仍由主仓库构建，公开 CI 只验证本机版源码，不发布另一套官方安装包。

源码边界不是对任意修改版程序的安全认证。Local 版仍会访问用户配置的 URL；平台自身必须执行鉴权，不能依赖别人不修改客户端。

### 不承诺的事情

- 不绕过上游的登录、地区、套餐、额度或服务限制。
- 不让没有工具、视觉或实时能力的模型凭转换获得这些能力。
- 不保证任意厂商私有状态能够跨协议、跨账号回放。
- 不保证固定缓存命中率、连接复用率、延迟或收益。
- 不把开源许可等同于上游服务访问权；第三方许可和商标权仍然有效。

代码与进一步阅读：[协议测试](../client/src-tauri/src/protocol/tests/)、[工具配置测试](../client/src-tauri/src/tool_config/tests/)、[Local 边界测试](../client/src-tauri/src/local_policy.rs)、[源码范围](../SOURCE.zh-CN.md)、[第三方声明](../THIRD_PARTY_NOTICES.md)。
