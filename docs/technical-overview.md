# Architecture and technical features

[简体中文](technical-overview.zh-CN.md) · [README](../README.md) · [Usage guide](usage.md) · [Build from source](../SOURCE.md)

CONST API is a desktop AI gateway. A tool connects to a local endpoint; the gateway selects a channel, sends the request upstream, and returns a response in the format the tool understands. It does not run a large language model or execute code on behalf of tools such as Codex and Claude Code.

This document is for readers evaluating an integration, learning how the gateway works, or contributing code. Each chapter links to its implementation. **Unless explicitly marked otherwise, features describe the public CONST API Local edition.** Official-platform features such as catalog distribution, dynamic pricing, and remote supply are identified separately; their server implementations are not part of this repository.

## Contents

1. [Architecture: a Rust core and a desktop interface](#architecture)
2. [API surfaces: three entrances, four generation protocols](#api-surfaces)
3. [Protocol conversion: pass through first, explain differences when converting](#conversion)
4. [Tool calls and conversation state](#tools-and-state)
5. [HTTP, SSE, and WebSocket connections](#transports)
6. [Images, audio, video, uploads, and resources](#media)
7. [Channels, subscriptions, and credentials](#channels)
8. [Model names, ordering, context limits, and compatibility](#models)
9. [Model catalogs and price updates](#catalog-and-pricing)
10. [Checks, routing, quotas, and usage](#routing-and-usage)
11. [Tool configuration, launching, and restoration](#tool-configuration)
12. [LAN sharing](#lan)
13. [Runtime, performance, and diagnostics](#runtime)
14. [Extension points, validation, and boundaries](#extension)

<a id="architecture"></a>
## 1. Architecture: a Rust core and a desktop interface

### Rust handles the actual requests

The local HTTP server, upstream connections, protocol conversion, subscription adapters, streaming, and tool configuration run in Rust. Tokio schedules network tasks, Warp serves the local APIs, Reqwest and WebSocket clients connect upstream, and Serde handles structured data.

Requests do not pass through the renderer's JavaScript. Adding a provider does not require a separate Python or Node.js proxy process for that provider.

### Tauri connects the interface to the runtime

The React / TypeScript interface uses Tauri commands to read state and perform configuration operations. The desktop window uses the system WebView. The accurate description is **a Rust gateway core in a Tauri desktop application**, not an entirely “pure Rust” project and dependency stack.

### One shared local request path

```text
Codex / Claude / other tools / LAN members
                      |
          Local API: authenticate, identify operation
                      |
          Resolve model, select channel, check limits
                      |
          +-----------+------------+
   Same-protocol forwarding    Cross-protocol conversion
          +-----------+------------+
                 Channel executor
          +-----------+------------+
       Generic HTTP        Subscription-specific adapter
          +-----------+------------+
                    Upstream
                      |
     Native / converted response, with usage and error observation
```

The channel executor is the common execution boundary. Provider differences stay in their adapters; each tool does not need its own OpenAI, Claude, and Gemini integration.

Code: [runtime](../client/src-tauri/src/runtime.rs), [channel executor](../client/src-tauri/src/channel_executor.rs), [dependencies and build features](../client/src-tauri/Cargo.toml).

<a id="api-surfaces"></a>
## 2. API surfaces: three entrances, four generation protocols

### Tools keep their familiar protocol

The local edition defaults to `http://127.0.0.1:38789`. One listening port exposes three API path families; the OpenAI surface supports two generation protocols.

| API base path | Example generation endpoint | Generation protocol |
| --- | --- | --- |
| `/v1` | `/v1/responses` | OpenAI Responses |
| `/v1` | `/v1/chat/completions` | OpenAI Chat Completions |
| `/anthropic` | `/anthropic/v1/messages` | Anthropic Messages |
| `/gemini` | `/gemini/v1beta/models/{model}:generateContent` | Native Gemini |

Tools do not all have to switch to Chat Completions. A Responses client can keep using Responses, and a Gemini client can keep its native paths and streaming operation.

### The API covers more than chat

The operation registry also recognizes model discovery, token counting, Responses compaction, files, multipart uploads, images, audio, video, embeddings, batches, conversation resources, Gemini caches, and realtime connections.

**Recognizing an endpoint does not mean every channel can execute it.** Non-chat operations generally need a suitable native upstream. A chat-only channel does not become an image or file service just because the gateway recognizes those paths.

### Paths, headers, and binary bodies remain distinct

The request envelope separates paths from query strings and supports repeated headers and binary bodies. It does not force every request through a chat JSON parser. Errors are shaped for the caller's API so existing tools can interpret them.

`/.well-known/const-api` exposes gateway discovery information. Paths and operations are defined in a shared contract.

Code: [API contract](../shared/api-surface-contract.json), [operation matching](../client/src-tauri/src/surface.rs), [request envelope](../client/src-tauri/src/surface_wire.rs).

<a id="conversion"></a>
## 3. Protocol conversion: pass through first, explain differences when converting

### Preserve native content when protocols match

When the incoming and target protocols match, and no CONST cross-protocol continuation needs decoding, the request takes a passthrough branch. It is not first broken into generic messages and rebuilt. If only the model needs changing, a targeted rewrite preserves other unknown fields and the original JSON formatting.

“Preserve” does not mean every byte is always identical. Upstream authentication, necessary transport-header handling, model ID replacement, and subscription-required adjustments still apply. Unsigned hidden Chat reasoning history also has a specific filtering rule. Subscription requirements belong in subscription adapters, not in a global cleanup rule applied to every channel.

### Cross-protocol requests use an intermediate representation

Responses, Chat Completions, Messages, and Gemini each have a decoder and an encoder. Conversion first produces a common structure and then encodes it for the destination.

That structure includes more than `role` and `content`:

- System instructions, conversation turns, text, and refusals.
- Reasoning, tool definitions, tool calls, and tool results.
- Images, audio, video, and file references.
- Cache hints, usage, finish reasons, and provider extensions.

A new shared content type can therefore be handled in one intermediate layer instead of requiring an independent converter for every pair of protocols.

### Conversion records capability differences

Plans distinguish `native`, `lossless`, `compatible`, `lossy`, and `blocked`, recording affected fields, adjustments, and reasons. These labels describe a conversion; they do not promise that every lossy plan will be executed.

Content is preserved wherever it can be represented sensibly. A required feature that cannot be represented produces an explicit error. Format conversion cannot give a destination model tools, media support, or provider-specific state it does not possess.

### Streaming conversion operates on events

The SSE decoder handles arbitrary network chunks before producing common start, content-delta, tool-delta, usage, finish, and error events. A renderer then emits the caller's protocol. One TCP read is not mistaken for one complete SSE message, and conversion does not require waiting for the whole answer.

Code: [conversion entry point](../client/src-tauri/src/protocol/mod.rs), [intermediate representation](../client/src-tauri/src/protocol/ir/), [plans and reports](../client/src-tauri/src/protocol/conversion/), [streaming](../client/src-tauri/src/protocol/stream/).

<a id="tools-and-state"></a>
## 4. Tool calls and conversation state

### Keep calls connected to their results

A tool call contains more than a function name: it has a call ID, arguments, a possible namespace, and result references. Conversion maintains those relationships, handles streamed argument fragments, and validates the tool transcript to reduce missing-call errors.

CONST transports definitions, calls, and results. The actual tool, such as Codex, still reads files, executes commands, and edits code.

### Handle Responses additional tools and freeform tools

- Tool definitions are read from top-level `tools` and from the `tools` array of `input[]` items whose `type` is `"additional_tools"`. Native forwarding preserves their placement; cross-protocol conversion merges them into the destination's declarations.
- When a destination has no namespaces, distinct function names are generated and mapped back to the original tool identities on responses.
- A freeform tool can be wrapped as a JSON function with a string `input`, then unwrapped on return. Its grammar is retained as format guidance, not as a claim that the destination supports native grammar-constrained sampling.

These mappings belong to the individual request, not to a global table containing every user's tool names.

### Provider state is not freely portable between accounts or backends

Reasoning signatures, encrypted content, file IDs, and continuation state may only be valid at the upstream that produced them. Conversion records origin and replay constraints rather than treating private state as ordinary text to send to any backend.

Visible messages and tool history are handled separately from opaque provider state. CONST continuation envelopes also have a dedicated decoding path; they do not depend on a tool's interface displaying hidden fields.

### Compaction has its own compatibility boundary

Responses compact and Claude server-side compaction are different operations. Claude compaction blocks, finish reasons, and usage receive dedicated handling and require a native Anthropic target. They are not silently turned into ordinary Chat requests. Availability still depends on the model and upstream.

Code: [tool bridge](../client/src-tauri/src/protocol/conversion/tool_bridge.rs), [Responses adapter](../client/src-tauri/src/protocol/adapters/openai_responses.rs), [artifact replay policy](../client/src-tauri/src/protocol/conversion/artifact_policy.rs), [continuation envelopes](../client/src-tauri/src/protocol/continuation.rs), [request conversion](../client/src-tauri/src/proxy/request_conversion.rs).

<a id="transports"></a>
## 5. HTTP, SSE, and WebSocket connections

### HTTP reuse and streaming responses

The runtime reuses HTTP clients and their connection pools instead of creating a new networking stack for every generation. JSON, SSE, and binary responses use their appropriate forwarding paths. Downstream disconnection releases the corresponding tasks and connection leases.

### HTTP/3 is a conditional upstream optimization

The generic upstream transport can probe HTTP/3 per origin and cache both readiness and failure cooldowns. A matching system proxy or a request that cannot safely be cloned keeps the ordinary HTTP path. Native subscription endpoints keep their normal transport rather than receiving extra HTTP/3 probes.

Changing transports is not a reason to retry blindly. A generation request that may already have executed is not automatically resent just because transport failed, avoiding duplicate quota consumption.

### Responses WebSocket reuse has separate modes

The pool groups connections by channel, credential reference, endpoint, handshake parameters, and protocol mode. Different identities or incompatible handshakes do not share a connection.

- One conversation can complete successive turns while keeping its continuation connection affinity.
- An upstream that supports `stream_id` multiplexing can carry distinct logical streams on one physical connection.
- The public OpenAI API and subscription endpoints select modes separately. Subscription multiplexing has a separate probing option; support is not assumed for every account.
- Without established multiplexing support, connections use exclusive mode. Exclusive connections have idle collection, and aging connections stop accepting reuse before closing.

Supporting WebSocket therefore does not mean every session shares one socket, nor that ordinary HTTP requests are always converted to WebSocket.

### Receive queues are bounded by bytes

Response buffers allocate on demand and apply a byte budget with backpressure: when a consumer is slow, upstream reading slows down instead of retaining an unlimited message queue. The current general output budget is 32 MiB, shared per HTTP response or physical WS connection. It is neither a large preallocation per token nor a process-wide memory limit.

Already-decoded large frames remain subject to transport limits. Terminal errors have a separate finishing path so a full data queue does not conceal why the stream ended.

### Platform tunnels are a different network hop

The official edition also uses WebSocket / QUIC between the platform and suppliers. That is distinct from a local tool's upstream connection. The public local edition disables those platform connection entry points while retaining its own upstream HTTP, SSE, and WebSocket access.

Code: [upstream transport selection](../client/src-tauri/src/upstream_transport.rs), [Responses WS pool](../client/src-tauri/src/proxy/responses_ws_pool.rs), [WS forwarding](../client/src-tauri/src/proxy/websocket.rs), [output backpressure](../client/src-tauri/src/output_buffer.rs).

<a id="media"></a>
## 6. Images, audio, video, uploads, and resources

### Media input is more than a URL

The common content structure distinguishes inline Base64, remote URLs, and provider file IDs, retaining media types and image detail settings. Cross-protocol use depends on the destination protocol and model accepting that input.

### Native media operations keep their own semantics

Image generation and editing, speech and transcription, video, and embeddings have distinct operation matching. Native API channels can forward appropriate endpoints and binary responses. Subscription channels admit only operations explicitly supported by their adapters; a subscription is not assumed to expose every API from the same company.

### Large files need not be fully buffered in memory

Files and multipart uploads have dedicated streaming paths with operation-specific size limits. Gemini resumable uploads also track upload sessions so subsequent chunks reach the corresponding upstream.

### Follow-up resource requests return to the creating channel

Resource IDs for files, Responses, conversations, and similar objects are associated with their channel. Reads, cancellation, and deletion use that ownership rather than randomly selecting a new account.

The ownership registry supports in-memory lookup and persists through local SQLite for restart recovery, with a bounded number of entries. Missing ownership is not treated as proof that some other channel can access the resource.

### Realtime connections are separate from ordinary SSE

Realtime voice can involve WebSocket or SDP/WebRTC setup, not audio disguised as chat text. The code contains local OpenAI Realtime setup adapters; individual entry points remain subject to the channel contract.

The official platform additionally forwards Opus and data-channel frames. Each edge handles WebRTC, while the tunnel carries encoded frames; it does not record microphones, transcode media, or replay old audio. **That platform media tunnel is not an available service in the public local edition.**

Code: [media content types](../client/src-tauri/src/protocol/ir/content.rs), [streaming uploads](../client/src-tauri/src/proxy/streaming_upload.rs), [Gemini upload sessions](../client/src-tauri/src/proxy/gemini_upload_session.rs), [resource ownership](../client/src-tauri/src/proxy/resource_owner.rs), [local Realtime](../client/src-tauri/src/proxy/local_openai_realtime.rs).

<a id="channels"></a>
## 7. Channels, subscriptions, and credentials

### Channel types define execution rules

Driver definitions centralize authentication, model discovery, API surfaces, and execution adapters. Common API providers and custom endpoints share the HTTP executor; only subscriptions and gateways with special requirements need dedicated handling.

| Category | Existing driver examples | Main differences |
| --- | --- | --- |
| Account subscriptions | OpenAI/Codex, Claude, Antigravity, Grok | Account credentials, dedicated endpoints, refresh, and quotas |
| Provider APIs | OpenAI, Anthropic, Gemini, xAI, Mistral, DeepSeek, DashScope, Moonshot, Zhipu, MiniMax, StepFun | API authentication and native protocols |
| Cloud and inference platforms | Azure OpenAI, Bedrock Mantle, Groq, Together, Fireworks, Hugging Face, NVIDIA, SiliconFlow, Volcengine, Baidu, Tencent | Endpoint layout, authentication, or catalogs |
| Gateways and coding plans | OpenRouter, OpenCode Go/Zen, Kilo, Cline, Command Code, Kimi Code, GLM Coding Plan, MiniMax Token Plan, Ollama Cloud, OmniRoute | Fixed service contracts, catalogs, and model-specific endpoints |
| Local and generic endpoints | Ollama, LM Studio, vLLM, custom endpoints, LAN sharing | Local servers or compatible APIs |

This describes driver coverage, not a separate OAuth flow for every service or completed live-account testing of every paid plan. Some subscription plans still provide ordinary API keys.

### Subscription adapters own the unavoidable differences

A provider's subscription and standard API may use different authentication, client identities, request fields, and model catalogs. Dedicated adapters handle those differences so the general protocol layer does not need to know every plan's details.

Supported authorization methods include browser OAuth, callback listeners, and credential imports, depending on the channel. Supported flows can accept a pasted callback URL when the browser cannot reach the local listener; they do not require terminating another application using that port.

### Credential refresh is coordinated and persisted

Refresh is locked by credential path; some refresh-token flows also coalesce work by identity to reduce concurrent token-rotation conflicts. Successful refreshes atomically update the credential file for later requests.

Network failures and explicit authorization invalidation are handled separately. Compatibility with older account-file formats does not mean ignoring a revoked token.

Antigravity has a public-build requirement: browser authorization and refresh need compatible OAuth application settings supplied by the builder. See [SOURCE](../SOURCE.md#optional-antigravity-oauth-configuration).

### User-Agent overrides stay at the relevant boundary

Generic HTTP/API channels offer optional Claude Code, Codex, and OpenCode identity profiles for upstreams that require a particular client identifier. Leaving the profile empty does not actively override the caller's identity; some fixed gateways have a default of their own. Subscription identities remain managed by their adapters.

A different User-Agent does not create subscription entitlement or guarantee upstream acceptance.

### OpenRouter discovery is account-scoped

OpenRouter discovery uses the account model catalog with all modalities, parsing pagination, context limits, supported parameters, and endpoint capabilities. Text probes avoid selecting speech- or image-output models by accident.

An authentication failure or explicitly empty account catalog is not silently replaced with the public unfiltered catalog. Listed models still undergo local availability decisions: being listed does not prove that the current account or region can invoke them.

Code: [driver catalog](../client/source-drivers.json), [executor](../client/src-tauri/src/channel_executor.rs), [credentials and detection](../client/src-tauri/src/detection.rs), [credential imports](../client/src-tauri/src/detection/subscription_credentials.rs), [User-Agent handling](../client/src-tauri/src/channel_user_agent.rs), [OpenRouter discovery](../client/src-tauri/src/openrouter/catalog.rs).

<a id="models"></a>
## 8. Model names, ordering, context limits, and compatibility

### Display a short name, call the original ID

Model identities have separate purposes rather than making one string do everything:

| Purpose | Example | Rule |
| --- | --- | --- |
| Display and tool selection | `model-a` | Lowercase final path component |
| Upstream request | `vendor/model-a` | Preserve the ID advertised by the channel |
| Routing | `vendor/model-a` on a particular channel | A shared display name does not merge accounts |
| Platform pricing | Canonical catalog model and pricing rules | Match explicit identities or aliases without rewriting the wire ID |

Normalization happens in the model layer and is shared by model APIs, tool injection, and LAN lists. It is not a cosmetic fix applied to one dropdown.

For short-name collisions within a channel, the first item is retained instead of switching randomly. Different channels can still supply the same public name. Short-name presentation does not promise to expose every vendor variant with the same name. Dynamic price catalogs have a separate deterministic source-merging rule.

### Ordering is a shared presentation policy

An ordered set of exact-name or prefix rules groups models, with model-ID ordering within a group and optional hiding. Tool directories and LAN lists reuse this priority. The official platform can distribute presentation rules; the public edition uses rules packaged with the source.

Hiding affects lists, not authorization to invoke a model. A third-party tool that sorts the list again still controls its final interface.

### Context limits prefer channel observations

Discovery can supply context windows, output limits, input/output modalities, supported parameters, and endpoint declarations. Tool metadata prefers valid channel observations and supplements them with the bundled catalog instead of guessing entirely from names.

When a model may route to multiple compatible candidates, capacity is combined conservatively. One candidate with a large window is not enough to claim that every possible route supports that window.

### `[1m]` is a context hint, not a duplicate model family

The ordinary model list does not duplicate entries just to add `[1m]`. Tools that understand this convention can receive an appropriate hint or configuration when supported by capability information, while forwarding preserves the information needed for the actual call.

Literal upstream IDs and tool context aliases are resolved separately. Adding a suffix does not give every Claude model a 1M window or bypass an upstream context limit.

### Compatible-model substitution is explicit

Compatibility groups define alternative candidates. Routing expands the candidate set only when substitution is enabled; similar display names are not automatically equivalent. Catalogs, compatibility groups, and capability data remain separate so they can evolve independently.

Code: [name handling](../client/src-tauri/src/config.rs), [catalog observations](../client/src-tauri/src/model_catalog.rs), [presentation policy](../client/src-tauri/src/model_discovery.rs), [tool model metadata](../client/src-tauri/src/tool_model_metadata.rs), [compatibility groups](../client/src-tauri/src/model_compatibility.rs), [bundled ordering rules](../shared/tool_model_presentation_defaults.json).

<a id="catalog-and-pricing"></a>
## 9. Model catalogs and price updates

### Keep three kinds of information separate

- **Account discovery:** models currently advertised by this channel's upstream.
- **Product catalog:** public metadata such as names, capabilities, context limits, and compatibility groups.
- **Reference prices:** rules used for platform quotes and billing, not evidence of invocation permission.

The public edition queries its own upstreams and includes bundled tool metadata. It does not include the official market-pricing page or platform settlement service, and copying official settings does not enable automatic platform-catalog updates. Bundled metadata is normally updated by taking a newer source snapshot and rebuilding.

### Official platform: application and catalog releases are separate

Official model and price catalogs can be published independently of applications. Adding a model need not require a client upgrade. Catalogs carry versions, digests, and signature checks; runtime consumers use validated snapshots. The public source retains some shared structures and verification code, but the official catalog refresh task is disabled in Local.

### Official platform: public price APIs provide dynamic data

Implemented sources include OpenRouter, Kilo, and Cline. The server reads these trusted public catalogs, converts their quotes into existing pricing rules, and merges them with the published baseline. An ID-only model list is not a price source, and arbitrary suppliers cannot set platform-wide reference prices.

Successful sources normally refresh about every six hours; failed sources retry independently later. Conditional requests, coalesced refreshes, and per-source last-successful caches reduce unnecessary traffic. Inference requests, pagination, and heartbeats do not trigger price fetching. Failures retain valid previous prices; without dynamic data, the published baseline applies.

### Official platform: normalize units without flattening price dimensions

Input, output, cache reads/writes, and long-context tiers remain distinct. A missing price is not zero. New media prices with unclear units are not admitted simply because a field looks familiar. `:free` follows explicit zero-price rules; `[1m]` does not itself add a fixed surcharge, and long-context usage follows actual pricing rules.

When a model has multiple trusted quotes, the higher reference value is used per billing dimension, with the existing multiplier still applied. This is a common reference price, not a supplier's purchase cost or a profit guarantee.

### Official platform: prices are pinned when a request starts

Model admission and prices use the same runtime snapshot. An in-flight request retains its prices and tier rules; later updates affect new requests only. A long stream does not change price halfway through, and historical bills are not recalculated.

Public code: [bundled tool catalog](../client/src-tauri/resources/tool-model-metadata.json), [catalog verification components](../client/src-tauri/src/catalog_registry.rs), [Local edition boundary](../client/src-tauri/src/local_policy.rs). The dynamic-pricing and settlement implementations marked “Official platform” are in the non-public server and cannot be started from this repository alone.

<a id="routing-and-usage"></a>
## 10. Checks, routing, quotas, and usage

### Refreshing is not a paid generation test

Channel refresh reads model, quota, and protocol metadata without issuing model-generation requests. A current-model test or full check validates actual calls and can consume upstream tokens.

Detection distinguishes catalog declarations, verified support, and unknown capabilities. A native Responses endpoint does not automatically imply support for every Codex tool or extension.

### Grouped checks avoid scanning every model

Full checks group models into Anthropic, Google, OpenAI, and other models, select a representative from each group, and run within the channel's concurrency limit. Attempts have a budget, including adapter-internal retries, rather than adding generation requests indefinitely.

Confirmed regional, policy, or account restrictions can affect a corresponding group. Model-specific problems, network transients, and checks that never ran are treated separately. A group result is not evidence that every member was individually tested.

Background recovery depends on failure state, due time, and actual demand, with bounded attempts. Confirmed hard restrictions do not enter frequent paid polling. An inconclusive check on an existing channel should not be treated as proof that all its models are unavailable.

### Discovery failures do not all erase the usable list

Transient network failures, rate limits, or upstream service errors can retain a previously verified catalog. Explicit authentication failures, parsing errors, and a valid empty account catalog are distinguished and surfaced rather than all being presented as successful refreshes.

### Routing considers identity, health, and capability

Routing resolves the requested model, the channel's models, availability, quota, and optional compatible candidates. Runtime failures and recovery are observed per model so one exhausted model does not automatically disable unrelated ones. Authorization failures affecting the whole channel are handled at channel scope.

The public local edition selects its own channels only; it does not silently fall back to the official paid marketplace. The official edition's local-first and platform fallback policies form a separate routing layer.

### Error classification reads protocol errors, not model answers

The observer reads HTTP status, protocol error objects, streamed error events, and evidence such as `Retry-After`. It distinguishes authorization, quota, capacity, rate limits, unsupported models, and network faults. A model mentioning “rate limit” in an ordinary answer is not classified as a failed request.

Errors retain scope, retry suitability, suggested waiting time, and a cause summary. A transport failure is not proof that upstream execution never happened; switching channels or recovering a connection still follows the retry conditions of the relevant path.

### Usage and cache accounting preserve provider semantics

- Input, output, cache reads, and cache writes are recorded separately. Claude's five-minute and one-hour cache writes can remain distinct.
- Anthropic accounting handles top-level input counts that exclude cached tokens. Compaction iterations are accumulated separately so later SSE deltas do not overwrite already observed compaction usage.
- An absent cache count is different from an explicit zero. A faster response alone is not proof of a cache hit.
- Available cache identities and hints are preserved to help upstream prefix reuse. CONST does not share cached model answers between users or promise a fixed cache-hit rate.

### Quota windows and request limits are separate

Quota windows retain their source and period. Primary account windows can gate supply; auxiliary feature buckets do not automatically become channel-wide limits. Sources with per-model quotas, such as Antigravity, receive separate handling. Configured concurrency, request rate, daily limits, and reserved quota are checked independently.

Local forwarding does not bypass the channel's own limits merely because it bypasses the platform. Displayed quota, measured request usage, and platform prices also remain different kinds of data.

Code: [grouped checks](../client/src-tauri/src/supplier/availability.rs), [local routing](../client/src-tauri/src/proxy/runtime.rs), [model outcomes](../client/src-tauri/src/proxy/model_health.rs), [failure classification](../client/src-tauri/src/upstream_failure.rs), [upstream usage](../client/src-tauri/src/supplier/upstream_usage.rs), [quota windows](../client/src-tauri/src/supplier/quota.rs), [subscription limits](../client/src-tauri/src/supplier/safety.rs).

<a id="tool-configuration"></a>
## 11. Tool configuration, launching, and restoration

### Tool descriptions are separate from config writers

The tool catalog defines protocols, model synchronization, program discovery, and launch behavior. Writers handle JSON, JSON5, TOML, YAML, environment files, or application databases. Adding a tool does not require copying channel execution logic.

The current catalog includes Codex, Claude Code, Claude Desktop, Claude Science, Gemini CLI, GitHub Copilot CLI, Cline CLI, OpenCode, OpenClaw, Goose, Raven, DeepSeek Reasonix, DeepSeek Harness, Pi, Hermes, VS Code, WorkBuddy, Kimi Code, MiMo Code, Qwen Code, Open Science, Open Interpreter, AnythingLLM, Mistral Vibe, Open Design, Vibe-Trading, and ZCode. Available operations depend on each tool's version and configuration interfaces; this is not blanket management of every IDE extension.

### Own only the fields the integration manages

Configuration management records original values, last-applied values, and the owning tool. Unchanged content is not rewritten. Changes use backups, atomic writes, and operation locks.

Unconfiguration uses a three-way comparison: if the current value still matches CONST's last value, restore the original; if the user changed it afterward, preserve their new value. Named providers and marked entries are cleaned up by ownership, leaving unrelated providers, MCP configuration, and other settings intact.

This is better suited to ongoing use than restoring an entire old file. Ownership also helps avoid duplicate model entries when switching between official and development configurations.

### Model injection carries more than names

Where supported, generated configurations include context and output limits, input modalities, tool support, reasoning capabilities, and display priority. Tools that accept a selected model are not forced to take an entire catalog; tools with catalog support receive the normalized available list.

Capabilities and limits come from common metadata rather than conflicting per-tool model tables. Whether an external tool shows custom names or reorders entries still depends on its own schema and behavior.

### Codex: switch configuration without hiding conversation history

Codex configuration maintains a dedicated provider rather than isolating history in another `CODEX_HOME`. Applying or removing CONST also synchronizes conversation provider ownership so the active integration can see the relevant history.

For newer sessions, synchronization updates necessary fields in the current SQLite indexes instead of rewriting indexed conversation JSONL files. Legacy sessions only have their leading provider metadata changed; conversation bodies are preserved. Removal synchronizes to the provider actually restored in config, including conversations created while using CONST, rather than overwriting new history with an old snapshot.

JSONL changes use atomic replacement and read-back checks; each database update uses its own transaction. That does not imply one transaction spanning all databases. Model-catalog injection is a separate option, not a requirement for every Codex user to change how their model list works.

### Claude and Gemini follow their own configuration conventions

Claude Code manages gateway environment settings, necessary onboarding state, and main/role model choices. Claude Desktop uses its configuration profile. Claude context hints follow the tool's convention rather than treating every tool as an OpenAI SDK client.

Gemini CLI uses a native Gemini Base URL and authentication mode. When launching a tool, CONST can inject the required child-process environment and remove known conflicts without changing system-wide variables or shell startup files.

Code: [tool catalog](../client/src-tauri/src/tool_config/production/catalog.rs), [storage and restoration](../client/src-tauri/src/tool_config/production/storage.rs), [operation lifecycle](../client/src-tauri/src/tool_config/production/operation.rs), [config writers](../client/src-tauri/src/tool_config/production/tool_writers.rs), [Codex sessions](../client/src-tauri/src/tool_config/production/codex.rs), [Codex model catalog](../client/src-tauri/src/tool_config/production/codex_catalog.rs), [launching](../client/src-tauri/src/tool_config/production/program_runtime.rs).

<a id="lan"></a>
## 12. LAN sharing

### One gateway, separate members

The host chooses shared models and gives each member a separate key, enabled state, and weekly allowance. Members use standard API endpoints without receiving the host's upstream subscription files or provider API key.

Model lists use the same short names and presentation policy. Requests enforce the allowed model set again rather than relying on hidden UI entries.

### Member usage is recorded locally

Usage is stored in local SQLite by member and weekly window. The current allowance uses weighted tokens: input × 1 + output × 4. Provider counts are preferred, with estimates used when missing; this is not a currency bill.

The gateway cannot know the final length of an in-progress answer in advance. This allowance should not be interpreted as a strict per-token financial balance lock.

### Detect sharing loops

The sharing path records visited nodes and checks repetitions and a hop limit. This prevents two machines configured as each other's upstream from forwarding indefinitely.

### Use a trusted network by default

LAN sharing is not a public-service deployment. Default local HTTP does not encrypt the link, and member keys are not an upstream-account isolation sandbox. Exposure to an untrusted network needs additional TLS, network access controls, and deployment design.

Code: [LAN members, authorization, and usage](../client/src-tauri/src/lan_share.rs).

<a id="runtime"></a>
## 13. Runtime, performance, and diagnostics

### One runtime per configuration directory

A file lock identifies the runtime owning the directory. A second launch can bring the existing window forward instead of binding another set of local ports. Control messages use a loopback connection and validation data; shutdown is coordinated by the runtime.

Local has its own application identity, data directory, and default port to reduce conflicts with the official app. Both editions can still edit the same external tool, so they should not manage that tool concurrently.

### Avoid repeated large objects and network lookups on hot paths

- Model metadata, presentation rules, and capability summaries reuse caches. Tool discovery can request a compact catalog rather than full evidence.
- Resource ownership reads use memory, with persistence handled separately.
- SSE is processed incrementally; uploads and output have streaming paths and bounded buffers.
- Usage records go through a bounded queue to a writer thread, with SQLite WAL. Request handling does not open a database connection for each log entry.

These are concrete ways to control overhead, not unmeasured throughput or memory guarantees.

### Local data has retention limits

Recent calls and aggregate statistics are stored separately. The current detailed-record limit is 500, with a 90-day retention window for daily statistics. Logs rotate by size, time, and file count. Tool backups are deduplicated and retained per file series instead of adding a complete backup whenever the interface opens.

### Observe actual latency and failures

Diagnostics distinguish connection setup, the first meaningful upstream event, usage, and final errors. An SSE request can fail after HTTP 200, so observation does not stop at response headers.

Repetitive low-level network details are quiet by default while request summaries and real failures remain visible. Error summaries are redacted, but logs should still be reviewed before reporting an issue; diagnostic output is not automatically safe to publish.

### Debug capabilities are explicit

The debug console can preview protocol conversion and experimental features. An experimental button is not automatically part of ordinary traffic. Raw traffic logging requires a development build, the dangerous-debug build feature, and the runtime switch together; a runtime setting alone cannot turn a normal build into a plaintext conversation recorder. Memory diagnostics also require a development build with their feature explicitly enabled.

Code: [single-instance control](../client/src-tauri/src/instance.rs), [compact catalogs](../client/src-tauri/src/model_discovery.rs), [usage storage](../client/src-tauri/src/supplier/usage_db.rs), [logging](../client/src-tauri/src/logging.rs), [debug console](../client/src-tauri/src/debug_console.rs), [build features](../client/src-tauri/Cargo.toml).

<a id="extension"></a>
## 14. Extension points, validation, and boundaries

### Put changes in the layer that owns them

| Addition | Primary location | Keep it out of |
| --- | --- | --- |
| API channel | Driver definitions, discovery, shared HTTP executor | Separate forwarding logic for every tool |
| Subscription feature | Its subscription adapter and credential handling | Global cleanup of every same-protocol request |
| Protocol content | Intermediate types, adapters, plans, and stream encoders | UI string replacement |
| Tool | Tool catalog, config writer, restoration rules | Channel routing and platform accounting |
| Model information | Observed metadata, bundled catalog, compatibility groups | Arbitrary renaming of upstream wire IDs |
| Display ordering | Shared presentation policy | Patches to individual dropdowns |

### Tests cover conversation behavior, not just JSON parsing

The repository includes protocol matrices, chunked streaming, tool round trips, resource ownership, config restoration, Codex index migration, LAN authorization, and Local platform-boundary tests. Important cases include multi-turn state, cancellation, terminal error events, and data preservation after unconfiguration.

Mocks establish code behavior, not live verification of every upstream account. Real generation can cost money; metadata refresh and ordinary CI should not conceal paid tests. See [SOURCE](../SOURCE.md) for build and recommended checks.

### The public boundary is explicit

Local retains its own upstream access, protocol conversion, tool management, and LAN sharing. Official accounts, the marketplace, remote supply, settlement, and platform request transports are unavailable. The boundary is enforced in config loading/saving and connection entry points, not just by hiding buttons.

This repository does not include the private server, official release keys, or platform abuse-prevention implementation. Official packages continue to be built in the main repository. Public CI checks the local source instead of publishing another set of official installers.

A source boundary does not authenticate arbitrary modified programs. Local still visits user-configured URLs, and hosted services must enforce their own authorization rather than rely on clients remaining unmodified.

### What this architecture does not promise

- Bypassing upstream login, regional, plan, quota, or service restrictions.
- Giving a model tools, vision, or realtime abilities it lacks.
- Replaying arbitrary private provider state across protocols or accounts.
- Fixed cache-hit rates, connection reuse, latency, or financial returns.
- Upstream service access through an open-source license; third-party licenses and trademark rights still apply.

Code and further reading: [protocol tests](../client/src-tauri/src/protocol/tests/), [tool configuration tests](../client/src-tauri/src/tool_config/tests/), [Local boundary tests](../client/src-tauri/src/local_policy.rs), [source scope](../SOURCE.md), [third-party notices](../THIRD_PARTY_NOTICES.md).
