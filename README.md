<p align="center">
  <img src="docs/images/logo.png" width="88" alt="CONST API logo">
</p>

<h1 align="center">CONST API</h1>

<p align="center"><strong>Use your AI tools through one local API.</strong></p>

<p align="center">
  <a href="https://github.com/llxisdsh/const-api-public/releases/latest">Download</a> ·
  <a href="docs/usage.md">Usage guide</a> ·
  <a href="README.zh-CN.md">简体中文</a> ·
  <a href="https://github.com/llxisdsh/const-api-public/issues">Get help</a>
</p>

CONST API is a desktop app for Windows, macOS, and Linux. It gives ChatGPT, Codex, Claude Code, Gemini CLI, OpenCode, VS Code, WorkBuddy, and other compatible tools a stable local API, then connects them to the model supply you choose: your own API key, a supported subscription, a local model, or shared supply.

## Start

1. Download and open the [latest release](https://github.com/llxisdsh/const-api-public/releases/latest).
2. When **Local API · Running** appears, click a tool icon to configure it. For manual setup or command-line use, see the [usage guide](docs/usage.md).
3. Sign in if you want shared supply. Open **Supply** only when you want to add your own API key, subscription, or local model.

![CONST API Use page with managed AI tools and the running local API](docs/images/use.png)

Your channels stay on this device by default. You decide whether a channel may also participate in shared routing.

![CONST API Supply page with illustrative API, subscription, and local channels](docs/images/supply.png)

<sub>The Supply screenshot uses demo data, not real accounts or credentials.</sub>

## Model prices

- **Your own API key, subscription, or local model:** the provider's pricing, subscription terms, or your own running cost applies.
- **Shared supply:** open **Price reference** to see model reference prices and currently available offers. Routing prefers lower-priced available supply while also considering reliability and capacity.
- A request locks its price snapshot when it starts and is settled from its usage. Shared requests without a clear pricing rule are rejected instead of being charged from a guessed price.
- Open **Usage log** after a request to see recorded tokens and any platform charge. Price data can update without installing a new desktop version.

## Security and privacy

- AI tools connect to `127.0.0.1:38787`; changing a provider or model does not require replacing the API address in every tool.
- Provider credentials remain on the machine that runs the channel. They are not included in public catalogs or channel registration data.
- A channel running on your device bypasses the CONST API platform and connects directly from that device to its provider or local runtime. When you choose shared supply, request content is relayed through the platform to the selected supplier and model provider.
- Usage logs keep operational metadata, not prompts, response text, API keys, tokens, cookies, or authorization headers.
- Managed tool setup creates a backup. Canceling setup restores only fields managed by CONST API, so later user edits take priority.

## How it works

```text
AI tool → local gateway → local channel or shared route → model provider
```

The gateway exposes native OpenAI, Anthropic, and Gemini interfaces. It keeps the tool-facing address stable, uses an exact local model when available, and otherwise routes through the supply you enabled. Model, price, compatibility, tool, and endpoint catalogs are signed and can update independently of the desktop app.

## Open source

The open-source release is in preparation.
