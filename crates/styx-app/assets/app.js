// STYX app client. Vanilla, same-origin only: every request carries the session token in a
// header, so the event stream is consumed with fetch-based streaming (EventSource cannot
// set headers, and the token stays out of URLs).

"use strict";

const TOKEN = window.STYX_TOKEN;
const HEADERS = { "x-styx-token": TOKEN, "content-type": "application/json" };

const $ = (id) => document.getElementById(id);

function journal(kind, detail) {
  const li = document.createElement("li");
  li.className = kind;
  const k = document.createElement("span");
  k.className = "kind";
  k.textContent = kind;
  li.append(k, detail);
  $("journal").prepend(li);
  while ($("journal").children.length > 200) $("journal").lastChild.remove();
}

function alertBanner(text) {
  const el = $("alert");
  if (text) {
    el.textContent = text;
    el.classList.remove("hidden");
  } else {
    el.classList.add("hidden");
  }
}

async function api(path, body) {
  const opts = body === undefined
    ? { headers: HEADERS }
    : { method: "POST", headers: HEADERS, body: JSON.stringify(body) };
  const resp = await fetch(path, opts);
  const text = await resp.text();
  if (!resp.ok) throw new Error(text || resp.status);
  try { return JSON.parse(text); } catch { return text; }
}

function fmt(n) {
  return Number(n).toLocaleString("en-US");
}

// elements' OutPoint prints "[elements]<txid>:<vout>"; show the txid head, not the
// prefix. The full string stays the select value - the API parses it back as-is.
function short(outpoint) {
  const [txid, vout] = outpoint.replace("[elements]", "").split(":");
  return txid.slice(0, 8) + ":" + vout;
}

// One in-flight marker per action button: the spinner is the "it is working" answer,
// the journal line that follows is the verdict.
async function withBusy(btn, act) {
  if (btn.classList.contains("busy")) return;
  btn.classList.add("busy");
  btn.disabled = true;
  try {
    await act();
  } finally {
    btn.classList.remove("busy");
    btn.disabled = false;
  }
}

// A refusal where the user is looking: a warn strip right under the button that was
// pressed, on top of the journal line. Cleared by time or by the next attempt.
function flashError(btn, text) {
  let el = btn.parentElement.querySelector(".op-error");
  if (!el) {
    el = document.createElement("p");
    el.className = "op-error";
    btn.insertAdjacentElement("afterend", el);
  }
  el.textContent = text;
  clearTimeout(el.dismiss);
  el.dismiss = setTimeout(() => el.remove(), 15000);
}

function clearError(btn) {
  const el = btn.parentElement.querySelector(".op-error");
  if (el) el.remove();
}

// --- broadcast-to-block tracking --------------------------------------------------------
// A broadcast op is not a done op: the snapshot counts only confirmed blocks, so the
// successor outpoint appearing in status IS the confirmation (for close, the acted
// outpoint going away; for vault-less txes, the next block). Until then the header badge
// says a block is being awaited and the acted vault's buttons stay down - the ratchet
// wants a block between consecutive ops anyway.
let pendings = [];

function trackPending(op, r, acted) {
  const h = lastStatus ? lastStatus.height : 0;
  let done;
  if (r.vault) {
    done = (st) => st.vaults.some((v) => v.outpoint === r.vault.outpoint);
  } else if (acted) {
    done = (st) => !st.vaults.some((v) => v.outpoint === acted);
  } else {
    done = (st) => st.height > h;
  }
  pendings.push({ op, txid: r.txid, acted, done, expires: h + 5 });
  renderPending();
}

function renderPending() {
  const badge = $("pending-badge");
  badge.classList.toggle("hidden", pendings.length === 0);
  if (pendings.length) {
    badge.textContent = "awaiting block: " + pendings.map((p) => p.op).join(", ");
  }
  $("open-form").querySelector("button").disabled = pendings.some((p) => p.op === "open");
}

function settlePendings(st) {
  pendings = pendings.filter((p) => {
    if (p.done(st)) {
      journal("confirmed", p.op + " " + p.txid.slice(0, 8) + "… is in a block");
      return false;
    }
    if (st.height >= p.expires) {
      journal("op_error", p.op + " " + p.txid.slice(0, 8)
        + "… not seen after 5 blocks - check the explorer");
      return false;
    }
    return true;
  });
  renderPending();
}

function fillVaultSelect(id, vaults) {
  const select = $(id);
  const chosen = select.value;
  select.textContent = "";
  select.append(new Option("(only vault)", ""));
  for (const v of vaults) {
    select.append(new Option(short(v.outpoint), v.outpoint));
  }
  if (chosen) select.value = chosen;
}

function renderVaults(st) {
  const tbody = $("vaults").querySelector("tbody");
  tbody.textContent = "";
  for (const v of st.vaults) {
    const tr = document.createElement("tr");
    const cr = v.cr_percent == null ? "-" : v.cr_percent + "%";
    for (const [text, cls] of [
      [short(v.outpoint), ""],
      [fmt(v.debt_units), ""],
      [fmt(v.collateral_sats), ""],
      [cr, v.cr_percent != null && v.cr_percent < 130 ? "low" : ""],
      [String(v.last_height), ""],
    ]) {
      const td = document.createElement("td");
      if (cls) td.className = cls;
      td.textContent = text;
      tr.append(td);
    }
    tr.append(document.createElement("td"));
    tbody.append(tr);
  }
  fillVaultSelect("vault-select", st.vaults);
  fillVaultSelect("export-vault", st.vaults);
  $("novaults").classList.toggle("hidden", st.vaults.length > 0);
}

function renderStatus(st) {
  $("height").textContent = st.height;
  $("address").textContent = st.funding_address;
  $("lbtc").textContent = fmt(st.lbtc_sats);
  $("obol").textContent = fmt(st.obol_units);
  $("price").textContent = st.tick ? fmt(st.tick.hi) : "no quorum";
  $("pot").textContent = st.protocol ? fmt(st.protocol.pot_units) : "-";
  $("reserve").textContent = st.protocol ? fmt(st.protocol.reserve_sats) : "-";
  const badge = $("keeper-badge");
  const mode = st.keeper.startsWith("alert") ? "alert" : st.keeper;
  badge.className = "badge " + mode;
  badge.textContent = "keeper " + st.keeper;
  $("keeper-toggle").textContent = mode === "running" ? "stop keeper" : "start keeper";
  alertBanner(mode === "alert" ? st.keeper : null);
  renderVaults(st);
  lastStatus = st;
  settlePendings(st);
  openBounds();
  openEstimate();
  opPanels();
}

async function refresh() {
  try {
    renderStatus(await api("/api/status"));
  } catch (e) {
    journal("op_error", "status: " + e.message);
  }
}

// --- the open card's sliders ----------------------------------------------------------
// The collateral preview mirrors coll_at_cr: debt_cents * cr% * 1e4 / min-quote, floored
// like the covenant's divide. The backend still does the exact sizing; this only lets the
// two sliders read in sats/dollars while they move.
let lastStatus = null;

function usd(cents) {
  return "$" + (cents / 100).toLocaleString("en-US",
    { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

function openEstimate() {
  const principal = Number($("principal").value) || 0;
  const cr = Number($("cr-range").value);
  $("cr-view").textContent = cr + "%";
  $("principal-usd").textContent = principal ? usd(principal) : "";
  const lo = lastStatus && lastStatus.tick ? lastStatus.tick.lo : null;
  $("coll-est").textContent =
    lo && principal ? fmt(Math.floor((principal * cr * 1e4) / lo)) : "-";
}

function openBounds() {
  if (!lastStatus || !lastStatus.tick) return;
  const cr = Number($("cr-range").value);
  // What the balance can fund at this CR: the collateral plus the 0.5% borrow fee, with
  // flat-FEE headroom for the tx fee. Only sizes the slider; the wallet's math decides.
  const spendable = Math.max(0, lastStatus.lbtc_sats - 10000);
  const max = Math.max(1, Math.floor((spendable * lastStatus.tick.lo) / ((cr + 0.5) * 1e4)));
  const range = $("principal-range");
  range.max = max;
  if (Number(range.value) > max) range.value = max;
}

$("principal").addEventListener("input", () => {
  $("principal-range").value = $("principal").value || 1;
  openEstimate();
});
$("principal-range").addEventListener("input", () => {
  $("principal").value = $("principal-range").value;
  openEstimate();
});
$("cr-range").addEventListener("input", () => {
  openBounds();
  openEstimate();
});

$("open-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const f = new FormData(ev.target);
  const body = { principal: Number(f.get("principal")) };
  if (f.get("collateral")) body.collateral = Number(f.get("collateral"));
  else if (f.get("cr_percent")) body.cr_percent = Number(f.get("cr_percent"));
  const goBtn = ev.target.querySelector("button");
  withBusy(goBtn, async () => {
    clearError(goBtn);
    try {
      const r = await api("/api/open", body);
      journal("open", r.txid);
      trackPending("open", r, null);
      await refresh();
    } catch (e) {
      journal("rejected", "open: " + e.message);
      flashError(goBtn, "open refused: " + e.message);
    }
  });
});

// --- the act card: one panel per op, sized to what the vault and the balances allow ----
// Bounds mirror the builders' refusals (repay/redeem: x <= debt and <= OBOL held; draw:
// the 150%-at-min-quote mint gate; close: the full debt in OBOL) so the sliders never
// offer what the chain would bounce. Previews price CR the way the table does: max quote.
const AMOUNT_OPS = ["repay", "draw", "redeem"];

function selectedVault() {
  if (!lastStatus || lastStatus.vaults.length === 0) return null;
  const key = $("vault-select").value;
  return lastStatus.vaults.find((v) => v.outpoint === key) || lastStatus.vaults[0];
}

function crAfter(debtUnits, collSats) {
  if (!lastStatus.tick || debtUnits <= 0) return null;
  return Math.floor((collSats * lastStatus.tick.hi) / (debtUnits * 1e4));
}

function crText(cr) {
  return cr == null ? "-" : cr + "%";
}

// Clamp a number+range pair to [1, max]; max 0 disables the pair (nothing to do).
function setPairMax(op, max) {
  const amount = $(op + "-amount");
  const range = $(op + "-range");
  amount.disabled = range.disabled = max < 1;
  range.max = Math.max(1, max);
  if (Number(range.value) > max) range.value = Math.max(1, max);
  if (Number(amount.value) > max) amount.value = max < 1 ? "" : max;
}

function opPanels() {
  const v = selectedVault();
  $("vault-facts").classList.toggle("hidden", !v);
  // Down without a vault, and down while this vault's last op awaits its block (the
  // ratchet would refuse a same-block follow-up anyway).
  const waiting = v && pendings.some((p) => p.acted === v.outpoint);
  for (const btn of document.querySelectorAll(".op-go")) btn.disabled = !v || waiting;
  if (!v || !lastStatus) return;
  const obol = lastStatus.obol_units;
  $("vf-debt").textContent = fmt(v.debt_units) + " (" + usd(v.debt_units) + ")";
  $("vf-coll").textContent = fmt(v.collateral_sats);
  $("vf-cr").textContent = crText(v.cr_percent);

  setPairMax("repay", Math.min(v.debt_units, obol));
  const repay = Number($("repay-amount").value) || 0;
  $("repay-usd").textContent = repay ? usd(repay) : "";
  $("repay-note").textContent =
    obol < 1
      ? "no OBOL to repay with"
      : repay
        ? "debt after: " + fmt(v.debt_units - repay) + " | CR after: "
          + crText(crAfter(v.debt_units - repay, v.collateral_sats))
        : "reduces the debt; the collateral stays";

  // The mint gate re-judges the POST-fee collateral: draw's tx fee comes out of the
  // vault, so the flat-FEE headroom (10k, the wallet's cap) is off the table first.
  const lo = lastStatus.tick ? lastStatus.tick.lo : null;
  const headroom = lo
    ? Math.floor((Math.max(0, v.collateral_sats - 10000) * lo) / (150 * 1e4)) - v.debt_units
    : 0;
  setPairMax("draw", Math.max(0, headroom));
  const draw = Number($("draw-amount").value) || 0;
  $("draw-usd").textContent = draw ? usd(draw) : "";
  $("draw-note").textContent = !lo
    ? "no tick - draw needs a live quorum"
    : headroom < 1
      ? "no headroom: the vault is at the 150% mint gate already"
      : draw
        ? "debt after: " + fmt(v.debt_units + draw) + " | CR after: "
          + crText(crAfter(v.debt_units + draw, v.collateral_sats))
        : "mints more OBOL against the same collateral (gate: 150% at the min quote)";

  setPairMax("redeem", Math.min(v.debt_units, obol));
  const redeem = Number($("redeem-amount").value) || 0;
  $("redeem-usd").textContent = redeem ? usd(redeem) : "";
  const hi = lastStatus.tick ? lastStatus.tick.hi : null;
  $("redeem-note").textContent =
    obol < 1
      ? "no OBOL to redeem with"
      : redeem && hi
        ? "~" + fmt(Math.floor(((redeem * 1e6) / hi) * 0.995)) + " sats back at the max quote "
          + "(0.5% fee to the reserve) | debt after: " + fmt(v.debt_units - redeem)
        : "swaps OBOL for this vault's collateral at the max quote; no health gate";

  $("close-note").textContent =
    obol >= v.debt_units
      ? "repays the full debt (" + fmt(v.debt_units) + " OBOL) and returns the collateral, ~"
        + fmt(v.collateral_sats) + " sats minus the tx fee"
      : "needs the full debt in OBOL: " + fmt(v.debt_units) + " owed, " + fmt(obol) + " held";
}

for (const tab of $("op-tabs").querySelectorAll("button")) {
  tab.addEventListener("click", () => {
    for (const t of $("op-tabs").querySelectorAll("button")) {
      t.classList.toggle("active", t === tab);
    }
    for (const p of document.querySelectorAll(".op-panel")) {
      p.classList.toggle("hidden", p.id !== "panel-" + tab.dataset.tab);
    }
  });
}

for (const op of AMOUNT_OPS) {
  $(op + "-amount").addEventListener("input", () => {
    $(op + "-range").value = $(op + "-amount").value || 1;
    opPanels();
  });
  $(op + "-range").addEventListener("input", () => {
    $(op + "-amount").value = $(op + "-range").value;
    opPanels();
  });
}
$("vault-select").addEventListener("change", opPanels);

for (const btn of document.querySelectorAll(".op-go")) {
  btn.addEventListener("click", () => {
    const op = btn.dataset.op;
    const v = selectedVault();
    const body = {};
    if ($("vault-select").value) body.vault = $("vault-select").value;
    if (AMOUNT_OPS.includes(op)) {
      const amount = Number($(op + "-amount").value);
      if (!amount) {
        flashError(btn, "amount required");
        return;
      }
      body.amount = amount;
    }
    withBusy(btn, async () => {
      clearError(btn);
      try {
        const r = await api("/api/" + op, body);
        journal(op, r.txid);
        trackPending(op, r, v ? v.outpoint : null);
        await refresh();
      } catch (e) {
        journal("rejected", op + ": " + e.message);
        flashError(btn, op + " refused: " + e.message);
      }
    });
  });
}

$("keeper-toggle").addEventListener("click", () => {
  const path = $("keeper-toggle").textContent.includes("stop")
    ? "/api/keeper/stop"
    : "/api/keeper/start";
  withBusy($("keeper-toggle"), async () => {
    clearError($("keeper-toggle"));
    try {
      await api(path, {});
      await refresh();
    } catch (e) {
      journal("keeper_error", e.message);
      flashError($("keeper-toggle"), "keeper: " + e.message);
    }
  });
});

let exportedTxid = null;
let exportedActed = null;

$("export-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const f = new FormData(ev.target);
  const op = f.get("op");
  const body = { op };
  if (f.get("vault")) body.vault = f.get("vault");
  const exportBtn = ev.target.querySelector("button");
  if (op === "repay" || op === "draw") {
    if (!f.get("amount")) { flashError(exportBtn, "amount required"); return; }
    body.amount = Number(f.get("amount"));
  }
  withBusy(exportBtn, async () => {
    clearError(exportBtn);
    try {
      const r = await api("/api/export", body);
      exportedTxid = r.txid;
      const vs = lastStatus ? lastStatus.vaults : [];
      exportedActed = body.vault || (vs.length ? vs[0].outpoint : null);
      $("export-txid").textContent = r.txid;
      $("export-digest").textContent = r.sighash;
      $("export-out").classList.remove("hidden");
      journal("export", r.txid);
    } catch (e) {
      journal("rejected", "export: " + e.message);
      flashError(exportBtn, "export refused: " + e.message);
    }
  });
});

$("apply-btn").addEventListener("click", () => {
  if (!exportedTxid) return;
  const sig = $("apply-sig").value.trim();
  withBusy($("apply-btn"), async () => {
    clearError($("apply-btn"));
    try {
      const r = await api("/api/apply", { txid: exportedTxid, owner_sig: sig });
      journal("apply", r.txid);
      trackPending("apply", r, exportedActed);
      $("export-out").classList.add("hidden");
      $("apply-sig").value = "";
      exportedTxid = null;
      exportedActed = null;
      await refresh();
    } catch (e) {
      journal("rejected", "apply: " + e.message);
      flashError($("apply-btn"), "apply refused: " + e.message);
    }
  });
});

// The event stream, fetch-streamed (see the header comment). Reconnects on any break.
async function streamEvents() {
  for (;;) {
    try {
      const resp = await fetch("/api/events", { headers: { "x-styx-token": TOKEN } });
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          const data = frame.split("\n").find((l) => l.startsWith("data:"));
          if (!data) continue;
          const ev = JSON.parse(data.slice(5));
          // ticks drive a refresh but are not journal material; the journal is for acts.
          if (ev.kind !== "tick" && ev.kind !== "status") journal(ev.kind, ev.detail);
          if (ev.kind === "performed" || ev.kind === "rejected" || ev.kind === "tick") refresh();
        }
      }
    } catch {
      // fall through to the retry pause
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
}

refresh();
streamEvents();
setInterval(refresh, 5000);
