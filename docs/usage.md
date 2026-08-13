# CONST API usage guide

[Back to README](../README.md) · [简体中文](usage.zh-CN.md)

This guide covers the public desktop release: installation, the local API, managed tool setup and recovery, model channels, manual API calls, and command-line operation. It does not require access to the private development repository.

## Contents

- [Install](#install)
- [First run](#first-run)
- [Use the local API](#use-the-local-api)
- [Configure a managed tool](#configure-a-managed-tool)
- [Codex session continuity](#codex-session-continuity)
- [Connect manually](#connect-manually)
- [Add a model channel](#add-a-model-channel)
- [Advanced local settings](#advanced-local-settings)
- [Command-line operation](#command-line-operation)
- [Local files and privacy](#local-files-and-privacy)
- [Troubleshooting](#troubleshooting)

## Install

Download only from the [CONST API public releases](https://github.com/llxisdsh/const-api-public/releases/latest).

| Platform | Package | Notes |
| --- | --- | --- |
| Windows x64 | `const-api_<version>_windows-x86_64-setup.exe` | Per-user installer by default |
| macOS Apple Silicon | `const-api_<version>_macos-aarch64.dmg` | M-series Macs |
| macOS Intel | `const-api_<version>_macos-x86_64.dmg` | Intel Macs |
| Linux x64 | `const-api_<version>_linux-x86_64.AppImage` | Make the AppImage executable before launching if required |

> [!WARNING]
> Current packages are unsigned trusted-tester builds. There is no Apple Developer ID/notarization, Windows publisher signature, or Linux package signature. macOS uses an ad-hoc bundle signature, and automatic-update artifacts use a Tauri updater signature; neither identifies an operating-system publisher. Continue only when the file came from this repository.

### macOS first launch

1. Drag `CONST API.app` from the DMG to **Applications**.
2. Try to open it once.
3. If macOS blocks it, open **System Settings → Privacy & Security**.
4. Find the CONST API notice, choose **Open Anyway**, and confirm.

A later unsigned build may require the same approval again.

## First run

1. Open CONST API normally.
2. The desktop window starts the same runtime used by tray and CLI modes.
3. Open **Use** and wait for **Local API · Running**.
4. Copy the local API key shown on that page when a tool asks for a key. Do not substitute a provider key there.

The client can start in a degraded state when no generation route is currently available. In that state the process, local health endpoint, configuration UI, and diagnostics still work, but a generation request will return a clear routing error until a usable local or remote channel exists.

Signing in is optional for local-only use. It is required for account features and for using or sharing channels through the Model Marketplace.

Closing the desktop window does not stop the runtime; it hides the window in the Windows/Linux tray or macOS menu bar. Use the tray/menu-bar **Quit** command to stop it completely.

## Use the local API

The **Use** page exposes three native Base URLs and one local API key.

| Protocol | Base URL | Common path |
| --- | --- | --- |
| OpenAI | `http://127.0.0.1:38787/v1` | `/models`, `/responses`, `/chat/completions` |
| Anthropic | `http://127.0.0.1:38787/anthropic` | `/v1/models`, `/v1/messages` |
| Gemini | `http://127.0.0.1:38787/gemini` | `/v1beta/models`, `/v1beta/models/{model}:generateContent` |

The URL selector changes what is copied; it does not collapse the three protocols into one. Use the protocol the tool natively supports. For OpenAI-compatible tools that require a complete Chat Completions URL instead of a Base URL, use:

```text
http://127.0.0.1:38787/v1/chat/completions
```

If a tool requires a handwritten model list, first read the corresponding `/models` endpoint. Managed OpenCode, Claude Desktop, VS Code, and WorkBuddy setup performs the required live model projection automatically.

The remaining Use-page actions are:

- **Advanced Settings** — local listening, LAN access, and compatibility-routing policy.
- **Connection Guide** — copy-ready protocol-specific values and manual setup hints.
- **Marketplace Pricing** — current catalog prices and available marketplace offers.
- **Usage Log** — privacy-filtered request metadata and route results.

## Configure a managed tool

The icon row currently manages ChatGPT/Codex, Claude Code, Claude Desktop, Gemini CLI, OpenCode, OpenClaw, Hermes, VS Code, and WorkBuddy.

### Common workflow

1. Click a tool icon to open its menu.
2. If the detected program is wrong or absent, choose **Locate program** and select the real executable or application bundle.
3. If the tool offers more than one native protocol, select the intended protocol before applying configuration.
4. Choose **Configure and restart**. If the tool is running, confirm the exact process CONST API discovered and allow it to close.
5. Wait for configuration write, readback, and launch confirmation to complete.
6. To stop using CONST API later, choose **Cancel configuration**. This restores fields still owned by CONST API and preserves later user edits.

**Launch** starts the program without changing configuration. Tool icons and the menu use the same launcher. **Configure and restart** is one backend operation, not an uncoordinated write followed by a second launch command.

The close confirmation is intentionally bound to a one-time process snapshot. If the process changed while the dialog was open, the operation asks again instead of closing an unrelated new process. If automatic close cannot be confirmed, close the tool yourself and continue; the configuration is not falsely reported as complete.

### Managed files and protocols

| Tool | Default managed location | Protocol |
| --- | --- | --- |
| ChatGPT / Codex | `~/.codex/auth.json`, `~/.codex/config.toml` | OpenAI Responses |
| Claude Code | `~/.claude/settings.json`, `~/.claude.json` | Anthropic Messages |
| Claude Desktop | Platform Claude / Claude-3p configuration library | Anthropic Messages |
| Gemini CLI | `~/.gemini/.env`, `~/.gemini/settings.json` | Gemini native |
| OpenCode | `~/.config/opencode/opencode.json` | Responses or Chat; Responses by default |
| OpenClaw | `~/.openclaw/openclaw.json` | Responses, Chat, Messages, or Gemini; Responses by default |
| Hermes | `~/.hermes/config.yaml`; Windows also checks configured/local application locations | OpenAI Chat |
| VS Code | Platform VS Code User `chatLanguageModels.json` and `settings.json` | Responses or Chat; Responses by default |
| WorkBuddy | `~/.workbuddy/models.json` by default | OpenAI Chat |

On Windows, `~` means the current user's profile directory. Platform-specific application configuration directories are discovered by the client.

### What a managed write guarantees

- JSON, JSON5, YAML, and TOML are parsed before mutation. A file that cannot be parsed is not overwritten.
- Only owned fields and named provider entries are changed; unrelated providers, MCP servers, extensions, and UI settings are preserved.
- Changed files receive a backup under `~/.const-api/backups`.
- Field ownership, the pre-CONST value, and the last applied value are recorded in `~/.const-api/tool-config-manifest.json`.
- Files are replaced atomically and read back before success is reported.
- Reapplying an identical configuration does not create pointless writes or backups.

**Cancel configuration** is a field-level three-way restore:

| Current state | Result when canceling |
| --- | --- |
| Field still equals the last CONST API value | Restore its original value, or remove it if it did not exist |
| User changed the managed field later | Keep the user's current value and release ownership |
| User changed an unrelated field | Keep the unrelated edit while restoring fields still owned by CONST API |
| CONST API created a named provider entry | Remove only that logical entry; preserve sibling entries |

Claude Code and Gemini CLI have one additional runtime layer: when CONST API launches them, it injects only the required gateway variables into that child process and removes known conflicting provider variables. It does not change system environment variables, shell profiles, or the parent process.

VS Code setup manages only the built-in BYOK provider and the required utility-model setting. It does not install, enable, disable, or update extensions.

WorkBuddy setup writes the current model list, context/output limits, vision/tool/reasoning capability metadata, and supported reasoning-effort choices where the catalog provides evidence. It does not invent options for models without that metadata.

## Codex session continuity

Codex associates sessions with a `model_provider`. Changing only `config.toml` can therefore make historical sessions appear missing even when their JSONL files still exist.

CONST API treats configuration and session-provider migration as one atomic tool operation:

- Setup migrates provider metadata in visible `~/.codex/sessions`, archived `~/.codex/archived_sessions`, and the Codex SQLite thread index to `CONST_API`.
- Canceling configuration first restores the effective Codex provider, then migrates all CONST-owned sessions—including sessions created while CONST API was active—to that provider.
- Conversation text, tool calls, all non-metadata JSONL lines, newline style, and the official OpenAI login cache are not changed.
- JSONL replacement is read back byte-for-byte; failed verification restores the original content. SQLite state and sidecars receive bounded backups.

Do not manually move sessions between alternate `CODEX_HOME` directories when using managed setup. CONST API deliberately keeps the official home and login cache in place so the same machine has one session history.

## Connect manually

Use the key currently shown on the **Use** page. The examples below use placeholders.

### OpenAI-compatible models

```bash
curl http://127.0.0.1:38787/v1/models \
  -H "Authorization: Bearer <local-api-key>"
```

### OpenAI Responses

```bash
curl http://127.0.0.1:38787/v1/responses \
  -H "Authorization: Bearer <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","input":"Reply with: connected"}'
```

### OpenAI Chat Completions

```bash
curl http://127.0.0.1:38787/v1/chat/completions \
  -H "Authorization: Bearer <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","messages":[{"role":"user","content":"Reply with: connected"}]}'
```

### Anthropic Messages

```bash
curl http://127.0.0.1:38787/anthropic/v1/messages \
  -H "x-api-key: <local-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model-id>","max_tokens":64,"messages":[{"role":"user","content":"Reply with: connected"}]}'
```

### Gemini native

```bash
curl "http://127.0.0.1:38787/gemini/v1beta/models/<model-id>:generateContent" \
  -H "x-goog-api-key: <local-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"contents":[{"role":"user","parts":[{"text":"Reply with: connected"}]}]}'
```

On PowerShell, use `curl.exe` if `curl` is aliased to another command, and adjust shell quoting as needed. Never place a real key into a committed script, issue, screenshot, or terminal transcript you plan to share.

## Add a model channel

Open **Models**, select **Add Channel**, and choose a source type.

### API credentials

Supported source drivers include OpenAI, OpenRouter, Azure OpenAI, Anthropic, Gemini, Amazon Bedrock through Mantle-compatible surfaces, custom endpoints, and local OpenAI-compatible runtimes. The form shows only fields relevant to the selected driver's authentication and discovery contract.

1. Enter the upstream URL and credential where required.
2. Run detection before enabling the channel.
3. Review discovered surfaces, real model IDs, context/output limits, capability evidence, and any quota information.
4. Enable the channel locally.
5. If signed in and remote routing is desired, explicitly enable platform participation.

### Subscription adapters

Supported subscription adapters use their own local OAuth/import flow. Account tokens remain on the client, are refreshed there, and are not copied into the supplier registration payload. Model discovery and quota windows are reported from the real upstream when available.

### Local models

Ollama, LM Studio, vLLM, and compatible local endpoints can be added as local channels. An enabled exact-model local channel is checked before platform routing; it can serve the request without signing in or contacting the server. Local-only channels are never advertised as shared supply.

### Status vocabulary

- **Local ready** — usable on this device, not shared.
- **Platform online** — the channel is registered through the active supplier connection and is eligible according to its limits.
- **Cooldown / authentication / quota / risk** — a specific safety state; open channel details before retrying.
- **Available models** — models that passed source discovery and local validation, not a hand-maintained marketing list.

One supplier-agent connection is maintained per selected server endpoint, even with multiple enabled channels. It prefers QUIC and falls back to WebSocket automatically.

## Advanced local settings

The secure default is loopback-only listening on `127.0.0.1:38787`.

To use CONST API from another device on a trusted LAN:

1. Open **Advanced Settings** and explicitly enable LAN listening.
2. Bind to the intended private interface or `0.0.0.0` only when necessary.
3. Allow the selected TCP port through the host firewall only for the trusted subnet.
4. Use `http://<host-lan-ip>:38787/...` from the other device and protect the local API key.

Managed tools on the same machine continue to use the loopback URLs. Do not expose port `38787` directly to the public internet. WebSocket operations on the local API use the same HTTP listener; they do not require a separate local port.

Compatibility-model routing is also an explicit advanced option. Leave it disabled when the requested model identity must be exact. When enabled, the server may select only models in the currently signed compatibility group; it does not treat arbitrary name similarity as compatibility.

## Command-line operation

The release contains one executable. It accepts at most one public option.

| Command | Behavior |
| --- | --- |
| `const-api-client` | Desktop mode. Open the window; closing it keeps the runtime in the tray/menu bar |
| `const-api-client --tray` | Start the desktop host hidden with a tray/menu-bar entry; no daemon fork |
| `const-api-client --headless` | Run in the current terminal with the same saved configuration; no UI or tray; press Ctrl+C to stop |
| `const-api-client --check` | Validate saved configuration, query any active runtime safely, print the result, and exit; no services are started |
| `const-api-client --help` | Print the supported syntax |
| `const-api-client --version` | Print the client version |

### Windows PowerShell

The default per-user installation is typically:

```powershell
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --help
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --check
& "$env:LOCALAPPDATA\CONST API\const-api-client.exe" --headless
```

Adjust the path if you selected another install location.

### macOS

For a foreground headless process, execute the binary inside the app bundle directly:

```bash
/Applications/CONST\ API.app/Contents/MacOS/const-api-client --check
/Applications/CONST\ API.app/Contents/MacOS/const-api-client --headless
```

Do not use `open -a "CONST API" --args --headless` for the documented foreground workflow; `open` detaches from the terminal, so Ctrl+C and terminal log behavior differ.

### Linux

Run the extracted binary or AppImage path directly. For example:

```bash
chmod +x ./const-api_*_linux-x86_64.AppImage
./const-api_*_linux-x86_64.AppImage --check
./const-api_*_linux-x86_64.AppImage --headless
```

Headless mode does not require `DISPLAY` or `WAYLAND_DISPLAY`; tray mode does require a graphical session.

### One active runtime

A configuration directory has at most one active runtime:

| New invocation | Existing desktop/tray | Existing headless |
| --- | --- | --- |
| Desktop, no option | Wake the existing window and exit | Report a conflict and exit `2` |
| `--tray` | Do nothing and exit `0` | Report a conflict and exit `2` |
| `--headless` | Report a conflict and exit `2` | Report a conflict and exit `2` |
| `--check` | Inspect safely and exit | Inspect safely and exit |

Exit codes are:

- `0` — success or normal shutdown;
- `1` — configuration, listener, or runtime failure;
- `2` — invalid arguments or an active-runtime mode conflict.

Start-at-login uses `--tray`, not `--headless`. Headless mode is intentionally foreground-only and does not accept credentials on the command line.

## Local files and privacy

| Path or storage | Purpose |
| --- | --- |
| `~/.const-api/client.json` | Listening settings, verified endpoint cache projection, channel configuration, account platform identity, and local device API key |
| Operating-system credential store (`CONST API account`) | Account refresh token; access tokens stay in process memory |
| `~/.const-api/tool-config-manifest.json` | Field-level ownership needed for safe cancellation |
| `~/.const-api/backups` | Bounded backups of changed tool configuration and relevant state databases |
| `~/.const-api/state` | Active-runtime lock and authenticated loopback control metadata; no provider credentials |
| `~/.const-api/logs` | Rotating operational logs |

Renderer reads filter sensitive configuration fields. Production call logs contain route/protocol/model/status/byte/token/latency/cost/error/stream metadata, but not request or response bodies, tool arguments, cookies, authorization headers, or provider keys. Raw protocol logging is unavailable in release builds.

Requests still have to reach the selected provider or supplier to be executed; the privacy guarantee is data minimization in configuration exchange, registration, diagnostics, and persistence—not a claim that model inference occurs without transmitting its input to the selected execution endpoint.

## Troubleshooting

### The application is blocked on first launch

Confirm the asset came from this repository. Follow the operating-system trusted-tester flow described in [Install](#install); do not disable system security globally.

### `Local API` is not running

1. Run `const-api-client --check` from a second terminal.
2. Check `~/.const-api/logs` or run `--headless` when no desktop/tray runtime is active.
3. Look for a port conflict on `38787`, an invalid listen address, or an unreadable `client.json`.
4. An unavailable model route is different from a listener failure: degraded mode can still report the local API as running.

### `invalid API key`

Use the **local API key from the Use page** in the tool. Provider keys belong in their model channels. If the local key changed, reapply managed tool configuration or update the manual client.

### A tool has no models or requires a model list

Open `/models` for that tool's native surface and confirm the expected IDs are present. Managed tools that require explicit models read the current verified snapshot during configuration; if the catalog or endpoint was temporarily unavailable, restore connectivity and configure again. Do not invent a model name from a display label.

### Configure-and-restart cannot confirm that the tool closed

Close the real tool and its session-writing helper processes manually, then continue. If discovery selected the wrong program, cancel and use **Locate program**. The bounded close check exists to avoid writing while a tool is still saving configuration or session state.

### A tool configuration file is invalid

Fix the original JSON/JSON5/YAML/TOML syntax first. CONST API refuses to overwrite an unparsable existing file. Review `~/.const-api/backups` before making a manual replacement.

### Codex sessions seem to disappear

Do not delete the JSONL files or switch to a second `CODEX_HOME`. Open the ChatGPT/Codex tool details and reapply the intended provider, or use **Cancel configuration** to restore the previous provider. Managed migration updates session-provider metadata and the thread index together.

### A request has no eligible route

Check, in order:

1. whether an exact model is enabled locally;
2. whether a provider/subscription channel is healthy and within quota;
3. whether the account and public endpoint are reachable for platform routing;
4. whether the request needs a capability the available supply has not declared;
5. whether compatibility routing is intentionally enabled for that model group;
6. whether the configured price ceiling excludes every candidate.

Use **Usage Log**, **Supply Log**, and the route trace to find the exact layer that rejected the request.

## Current release boundary

This public repository distributes the desktop packages, updater metadata, and signed endpoint/catalog registries. Source licensing and the contribution workflow are still being prepared. See [Open source](../README.md#open-source) before assuming source availability or redistribution rights.
