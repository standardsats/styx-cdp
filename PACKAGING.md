# Packaging

Three shapes for the same binaries, all provenance-tagged by the flake rev (the container
images already do this; the release artifacts and the app packages join them):

- **Per-platform release binaries** - the CLIs and daemons, built by CI, checksummed.
- **Umbrel / Start9** - styx-app plus its node, for the self-hosting audience (the node
  runners are the keeper audience).
- **Tauri desktop** - the U1 UI in a native shell for people who will not run a terminal.

The signing seam under all of them: the vault owner key can live off the online machine.
`styx_pset::signing` exports the owner digest (`OwnerSigningRequest`), an external device
signs it, and `apply_owner_sig` verifies and installs the answer before broadcast - the
funding key stays hot, the authorizing key need not. This is what a hardware-wallet or
air-gapped signer integration builds on; the crate is the tested core, the device transport
is out of scope here.

## Release binaries

`.github/workflows/release.yml` builds on tag. Each artifact is named with the tag and the
short rev, and a SHA256SUMS file is attached - the same "the rev is the provenance" the
images carry. Reproduce any artifact by checking out its rev and `cargo build --release`
(or `nix build .#styx`, which is bit-for-bit from the flake).

## Umbrel / Start9

`packaging/umbrel/` and `packaging/start9/` hold the manifests and their compose files. Both
run two containers from the flake images (`styx-elementsd` + `styx-app`, published per the
`deploy/docker` README) and reach the app through the platform's authenticated proxy (Tor /
the platform's auth), never a raw public bind.

The proxy is a separate container, so the app cannot bind loopback - the proxy would get
connection-refused, and even a reachable bind would 403 on the gate (the proxied Host and
Origin are not `127.0.0.1`). This is a real relaxation of the loopback rule, and the app has
an EXPLICIT seam for it, not a silent manifest override: an app.toml `[proxy]` section
(`bind` where the proxy reaches it, `allow_hosts` and `allow_origins` for the exact Host /
Origin the platform sends) is the one opt-in way past loopback-only - default-deny,
negative-tested like every other gate invariant (see `deploy/app.toml.example` and the
`security` tests). The scheme is configured, not assumed: Umbrel serves apps over plain
http on the LAN, Tor onion origins are http too, only a StartOS LAN cert is https - an
https-only rule would 403 two of the three real paths.

**Not yet verified on-platform.** The exact Host and Origin a live Umbrel / StartOS box
forwards (host with or without port, onion vs LAN name, scheme) have to be confirmed on the
box and pasted into `[proxy]` before this packaging is working, not merely plausible.
Building and submitting the manifests needs the respective app stores' tooling - an ops
step, not a repo build.

## Tauri desktop

`packaging/tauri/` is a thin shell: it starts `styx-app` as a child bound to a random
loopback port, waits for it, and points a webview at it - the exact same served UI as the
browser, no second frontend. Building it needs the platform webview toolchain
(webkit2gtk / WebView2 / WKWebView) and the Tauri CLI, so it is NOT part of `cargo build
--workspace` or the flake's default package; it is a separate `cargo tauri build` on a
machine with the GUI stack. The wrapper code and config are here and reviewed; the built
bundles come from CI runners with the toolchain, tagged by the same rev.

## What is verified in-repo vs. what needs its toolchain

- Verified here (fast tier): the signing seam round trip (`styx-pset` `signing` tests) and
  everything the app/explorer already prove.
- Not built in this sandbox (needs external toolchains, marked so): the Tauri bundles, the
  Umbrel/Start9 store submissions, the per-platform release binaries. Their sources and
  manifests live in `packaging/` and are pinned to the flake rev; CI produces the artifacts.
