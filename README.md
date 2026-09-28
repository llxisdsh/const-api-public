<p align="center"><img src="docs/images/logo.png" width="88" alt="CONST API"></p>

# CONST API

One local gateway for your AI tools, API providers, subscriptions, and local models.

[简体中文](README.zh-CN.md) · [Download the official app](https://github.com/llxisdsh/const-api-public/releases/latest) · [Usage guide](docs/usage.md) · [Architecture](docs/technical-overview.md) · [Build from source](SOURCE.md) · [Issues](https://github.com/llxisdsh/const-api-public/issues)

Connect a tool once, then manage its model channels in one desktop app. CONST API
provides OpenAI Responses / Chat Completions, Anthropic Messages, and Gemini API
interfaces on Windows, macOS, and Linux.

## Choose your edition

This repository contains **local-edition source code** and hosts **official app downloads**.
They are different editions:

| | CONST API Local — built from this source | Official app — downloaded from Releases |
| --- | --- | --- |
| Your own APIs, supported subscriptions, and local models | Yes | Yes |
| Managed tool setup and local / LAN access | Yes | Yes |
| CONST account, Model Marketplace, remote supply, and billing | Not included | Available with a CONST account |
| Default local API port | `38789` | `38787` |
| Configuration directory | `~/.const-api-local` | `~/.const-api` |
| Updates | Rebuild from a newer source snapshot | Official update channel |

Public CI checks the local source build; it does **not** publish installers.
Official releases continue to be built and published from the main private
repository. This source is not a reproducible build of the official application.

## What you can do locally

- **Manage model channels.** Add API keys, supported account subscriptions, or an
  OpenAI-compatible local server. Available models and capabilities depend on the upstream.
- **Connect your tools.** Configure supported tools such as Codex, Claude Code,
  OpenCode, and WorkBuddy from the app, or copy the local URL and key manually.
  Managed configuration is backed up; restoration preserves unrelated edits.
- **Use native protocols.** Same-protocol requests preserve native fields where
  the upstream allows them. Cross-protocol conversion handles messages,
  streaming, tool calls, and usage; it cannot create capabilities a model lacks.
- **Share on a trusted LAN.** Choose models and create separate member keys and
  usage limits. This is local sharing, not participation in the hosted market.
- **Troubleshoot requests.** Inspect channel health, model availability, usage,
  latency, and errors without guessing which upstream was used.

Local use needs no CONST account. Your provider may charge for requests and checks.
**Refreshing channel metadata is not a generation test**; model and full-channel
tests can consume upstream quota.

Antigravity browser sign-in and token refresh require compatible OAuth application
credentials supplied by the builder; those credentials are not published here.
See the [source-build requirements](SOURCE.md#optional-antigravity-oauth-configuration).

## How it works

The gateway runs in **Rust**, with a **Tauri + React/TypeScript** desktop interface.
Tools share one local request path; protocol and provider differences are handled
in dedicated adapters rather than repeated in each tool integration.

| Technical feature | Implementation |
| --- | --- |
| Native forwarding and cross-protocol conversion | Preserve native fields on matching protocols; use a common message/tool representation and event-based streaming conversion when protocols differ. |
| HTTP, SSE, WebSocket, and media | Reuse HTTP clients, manage Responses WS connections according to upstream support, and provide dedicated upload and realtime paths. |
| API and subscription channels | Share a channel executor while isolating provider authentication, credential refresh, and required subscription adjustments. |
| Consistent model lists | Keep short display names separate from wire IDs; share ordering, availability, context limits, and compatibility metadata across tools and LAN access. |
| Reversible tool configuration | Track field ownership, back up changes, preserve user edits on removal, and synchronize Codex session indexes when switching providers. |
| Bounded runtime overhead | Incremental streams, byte-budgeted output buffers, compact model catalogs, cached metadata, and bounded log/history retention. |

The [technical guide](docs/technical-overview.md) explains these mechanisms and more,
with code links across 14 chapters. It also describes **official-platform-only**
catalog releases and dynamic reference pricing without presenting them as local
source-build services. [Read in Chinese](docs/technical-overview.zh-CN.md).

## Quick start

### Use the official app

1. Download the package for your OS from [Releases](https://github.com/llxisdsh/const-api-public/releases/latest).
2. Add a channel under **Models**. Sign-in is optional for local use; the hosted
   Model Marketplace requires an account.
3. Wait for **Local API · Running**, then configure a tool from **Use**.

### Build CONST API Local

Install Node.js matching `client/.node-version`, stable Rust, and the native
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.

```sh
git clone https://github.com/llxisdsh/const-api-public.git
cd const-api-public/client
npm ci
npm run dev
```

Add and enable your own channel, then configure a tool. No server deployment or
private repository is needed. To create an executable, run `npm run build`.
See [SOURCE.md](SOURCE.md) for build checks, output paths, and edition boundaries.

Both editions are desktop applications. There is no supported headless or
configuration-check CLI; older instructions using those modes are obsolete.

## How requests travel

**Local edition:** your tool → local CONST gateway → your configured upstream.
The built-in CONST platform login, discovery, supply connection, and request
transports are unavailable. Copying official account settings does not enable them.

**Official app:** requests use your own channels or the hosted Model Marketplace
according to your routing settings. A marketplace request passes through the
platform and selected supplier before reaching the upstream provider. Provider
credentials remain on the supplier's machine, but request contents necessarily
reach the systems that process them.

The local edition is not an offline model or a network security sandbox. It connects
to the upstream URLs you configure. Source-level restrictions cannot authenticate
an arbitrary modified client; hosted services enforce their own authorization.

## Project layout

| Path | Contents |
| --- | --- |
| `client/src/` | React / TypeScript desktop interface |
| `client/src-tauri/` | Rust runtime, local gateway, adapters, tool configuration |
| `shared/` | Public API contracts and bundled metadata |
| `testdata/` | Protocol fixtures |
| `.github/workflows/check-source.yml` | Build and boundary checks; no publishing |

Source snapshots are synchronized from the main repository. Contributions should
be small and focused; include a reproduction and relevant tests. Maintainers
integrate accepted changes upstream before exporting the next snapshot.

## Help and licensing

- [Usage, manual API configuration, and troubleshooting](docs/usage.md)
- [Architecture, technical features, and code map](docs/technical-overview.md)
- [Source build and isolation details](SOURCE.md)
- [Report a bug](https://github.com/llxisdsh/const-api-public/issues): include your
  edition, version, OS, and redacted error. Never post keys, tokens, account files,
  or private conversation content.

Original exported source is [MIT licensed](LICENSE). Third-party dependencies and
assets retain their own licenses; see [Third-party notices](THIRD_PARTY_NOTICES.md).
The license does not grant access to the hosted service, upstream subscriptions,
or rights to third-party trademarks.
