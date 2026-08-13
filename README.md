<p align="center">
  <img src="docs/images/logo.png" width="88" alt="CONST API logo">
</p>

<h1 align="center">CONST API</h1>

<p align="center"><strong>All your AI tools. One API.</strong></p>

<p align="center">Set up once. Switch models anytime.</p>

<p align="center">
  <a href="https://github.com/llxisdsh/const-api-public/releases/latest">Download</a> ·
  <a href="docs/usage.md">Usage guide</a> ·
  <a href="README.zh-CN.md">简体中文</a> ·
  <a href="https://github.com/llxisdsh/const-api-public/issues">Get help</a>
</p>

CONST API connects each AI tool to the model you choose. Connect a tool once, then switch API keys, subscriptions, local models, or shared models without setting it up again. Available for Windows, macOS, and Linux.

## Start

1. [Download](https://github.com/llxisdsh/const-api-public/releases/latest) and open CONST API.
2. When **Local API · Running** appears, click your tool's icon. CONST API takes care of its connection settings.
3. Sign in to use models from the platform, or open **Supply** to add your own API key, subscription, or local model.

Need a manual connection or command-line mode? See the [usage guide](docs/usage.md).

Anything you add on the **Supply** page stays on this device by default. You decide whether to share it.

## What does it cost?

- **Your own API key, subscription, or local model:** you pay the provider or your own running cost. CONST API does not change that price.
- **Models from the platform:** open **Price reference** before you use them. CONST API prefers lower-cost options that are stable and available.
- The price for a request is fixed when it starts and the final charge follows its usage. If there is no clear price, the request is not sent.
- **Usage log** shows the recorded tokens and any platform charge.

## Is it private?

- Your tools connect to the local address `127.0.0.1:38787`.
- API keys and subscription credentials stay on the computer running the channel. They are not put into public catalogs.
- With your own channel, requests go directly from that computer to the provider or local model—without passing through the CONST API platform. With a platform model, requests pass through the platform to the supplier and model provider.
- Logs do not store prompts, response text, API keys, tokens, cookies, or authorization headers.
- Before changing a tool's settings, CONST API creates a backup. Canceling setup removes only CONST API's changes and keeps edits you made later.

## How does it work?

```text
AI tool → CONST API on your computer → your channel or a platform model → model provider
```

Your tool always talks to the same local address. CONST API finds a model you enabled and returns the result in the OpenAI, Anthropic, or Gemini format the tool expects.

Models, prices, tool support, and platform addresses can update without reinstalling the app. Every update is checked before use.

## Open source

The open-source release is in preparation.
