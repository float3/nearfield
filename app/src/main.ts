import "maplibre-gl/dist/maplibre-gl.css";
import "./style.css";

import {
  appInfo,
  canFetchOnDemand,
  checkUpdate,
  fetchPaperDetail,
  loadDigest,
  loadIndex,
  openExternal,
} from "./data";
import { PaperMap, escapeHtml, legendHtml } from "./map";
import {
  allEmails,
  formatKm,
  pinsFor,
  SOURCE_LABEL,
  type Digest,
  type IndexEntry,
  type Paper,
} from "./types";

const el = <T extends HTMLElement>(id: string): T => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node as T;
};

let digest: Digest | null = null;
let index: IndexEntry[] = [];
let current: Paper | null = null;

const paperMap = new PaperMap(el("map"), (p) => openSheet(p));

// --- rendering ----------------------------------------------------------

function renderStats(d: Digest): void {
  el("hdr-date").textContent = d.date;
  el("stats").innerHTML = [
    stat(d.stats.harvested, "papers"),
    stat(d.stats.located, "placed"),
    stat(d.stats.enriched, "read"),
    stat(d.stats.with_email, "emails"),
  ].join("");
}

function stat(n: number, label: string): string {
  return `<div class="stat"><b>${n}</b><span>${label}</span></div>`;
}

function renderTimeline(): void {
  const max = Math.max(1, ...index.map((e) => e.papers));
  // Oldest on the left, so time runs the usual direction.
  const days = [...index].reverse();
  el("timeline").innerHTML = days
    .map((e) => {
      const h = Math.max(3, Math.round((e.papers / max) * 30));
      const sel = digest?.date === e.date;
      const near = e.nearest_km != null ? `, nearest ${formatKm(e.nearest_km)}` : "";
      return `<button class="tl-day" data-date="${e.date}" aria-current="${sel}"
        title="${e.date}: ${e.papers} papers, ${e.located} placed${near}">
        <span class="tl-bar" style="height:${h}px"></span>
        <span class="tl-label">${e.date.slice(8)}</span>
      </button>`;
    })
    .join("");

  for (const b of el("timeline").querySelectorAll<HTMLButtonElement>(".tl-day")) {
    b.addEventListener("click", () => {
      const date = b.dataset.date;
      if (date && date !== digest?.date) void show(date);
    });
  }
  const sel = el("timeline").querySelector('[aria-current="true"]');
  sel?.scrollIntoView({ inline: "center", block: "nearest" });
}

function renderList(d: Digest): void {
  el("list").innerHTML = d.papers
    .map((p, i) => {
      const pins = pinsFor(p);
      const first = pins[0];
      const where = first
        ? [first.label, first.city].filter(Boolean).join(", ")
        : "not located";
      const extra = pins.length > 1 ? ` +${pins.length - 1} more` : "";
      return `<button class="paper" data-i="${i}">
        <div class="paper-top">
          <span class="km${p.distance_km == null ? " unlocated" : ""}">${formatKm(p.distance_km)}</span>
          <span class="paper-title">${escapeHtml(p.title)}</span>
        </div>
        <div class="paper-meta">
          <span class="dot dot-${p.geo_source}"></span>
          <span class="where">${escapeHtml(where)}</span>${extra}
          · ${p.authors.length} author${p.authors.length === 1 ? "" : "s"}
        </div>
        <div class="cats">${escapeHtml(p.categories.join(" · "))}</div>
      </button>`;
    })
    .join("");

  for (const b of el("list").querySelectorAll<HTMLButtonElement>(".paper")) {
    b.addEventListener("click", () => {
      const i = Number(b.dataset.i);
      const p = d.papers[i];
      if (p) openSheet(p);
    });
  }
}

// --- detail sheet -------------------------------------------------------

function renderSheet(p: Paper): void {
  const emails = allEmails(p);
  const pins = pinsFor(p);
  const subject = encodeURIComponent(`Re: ${p.title}`);

  const authors = p.authors
    .map((a) => {
      const bits: string[] = [];
      if (a.institution) {
        bits.push(
          `${escapeHtml(a.institution)}${a.city ? `, ${escapeHtml(a.city)}` : ""} · ${formatKm(a.distance_km)}`,
        );
      }
      const mail = a.email
        ? `<a href="mailto:${encodeURIComponent(a.email)}?subject=${subject}">${escapeHtml(a.email)}</a>`
        : "";
      return `<li>
        <div><span class="dot dot-${a.geo_source}"></span>${escapeHtml(a.name)}</div>
        ${bits.length ? `<div class="aff">${bits.join("")}</div>` : ""}
        ${mail ? `<div class="aff">${mail}</div>` : ""}
      </li>`;
    })
    .join("");

  const unattributed = (p.contact_emails ?? []).length
    ? `<h3>Other addresses on the paper</h3><p class="aff">${(p.contact_emails ?? [])
        .map(
          (e) =>
            `<a href="mailto:${encodeURIComponent(e)}?subject=${subject}">${escapeHtml(e)}</a>`,
        )
        .join("<br>")}</p>`
    : "";

  const mailAll = emails.length
    ? `<p><a class="mailall" href="mailto:${emails.map(encodeURIComponent).join(",")}?subject=${subject}">Email all ${emails.length} authors</a></p>`
    : `<p class="muted">No addresses found on this paper.</p>`;

  const intro = p.intro
    ? `<h3>Introduction</h3><div class="intro">${escapeHtml(p.intro)}</div>`
    : p.enriched
      ? `<h3>Introduction</h3><p class="muted">This paper has no HTML rendering on arXiv, so its introduction can't be extracted. The PDF button above opens it in your reader.</p>`
      : `<h3>Introduction</h3><p><button id="fetch-intro">Fetch introduction &amp; emails</button></p>
         <p class="muted">Outside tonight's prefetch window — this pulls it from arXiv now.</p>`;

  el("sheet-body").innerHTML = `
    <h2>${escapeHtml(p.title)}</h2>
    <p class="muted">
      ${formatKm(p.distance_km)} away · ${SOURCE_LABEL[p.geo_source]}
      · ${escapeHtml(p.categories.join(" · "))} · arXiv:${escapeHtml(p.id)}
    </p>
    ${pins.length ? `<p class="muted">${pins.map((n) => escapeHtml([n.label, n.city, n.country].filter(Boolean).join(", "))).join(" · ")}</p>` : ""}
    <h3>Abstract</h3>
    <p class="abstract">${escapeHtml(p.abstract)}</p>
    ${intro}
    <h3>Authors</h3>
    <ul class="authors">${authors}</ul>
    ${unattributed}
    ${mailAll}
  `;

  const fetchBtn = document.getElementById("fetch-intro");
  fetchBtn?.addEventListener("click", () => void onFetchIntro(p, fetchBtn as HTMLButtonElement));

  const locate = el<HTMLButtonElement>("sheet-locate");
  locate.disabled = pins.length === 0;
}

async function onFetchIntro(p: Paper, button: HTMLButtonElement): Promise<void> {
  if (!canFetchOnDemand()) {
    button.outerHTML = `<span class="err">Fetching on demand needs the app — a browser cannot read arxiv.org cross-origin.</span>`;
    return;
  }
  button.disabled = true;
  button.textContent = "Fetching…";
  try {
    const detail = await fetchPaperDetail(p.id);
    // Keep the harvested metadata; take only what the paper's HTML adds.
    p.intro = detail.intro;
    p.enriched = true;
    p.contact_emails = detail.contact_emails;
    p.affiliations = detail.affiliations;
    for (const a of p.authors) {
      const match = detail.authors.find((d) => d.name === a.name);
      if (match?.email) a.email = match.email;
    }
    renderSheet(p);
  } catch (e) {
    button.disabled = false;
    button.textContent = "Fetch introduction & emails";
    const msg = e instanceof Error ? e.message : String(e);
    button.insertAdjacentHTML("afterend", `<div class="err">${escapeHtml(msg)}</div>`);
  }
}

function openSheet(p: Paper): void {
  current = p;
  renderSheet(p);
  el("sheet").hidden = false;
  el("sheet-body").scrollTop = 0;
}

function closeSheet(): void {
  el("sheet").hidden = true;
  current = null;
}

// --- day loading --------------------------------------------------------

async function show(date: string): Promise<void> {
  el("list").innerHTML = `<p class="spinner" style="padding:14px">Loading ${date}…</p>`;
  try {
    digest = await loadDigest(date);
    paperMap.refit();
    renderStats(digest);
    renderList(digest);
    renderTimeline();
    if (!el("pane-map").hidden) paperMap.render(digest);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    el("list").innerHTML = `<p class="err" style="padding:14px">Could not load ${date}: ${escapeHtml(msg)}</p>`;
  }
}

function selectTab(which: "list" | "map", pushHash = true): void {
  if (pushHash && location.hash.slice(1) !== which) {
    history.replaceState(null, "", `#${which}`);
  }
  const isMap = which === "map";
  el("pane-list").hidden = isMap;
  el("pane-map").hidden = !isMap;
  el("tab-list").setAttribute("aria-selected", String(!isMap));
  el("tab-map").setAttribute("aria-selected", String(isMap));
  if (isMap && digest) {
    paperMap.render(digest);
    paperMap.resize();
  }
}

/**
 * Offer an update if this build's channel has a newer one.
 *
 * Android will not let a sideloaded app install silently, so the most this can
 * do is hand the APK to the system installer — which always asks. A dismissal
 * is remembered per version so the banner does not nag.
 */
async function offerUpdate(): Promise<void> {
  const info = await appInfo();
  if (info) {
    el("version").textContent = `Nearfield ${info.version} · ${info.channel} channel`;
  }
  let update;
  try {
    update = await checkUpdate();
  } catch {
    return; // Offline or the channel is unreachable; not worth a message.
  }
  if (!update) return;
  if (localStorage.getItem("dismissed-update") === update.version) return;

  const bar = el("updatebar");
  el("update-text").textContent = `Version ${update.version} is available${
    update.notes ? ` — ${update.notes}` : ""
  }`;
  bar.hidden = false;
  el("update-get").addEventListener("click", () => void openExternal(update.url));
  el("update-dismiss").addEventListener("click", () => {
    localStorage.setItem("dismissed-update", update.version);
    bar.hidden = true;
  });
}

async function boot(): Promise<void> {
  el("legend").innerHTML = legendHtml();
  el("tab-list").addEventListener("click", () => selectTab("list"));
  el("tab-map").addEventListener("click", () => selectTab("map"));
  // Deep link: #map opens straight onto the map.
  if (location.hash === "#map") selectTab("map", false);
  el("sheet-close").addEventListener("click", closeSheet);

  el("sheet-pdf").addEventListener("click", () => {
    if (current) void openExternal(`https://arxiv.org/pdf/${current.id}`);
  });
  el("sheet-abs").addEventListener("click", () => {
    if (current) void openExternal(`https://arxiv.org/abs/${current.id}`);
  });
  el("sheet-locate").addEventListener("click", () => {
    if (!current) return;
    const pin = pinsFor(current)[0];
    if (!pin) return;
    closeSheet();
    selectTab("map");
    paperMap.flyTo(pin.lat, pin.lon);
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !el("sheet").hidden) closeSheet();
  });

  try {
    index = await loadIndex();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    el("list").innerHTML = `<p class="err" style="padding:14px">Could not load the index: ${escapeHtml(msg)}</p>`;
    return;
  }
  const latest = index[0];
  if (!latest) {
    el("list").innerHTML = `<p class="muted" style="padding:14px">No digests published yet.</p>`;
    return;
  }
  await show(latest.date);
  if (location.hash === "#map") selectTab("map", false);
  void offerUpdate();
}

void boot();
