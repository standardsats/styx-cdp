//! Server-rendered HTML: the whole page is a pure function of one `View`. No script ships
//! (a meta refresh keeps it current) and the CSP has no script-src at all, so there is
//! nothing to inject into. Values are integers and hex strings from our own types; the one
//! escape point is kept anyway for the day a config string lands on the page.

use crate::state::View;

const CSS: &str = include_str!("../assets/explorer.css");

/// The hardening headers every response carries. No scripts exist, so none are allowed.
pub const CSP: &str =
    "default-src 'none'; style-src 'self'; font-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'";

pub fn css() -> &'static str {
    CSS
}

/// A tiny self-hosted favicon (the gold meander three-bar mark on obsidian), served at
/// /favicon.svg so it stays same-origin under the `img-src 'self'` CSP.
pub const FAVICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\
<rect width=\"64\" height=\"64\" rx=\"14\" fill=\"#0b0e12\"/>\
<g fill=\"#c8a45c\"><rect x=\"16\" y=\"17\" width=\"32\" height=\"5\"/>\
<rect x=\"21\" y=\"29\" width=\"22\" height=\"5\"/><rect x=\"16\" y=\"42\" width=\"32\" height=\"5\"/></g></svg>";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn fmt(n: u64) -> String {
    // Thousands separators, locale-free.
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// OBOL is denominated in cents; show it as dollars so the debt reads like a price, not a
/// raw cent count ($1,000.00, not 100,000).
fn usd(cents: u64) -> String {
    format!("${}.{:02}", fmt(cents / 100), cents % 100)
}

fn short(outpoint: &str) -> String {
    // elements' OutPoint Display is "[elements]<txid>:<vout>"; show the txid head, not the prefix.
    let (txid, vout) = outpoint.rsplit_once(':').unwrap_or((outpoint, "?"));
    let txid = txid.strip_prefix("[elements]").unwrap_or(txid);
    format!("{}:{}", &txid[..txid.len().min(8)], vout)
}

/// The page. `app_url` is the one optional external reference (the "open a vault"
/// call-to-action pointing at the styx-app download); absent, the section is omitted.
pub fn page(v: &View, app_url: Option<&str>) -> String {
    let mut html = String::with_capacity(16 * 1024);
    html.push_str(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"refresh\" content=\"10\">\
         <title>STYX explorer</title>\
         <link rel=\"icon\" href=\"/favicon.svg\">\
         <link rel=\"stylesheet\" href=\"/explorer.css\"></head><body>",
    );
    html.push_str("<header><h1>\u{3a3}\u{3a4}\u{3a5}\u{39e} <span class=\"sub\">explorer</span></h1>");
    html.push_str(&format!("<div class=\"chainline\"><span>height <b>{}</b></span>", v.height));
    match &v.tick {
        Some(t) if t.lo == t.hi => {
            html.push_str(&format!("<span>price <b>${}</b></span>", fmt(t.hi as u64)))
        }
        Some(t) => html.push_str(&format!(
            "<span>price <b>${} - ${}</b></span>",
            fmt(t.lo as u64),
            fmt(t.hi as u64)
        )),
        None => html.push_str("<span>price <b>no quorum</b></span>"),
    }
    // How current the pricing (and therefore the vault bands) is.
    if let Some(t) = &v.tick {
        let cls = if v.pricing_stale { "stale" } else { "" };
        html.push_str(&format!(
            "<span class=\"tickage {}\">tick h<a href=\"https://liquid.network/testnet/block-height/{}\" \
             target=\"_blank\" rel=\"noopener\">{}</a> &middot; {}s ago{}</span>",
            cls,
            t.height,
            t.height,
            t.age_secs,
            if v.pricing_stale { " (stale)" } else { "" },
        ));
    }
    if let Some(p) = &v.protocol {
        html.push_str(&format!(
            "<span>pot <b>{}</b></span><span>reserve <b>{}</b> sats</span>\
             <span>anchor <b>{}</b></span>",
            usd(p.pot_units),
            fmt(p.reserve_sats),
            p.issuer_anchor,
        ));
    }
    html.push_str("</div></header><main>");

    // The vault table, most endangered first (the view pre-sorts by CR).
    html.push_str("<section class=\"card wide\"><h2>Vaults</h2>");
    if v.vaults.is_empty() {
        html.push_str("<p class=\"muted\">no open vaults</p>");
    } else {
        html.push_str(
            "<table><thead><tr><th>outpoint</th><th>debt (OBOL)</th><th>collateral (sats)</th>\
             <th>CR</th><th>band</th><th>ratchet</th></tr></thead><tbody>",
        );
        for vt in &v.vaults {
            let cr = vt.cr_percent.map(|c| format!("{c}%")).unwrap_or_else(|| "-".into());
            // On a stale tick the band may lag the chain; dim it so it does not read as a
            // confident current verdict.
            let band_cls = if v.pricing_stale { "band stale" } else { "band" };
            html.push_str(&format!(
                "<tr><td><a href=\"https://liquid.network/testnet/tx/{}\" target=\"_blank\" \
                 rel=\"noopener\">{}</a></td><td>{}</td><td>{}</td><td>{}</td>\
                 <td><span class=\"{} {}\">{}</span></td><td>{}</td></tr>",
                esc(&vt.txid),
                esc(&short(&vt.outpoint)),
                usd(vt.debt_units),
                fmt(vt.collateral_sats),
                cr,
                band_cls,
                vt.band,
                vt.band,
                vt.last_height,
            ));
        }
        html.push_str("</tbody></table>");
        html.push_str(
            "<p class=\"muted\">bands are priced at the current tick's max quote; acting on \
             a vault additionally needs a tick above its own ratchet height.</p>",
        );
    }
    if v.lost > 0 {
        html.push_str(&format!("<p class=\"muted\">{} lost outpoint(s) tracked</p>", v.lost));
    }
    html.push_str("</section>");

    // Singletons and oracles share one full-width row (their own two-column grid) so the
    // relay address has room to sit on one line.
    html.push_str("<div class=\"pair\">");

    // The protocol singletons, linked to the public explorer so anyone can confirm the
    // indexer is tracking the real on-chain outputs.
    if let Some(p) = &v.protocol {
        html.push_str(
            "<section class=\"card\"><h2>Singletons</h2>\
             <p class=\"muted\">the protocol's on-chain outputs - open each on \
             liquid.network to verify</p><table><tbody>",
        );
        for s in &p.singletons {
            html.push_str(&format!(
                "<tr><td>{}</td><td><a href=\"https://liquid.network/testnet/tx/{}\" \
                 target=\"_blank\" rel=\"noopener\">{}</a></td></tr>",
                s.name,
                esc(&s.txid),
                esc(&short(&s.outpoint)),
            ));
        }
        html.push_str("</tbody></table></section>");
    }

    // Oracle recency, name, and last price, from the public relay.
    html.push_str("<section class=\"card\"><h2>Oracles</h2>");
    if let Some(relay) = &v.relay {
        html.push_str(&format!("<p class=\"muted\">quote relay: <code>{}</code></p>", esc(relay)));
    }
    html.push_str("<div class=\"slots\">");
    for (slot, age) in v.oracle_age_secs.iter().enumerate() {
        let (cls, text) = match age {
            Some(a) if *a <= 120 => ("ok", format!("{a}s ago")),
            Some(a) => ("stale", format!("{a}s ago")),
            None => ("never", "never".into()),
        };
        // The oracle's own name if it published one, else the bare slot.
        let label = v.oracle_name[slot].as_deref().map(esc).unwrap_or_else(|| format!("slot {slot}"));
        let price = match v.oracle_price[slot] {
            Some(p) => format!("<br>${}", fmt(p as u64)),
            None => String::new(),
        };
        html.push_str(&format!("<span class=\"slot {cls}\">{label}<br><b>{text}</b>{price}</span>"));
    }
    html.push_str("</div></section>");
    html.push_str("</div>"); // close .pair

    // The event feed.
    html.push_str("<section class=\"card wide\"><h2>Events</h2>");
    if v.events.is_empty() {
        html.push_str("<p class=\"muted\">nothing yet</p>");
    } else {
        html.push_str("<ul class=\"events\">");
        for e in &v.events {
            html.push_str(&format!(
                "<li><span class=\"h\">{}</span> <span class=\"tx\">{}</span> {}</li>",
                e.height,
                esc(&e.txid[..8.min(e.txid.len())]),
                esc(&e.what),
            ));
        }
        html.push_str("</ul>");
    }
    html.push_str("</section>");

    if let Some(url) = app_url {
        html.push_str(&format!(
            "<section class=\"card\"><h2>Open a vault</h2><p class=\"muted\">the wallet and the \
             keeper run on YOUR machine - keys never leave it.</p>\
             <p><a class=\"cta\" href=\"{}\">get styx-app</a></p></section>",
            esc(url)
        ));
    }
    html.push_str("</main></body></html>");
    html
}
