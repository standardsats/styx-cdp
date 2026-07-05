/* STYX spec site - shared navigation. No build step: each page includes
   an empty <aside class="side" id="side"> and <nav class="pager" id="pager">;
   this script fills both from the single PAGES list below. */

const PAGES = [
  { file: "index.html",        gn: "Αʹ",       title: "Overview" },
  { file: "architecture.html", gn: "Βʹ",       title: "Architecture" },
  { file: "covenants.html",    gn: "Γʹ",       title: "The covenants" },
  { file: "lifecycle.html",    gn: "Δʹ",       title: "Transaction lifecycle" },
  { file: "oracle.html",       gn: "Εʹ",       title: "The oracle" },
  { file: "reserve.html",      gn: "ΣΤʹ", title: "Reserve and bad debt" },
  { file: "economics.html",    gn: "Ζʹ",       title: "Economic model" },
  { file: "security.html",     gn: "Ηʹ",       title: "Security model" },
  { file: "deployment.html",   gn: "Θʹ",       title: "Deployment and freeze" },
  { file: "building.html",     gn: "Ιʹ",       title: "Building on STYX" },
  { file: "glossary.html",     gn: "ΙΑʹ", title: "Glossary and appendix" },
  { file: "source.html",       gn: "ΙΒʹ", title: "Covenant source" },
];

(function () {
  const here = (location.pathname.split("/").pop() || "index.html");
  const idx = PAGES.findIndex(p => p.file === here);

  const side = document.getElementById("side");
  if (side) {
    const links = PAGES.map(p =>
      `<a href="${p.file}"${p.file === here ? ' class="here"' : ""}>` +
      `<span class="num">${p.gn}</span><span>${p.title}</span></a>`
    ).join("");
    side.innerHTML =
      `<a class="brand" href="index.html">` +
      `<span class="name">ΣΤΥ<span class="xi">Ξ</span></span>` +
      `<div class="tag">covenant CDP · protocol spec v1</div></a>` +
      `<div class="meander"></div>` +
      `<nav>${links}</nav>` +
      `<div class="foot">frozen covenant · 2026-07-03<br>` +
      `<a href="diagram.html">interactive diagram</a><br>` +
      `normative for v1 · CMR-pinned source</div>`;
  }

  const pager = document.getElementById("pager");
  if (pager && idx >= 0) {
    const prev = PAGES[idx - 1], next = PAGES[idx + 1];
    pager.innerHTML =
      (prev ? `<a class="prev" href="${prev.file}"><span class="dir">&lsaquo; previous</span>` +
              `<span class="ttl">${prev.gn} ${prev.title}</span></a>` : "<span></span>") +
      (next ? `<a class="next" href="${next.file}"><span class="dir">next &rsaquo;</span>` +
              `<span class="ttl">${next.gn} ${next.title}</span></a>` : "");
  }
})();
