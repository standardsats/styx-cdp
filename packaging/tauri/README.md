# STYX desktop (Tauri)

A native window around the bundled `styx-app`. The window is a webview pointed at the
app's own loopback server - the same UI the browser shows, so there is no separate
frontend to maintain or to let drift.

## Build (needs the GUI toolchain)

Not part of `cargo build --workspace`: Tauri needs the platform webview stack
(webkit2gtk on Linux, WebView2 on Windows, WKWebView on macOS) and the Tauri CLI.

```bash
cargo install tauri-cli --version '^2'
cd packaging/tauri
cargo tauri build          # bundles for the host platform
```

The build expects a `styx-app` binary alongside the bundle (CI stages the release binary
into the bundle's resources). At runtime the shell spawns it on a fixed loopback port and
navigates the webview there; `STYX_APP_CONFIG` overrides the config path.

## Why a webview over the loopback server, not embedded assets

The security model is the app's: loopback bind, session token, Host/Origin gate, CSP. A
webview pointed at that origin inherits all of it unchanged. Embedding the assets in the
desktop binary instead would fork the served UI and its hardening - this way the desktop
and the browser are byte-identical.
