# CONST API usage guide

[README](../README.md) · [简体中文](usage.zh-CN.md) · [Source build](../SOURCE.md)

This guide covers the desktop app. Marketplace/account features apply only to
the official download; source builds provide the local edition.

## Install and start

Download an OS-appropriate package from [Releases](https://github.com/llxisdsh/const-api-public/releases/latest),
or follow the [local source build guide](../SOURCE.md). On macOS, move the app into
Applications before opening it. Follow OS security prompts only for a download
you trust; do not disable system protection globally.

Open the desktop application normally. Closing the window hides it in the tray
or menu bar; use **Quit** there to stop the gateway. The removed headless/check/help/version
command-line modes are not supported. The tray launch hint is internal to login startup.

## Configure a channel and a tool

1. Under **Models**, add your upstream API key, supported subscription, or local server.
2. Refresh channel information to discover model metadata. A model test or full
   check makes inference requests and can consume upstream quota.
3. Enable the channel. Under **Use**, wait for **Local API · Running**.
4. Select a tool and apply its configuration, then restart that tool when prompted.
   Use **Cancel configuration** to restore its previous managed settings.

Channel availability and tool support are different: a successful text request
does not prove support for images, tools, or every Responses feature. Refer to
channel checks and the final request error.

Your provider credentials belong in the channel. The key shown under **Use** is
the local gateway key and belongs in manually configured tools.

Managed setup backs up changed files and preserves unrelated later edits when
restoring. Do not use the official and local editions to manage the same tool at
the same time. For Codex, keep its existing home/session directory; do not delete
session files if changing provider makes history appear missing.

## Manual API connection

Always copy the current address and key from the app. Default ports are
`38787` for the official app and `38789` for source builds.

| Native protocol | Base URL for a source build | Example operation |
| --- | --- | --- |
| OpenAI | `http://127.0.0.1:38789/v1` | `/models`, `/responses`, `/chat/completions` |
| Anthropic | `http://127.0.0.1:38789/anthropic` | `/v1/messages` |
| Gemini | `http://127.0.0.1:38789/gemini` | `/v1beta/models/{model}:generateContent` |

For example, list models using your local key:

```sh
curl http://127.0.0.1:38789/v1/models -H "Authorization: Bearer <local-api-key>"
```

On Windows PowerShell, use `curl.exe` if `curl` is an alias. Replace placeholders
locally, and keep real keys out of shared shell history, screenshots, and issues.
Use a returned model ID, not a guessed display name. HTTP and supported WebSocket
operations share the same local port.

## Local routing and LAN sharing

Own channels send requests directly to their configured upstream. The local
edition never falls back to the hosted marketplace. In the official app, local
priority and compatible-model substitution depend on your settings.

To share locally, open **LAN sharing**, select the allowed models, and create a
member key and usage limit. Use the address shown in the dialog and allow access
only from a trusted subnet in your firewall. Local HTTP traffic is not encrypted;
do not expose the listener to the public Internet.

## Official Model Marketplace

Sign in to the official app to use or supply marketplace models. Review available
prices and your route settings before use. Marketplace requests pass through the
platform and selected supplier; provider credentials remain on the supplying
machine. Quota, model capabilities, availability, and price can affect selection.
The public source edition does not include this service.

## Files and troubleshooting

Official data is under `~/.const-api`; local-edition data is under `~/.const-api-local`.
The directory contains configuration, managed-tool backups and ownership metadata,
and rolling logs. Provider tokens and imported channel files are sensitive.

| Problem | What to check |
| --- | --- |
| Local API is not running | Check the displayed port, conflicts/reserved ports, and recent runtime logs. Restart the app after resolving the cause. |
| Invalid API key | Use the key from **Use**, not the provider key. Reapply tool configuration after changing the local key. |
| No model / no usable route | Enable a healthy channel; check its exact model IDs, quota, concurrency, and required capabilities. Refresh metadata after upstream changes. |
| Tool settings do not take effect | Close the tool fully and reopen it. If needed, cancel its managed configuration, restart in its normal mode, then configure again. |
| Codex history appears missing | Keep the original session directory. Reapply the intended provider or cancel managed configuration; do not delete history. |
| A source build asks about platform access | Built-in hosted features are unavailable; use your own upstream channel or the official app. |

Standard logs are for diagnostics, not prompt/response archiving. An explicitly
enabled raw-traffic debug build can contain sensitive bodies: never upload those
logs unreviewed. For bug reports include edition, version, OS, reproduction steps,
and redacted errors. Do not upload complete account/configuration files.
