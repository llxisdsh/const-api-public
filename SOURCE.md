# Build CONST API Local

[简体中文](SOURCE.zh-CN.md) · [README](README.md)

This source builds a desktop gateway for your own channels. It does not build
the platform-enabled application offered in this repository's Releases.

## Prerequisites

- Node.js matching `client/.node-version` and npm.
- Stable Rust, as selected by `client/rust-toolchain.toml`.
- [Tauri 2 native prerequisites](https://v2.tauri.app/start/prerequisites/):
  C++ Build Tools and WebView2 on Windows; Xcode Command Line Tools on macOS;
  a C/C++ toolchain, WebKitGTK 4.1, GTK 3, OpenSSL, librsvg, and AppIndicator
  development packages on Linux.

The lockfiles pin dependency resolution. No private checkout, server, production
environment file, signing key, or upstream mirror is needed.

## Run and build

From the repository root:

```sh
cd client
npm ci
npm run dev
```

To build an executable with the renderer embedded:

```sh
npm run build
```

By default, the output is `client/src-tauri/target/release/const-api-local`
(`const-api-local.exe` on Windows). `npm run build -- --debug` produces a faster
development build under `target/debug`. A custom `CARGO_TARGET_DIR` changes
these locations. The build uses `--no-bundle`: it does not create a signed
installer, updater artifact, or GitHub release.

The application needs a graphical session. Launch it normally; use the tray or
menu-bar **Quit** action to stop it. Unsupported legacy CLI modes are not part
of the interface.

## First local request

### Optional Antigravity OAuth configuration

Google OAuth application credentials are deliberately absent from the public
source. For Antigravity browser sign-in and token refresh, supply compatible
application credentials through `CONST_LOCAL_ANTIGRAVITY_CLIENT_ID` and
`CONST_LOCAL_ANTIGRAVITY_CLIENT_SECRET` in the build process environment. Restart
the build after changing them. They are compiled into that binary; this is not a
secure store for a confidential server application secret. Do not commit them,
place them in public CI, or distribute credentials you are not entitled to share.

Without this optional configuration, Antigravity sign-in and refresh fail locally
with an actionable message. An imported access token alone does not ensure it can
be refreshed. Other API channels do not need these values. A compatible client
registration and upstream entitlement are still required; an arbitrary Google
OAuth project is not guaranteed to grant Antigravity access.

### Configure your first channel

1. Add an API, supported subscription, or local model channel.
2. Refresh its model metadata, choose a model, and run a model test if needed.
   Tests can incur provider charges.
3. Enable the channel. Open **Use**, wait for the local API to start, and configure
   a tool or copy the displayed URL and local key.
4. Use an actual model ID from the local model list. Do not put the upstream
   provider key in your tool's local-gateway configuration.

Default data directory: `~/.const-api-local`. Default API: `127.0.0.1:38789`.
Application ID: `xin.const.api.local`. The official application uses a different
directory, ID, executable, and default port.

Do not let both editions manage the same external tool configuration at once.
Cancel managed setup in the current edition before configuring the other one;
back up important tool settings and session data.

## Checks

Run from `client/`:

```sh
npm run typecheck
npm run i18n:check
npm run test:renderer
cargo check --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo test --manifest-path src-tauri/Cargo.toml local_policy::tests -- --test-threads=1
```

The boundary tests check persisted configuration isolation, refusal of hosted
transports even with injected endpoints/tokens, and local forwarding for four
protocols, plus the missing OAuth-application error. They use local mocks, not paid
provider accounts. Public CI builds on
Windows, Linux, and macOS and runs the checks configured in its workflow.
Some retained native tests describe the platform edition, so public CI selects
the local-edition boundary tests rather than claiming the entire private suite.

## Platform boundary

- Platform account, credential-storage, installation-proof, and reward
  implementations are excluded. Their public interfaces return unavailable.
- Official discovery, updater sources, and platform credentials are disabled;
  loading copied configuration does not enable them.
- Hosted HTTP/HTTP3 and supplier WS/QUIC connection implementations are removed
  from this snapshot. This does not disable upstream API/SSE/WebSocket or LAN use.
- This is a product boundary, not an anti-tampering guarantee. A modified program
  can perform different network requests; upstream URLs remain user-configurable.
  A valid service credential may authorize generic API use independently of
  the built-in platform features. Hosted authorization is enforced server-side.

## Source and release policy

Snapshots are synchronized one way from the main private repository.
`public-source-manifest.json` records the source revision and exported-file hashes;
it does not include private history. The manifest is provenance metadata, not a
runtime authorization credential or signed release attestation.

Official installers are still built and published through the existing private
pipeline. Public CI has read-only repository permissions and never publishes
applications or update feeds. Accepted contributions are integrated upstream
before a new snapshot is exported.

See [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).
