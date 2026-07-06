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

function renderVaults(st) {
  const tbody = $("vaults").querySelector("tbody");
  tbody.textContent = "";
  const select = $("vault-select");
  const chosen = select.value;
  select.textContent = "";
  select.append(new Option("(only vault)", ""));
  for (const v of st.vaults) {
    const tr = document.createElement("tr");
    const short = v.outpoint.slice(0, 8) + ":" + v.outpoint.split(":")[1];
    const cr = v.cr_percent == null ? "-" : v.cr_percent + "%";
    tr.innerHTML = "";
    for (const [text, cls] of [
      [short, ""],
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
    select.append(new Option(short, v.outpoint));
  }
  if (chosen) select.value = chosen;
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
}

async function refresh() {
  try {
    renderStatus(await api("/api/status"));
  } catch (e) {
    journal("op_error", "status: " + e.message);
  }
}

$("open-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const f = new FormData(ev.target);
  const body = { principal: Number(f.get("principal")) };
  if (f.get("collateral")) body.collateral = Number(f.get("collateral"));
  else if (f.get("cr_percent")) body.cr_percent = Number(f.get("cr_percent"));
  try {
    const r = await api("/api/open", body);
    journal("open", r.txid);
    await refresh();
  } catch (e) {
    journal("rejected", "open: " + e.message);
  }
});

$("op-form").addEventListener("submit", (ev) => ev.preventDefault());
for (const btn of $("op-form").querySelectorAll("button")) {
  btn.addEventListener("click", async (ev) => {
    ev.preventDefault();
    const op = btn.dataset.op;
    const f = new FormData($("op-form"));
    const body = {};
    if (f.get("vault")) body.vault = f.get("vault");
    if (op === "repay" || op === "draw" || op === "redeem") {
      if (!f.get("amount")) {
        journal("rejected", op + ": amount required");
        return;
      }
      body.amount = Number(f.get("amount"));
    }
    try {
      const r = await api("/api/" + op, body);
      journal(op, r.txid);
      await refresh();
    } catch (e) {
      journal("rejected", op + ": " + e.message);
    }
  });
}

$("keeper-toggle").addEventListener("click", async () => {
  const path = $("keeper-toggle").textContent.includes("stop")
    ? "/api/keeper/stop"
    : "/api/keeper/start";
  try {
    await api(path, {});
    await refresh();
  } catch (e) {
    journal("keeper_error", e.message);
  }
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
