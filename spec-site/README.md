# STYX v1 specification site

The protocol specification for the frozen v1 covenant CDP. Plain static HTML + CSS, no build
step: open `index.html` in a browser (works from `file://`). This site is the normative source of
truth for v1; it references nothing beyond itself and the covenant sources. Covenant `.simf` file:line chips are fine (the covenant
source ships with the deployed artifacts, CMR-pinned).

- `index.html` .. `glossary.html` - the eleven chapters.
- `diagram.html` - the interactive architecture diagram (standalone).
- `styles.css`, `nav.js`, `fonts.css`, `fonts/`, `assets/` - shared chrome. Fonts (GFS Didot,
  Source Serif 4, IBM Plex Mono) are self-hosted so the site versions with the repo and works
  offline. Navigation lives in one place, the `PAGES` list in `nav.js`.

Maintenance rules:

- The spec is normative for the public, and it must stay in exact agreement with the frozen
  covenant. Before changing any behavioural or numeric claim, check it against the `.simf`
  asserts in `covenants/`.
- The frozen CMRs live on the deployment page; if they ever change, the covenant was re-frozen
  and the spec needs a version bump (a v2 gets its own spec, not an edit of this one).
- Doc style: human-facing prose, no em-dashes, ASCII `->`, no AI-isms.
