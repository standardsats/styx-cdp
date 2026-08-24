# STYX landing

The public entry point for the protocol: one page that routes to the specification, the
explorer, the covenant source, and the code. Plain static HTML + CSS, no build step, no JS - open
`index.html` in a browser (works from `file://`, though the cross-surface links below only
resolve once deployed).

Same obsidian-water chrome as `spec-site/`: `fonts/`, `fonts.css`, and `assets/meander.svg`
are copies so the landing deploys standalone. If the spec-site chrome changes, re-sync them.

## Links and deploy layout

The hrefs assume the landing is the apex home (e.g. `styx.network`) with the spec served
same-origin under `/spec/`, and the explorer on its own testnet host:

- `spec/` -> the specification site (`spec-site/` in this repo).
- `spec/source.html` -> the covenant source chapter.
- `https://explorer.testnet.styx.network` -> the read-only explorer (matches `DEPLOY.md`).
- `https://github.com/standardsats/styx-cdp` -> this repository (topbar, gateway card, footer).
- `testnet.html` -> the deployment record: the ceremony result in prose (genesis, asset ids,
  issuer anchor, the five oracle keys, the relay) and where to get `liquid-testnet.toml`.
  Its values are a copy of `deploy/liquid-testnet.toml`, so a re-deploy has to update both -
  the publish step in `DEPLOY.md` lists every place.

The explorer surfaces per-slot oracle freshness, so there is no separate oracle gateway -
the oracles have no public HTTP by design (reached over the relay; admin ports stay
loopback). If you serve the spec-site somewhere other than `/spec/`, update the spec hrefs
in `index.html` (topbar nav + gateway cards).

## Rules

- Numeric and behavioural claims (CR bands, quorum, supply, freeze date) must stay in exact
  agreement with the frozen covenants and the spec-site; check against them before editing.
- Doc style: human-facing prose, no em-dashes, ASCII `->`, no AI-isms.
