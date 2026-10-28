import "maplibre-gl/dist/maplibre-gl.css";
import "./style.css";

import {
  appInfo,
  canFetchOnDemand,
  checkUpdate,
  currentPosition,
  fetchPaperDetail,
  geocode,
  loadDigest,
  loadIndex,
  openExternal,
  openMail,
  downloadDays,
  removeDay,
  savedDays,
  type Place,
  type SavedDay,
} from "./data";
import {
  forgetPlace,
  homeOrigin,
  loadOrigin,
  loadSaved,
  reproject,
  savePlace,
  saveOrigin,
  type Origin,
} from "./origin";
import { PaperMap, escapeHtml, legendHtml } from "./map";
import { Nav, viewFromHash, type Tab, type View } from "./nav";
import {
  allEmails,
  formatKm,
  pinsFor,
  SOURCE_LABEL,
  type Digest,
  type IndexEntry,
  type Issue,
  type Paper,
  type Pin,
} from "./types";

const el = <T extends HTMLElement>(id: string): T => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node as T;
};

let digest: Digest | null = null;
let index: IndexEntry[] = [];
let current: Paper | null = null;
/** null means "wherever this digest was ranked against". */
let origin: Origin | null = loadOrigin();

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
      const offline = offlineDays.has(e.date);
      return `<button class="tl-day${offline ? " tl-saved" : ""}" data-date="${e.date}"
        aria-current="${sel}"
        title="${e.date}: ${e.papers} papers, ${e.located} placed${near}${
          offline ? " · saved offline" : ""
        }">
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

/** The pin responsible for a paper's distance. Mirrors nearest_place() in the core. */
function nearestPin(p: Paper, pins: Pin[]): Pin | undefined {
  if (p.distance_km == null || pins.length === 0) return undefined;
  let best = pins[0];
  for (const pin of pins) {
    if (best && pin.distance_km < best.distance_km) best = pin;
  }
  return best;
}

function renderList(d: Digest): void {
  el("list").innerHTML = d.papers
    .map((p, i) => {
      const pins = pinsFor(p);
      // Name the place that *set* the distance, not whichever author happens
      // to be listed first — otherwise a 0 km paper reads "Chongqing".
      const nearest = nearestPin(p, pins);
      const where = nearest
        ? [nearest.label, nearest.city].filter(Boolean).join(", ")
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

// --- origin -------------------------------------------------------------

function activeOrigin(d: Digest): Origin {
  return origin ?? homeOrigin(d);
}

/** Re-rank the loaded digest and redraw everything that shows a distance. */
function applyOrigin(d: Digest): void {
  const o = activeOrigin(d);
  reproject(d, o);
  // The map's home marker is the origin in use, not the build location.
  d.home = { lat: o.lat, lon: o.lon, label: o.label };
  el("origin-label").textContent = o.label;
  el("origin-btn").setAttribute(
    "title",
    `Measuring from ${o.label} (${o.lat.toFixed(3)}, ${o.lon.toFixed(3)})`,
  );
  renderStats(d);
  renderList(d);
  paperMap.refit();
  if (!el("pane-map").hidden) paperMap.render(d);
}

function setOrigin(o: Origin | null): void {
  origin = o;
  saveOrigin(o);
  if (digest) applyOrigin(digest);
}

/** Nominatim display names are long; keep the place and the country. */
function shortPlace(label: string): string {
  const parts = label.split(",").map((s) => s.trim()).filter(Boolean);
  const first = parts[0];
  const last = parts[parts.length - 1];
  if (parts.length > 1 && first && last && first !== last) {
    return `${first}, ${last}`;
  }
  return first ?? label;
}

function originFromPlace(p: Place, kind: Origin["kind"]): Origin {
  const label =
    p.kind === "coordinates"
      ? `${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}`
      : shortPlace(p.label);
  return { lat: p.lat, lon: p.lon, label, kind };
}

function renderSavedPlaces(): void {
  const rows: string[] = [];

  if (digest) {
    const h = homeOrigin(digest);
    const isHome = origin === null;
    rows.push(
      `<button class="place-row${isHome ? " place-current" : ""}" data-home="1">` +
        `<span class="place-name">${escapeHtml(h.label)}</span>` +
        `<span class="place-sub">where this digest was ranked${isHome ? " · in use" : ""}</span>` +
        `</button>`,
    );
  }

  const saved = loadSaved();
  for (const [i, p] of saved.entries()) {
    const inUse =
      origin != null &&
      Math.abs(origin.lat - p.lat) < 1e-6 &&
      Math.abs(origin.lon - p.lon) < 1e-6;
    rows.push(
      `<button class="place-row${inUse ? " place-current" : ""}" data-saved="${i}">` +
        `<span class="place-name">${escapeHtml(p.label)}</span>` +
        `<span class="place-sub">${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}${inUse ? " · in use" : ""}</span>` +
        `<span class="place-forget" data-forget="${i}" role="button" aria-label="Forget this place">×</span>` +
        `</button>`,
    );
  }

  el("saved-places").innerHTML = rows.join("");

  el("saved-places")
    .querySelector<HTMLElement>("[data-home]")
    ?.addEventListener("click", () => {
      setOrigin(null);
      renderSavedPlaces();
      closeOriginSheet();
    });

  for (const b of el("saved-places").querySelectorAll<HTMLElement>("[data-saved]")) {
    b.addEventListener("click", (ev) => {
      const place = loadSaved()[Number(b.dataset.saved)];
      if (!place) return;
      if ((ev.target as HTMLElement).dataset.forget != null) {
        ev.stopPropagation();
        forgetPlace(place);
        renderSavedPlaces();
        return;
      }
      setOrigin({ lat: place.lat, lon: place.lon, label: place.label, kind: "custom" });
      renderSavedPlaces();
      closeOriginSheet();
    });
  }
}

function renderPlaceResults(places: Place[]): void {
  if (places.length === 0) {
    el("place-results").innerHTML = `<p class="muted">Nothing matched.</p>`;
    return;
  }
  el("place-results").innerHTML = places
    .map(
      (p, i) =>
        `<button class="place-row" data-hit="${i}">` +
        `<span class="place-name">${escapeHtml(shortPlace(p.label))}</span>` +
        `<span class="place-sub">${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}${
          p.kind ? ` · ${escapeHtml(p.kind)}` : ""
        }</span></button>`,
    )
    .join("");

  for (const b of el("place-results").querySelectorAll<HTMLButtonElement>("[data-hit]")) {
    b.addEventListener("click", () => {
      const p = places[Number(b.dataset.hit)];
      if (!p) return;
      const o = originFromPlace(p, "custom");
      // Looked-up places are kept, so preparing for a trip is a one-off.
      savePlace({ lat: o.lat, lon: o.lon, label: o.label });
      setOrigin(o);
      renderSavedPlaces();
      closeOriginSheet();
    });
  }
}

function openOriginSheet(): void {
  renderSavedPlaces();
  el("place-results").innerHTML = "";
  el("gps-status").textContent = "";
  nav.go({ view: "origin", tab: baseTab() });
}

function closeOriginSheet(): void {
  nav.back();
}

async function useCurrentLocation(): Promise<void> {
  const status = el("gps-status");
  status.className = "muted";
  status.textContent = "Finding you…";
  try {
    const pos = await currentPosition();
    setOrigin({ lat: pos.lat, lon: pos.lon, label: "My location", kind: "current" });
    renderSavedPlaces();
    closeOriginSheet();
  } catch (e) {
    status.className = "err";
    status.textContent = e instanceof Error ? e.message : String(e);
  }
}

async function runPlaceSearch(): Promise<void> {
  const q = el<HTMLInputElement>("place-query").value;
  if (!q.trim()) return;
  el("place-results").innerHTML = `<p class="muted">Searching…</p>`;
  try {
    renderPlaceResults(await geocode(q));
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    el("place-results").innerHTML = `<p class="err">${escapeHtml(msg)}</p>`;
  }
}

// --- parsing report -----------------------------------------------------

/** Mirrors Diagnostics::issues() in the Rust core. */
const ISSUE_SPECS: { key: string; what: string; fix: string; actionable: boolean }[] = [
  {
    key: "affiliations_unresolved",
    what: "Affiliation read off the paper matched no institution",
    fix: "These papers fall back to the author prior instead of their printed address.",
    actionable: true,
  },
  {
    key: "intro_not_found",
    what: "Paper had HTML but no section was recognised as the introduction",
    fix: "The heading form may be unhandled — 'Background', 'Motivation', unnumbered.",
    actionable: true,
  },
  {
    key: "emails_not_found",
    what: "Paper had HTML but no contact address was found",
    fix: "Some papers print none; if arXiv shows one, the front-matter cut is wrong.",
    actionable: true,
  },
  {
    key: "papers_without_authors",
    what: "Feed entry carried no author names",
    fix: "These cannot be placed at all.",
    actionable: true,
  },
  {
    key: "items_unidentified",
    what: "Feed item had no readable arXiv id and was dropped",
    fix: "This silently loses papers — worth checking the feed format.",
    actionable: true,
  },
  {
    key: "feeds_failed",
    what: "A category feed could not be fetched",
    fix: "Usually transient; a nightly repeat means the feed name changed.",
    actionable: true,
  },
  {
    key: "institutions_without_geo",
    what: "Institution resolved but has no coordinates upstream",
    fix: "An OpenAlex data gap; ROR sometimes has coordinates where it does not.",
    actionable: true,
  },
  { key: "html_errors", what: "Fetching the paper HTML failed", fix: "Usually rate limiting.", actionable: true },
  { key: "authors_unknown", what: "OpenAlex had no record of this author", fix: "", actionable: false },
  { key: "html_absent", what: "arXiv has no HTML rendering of the paper", fix: "", actionable: false },
];

function issuesFrom(d: Digest): Issue[] {
  const diag = d.diagnostics;
  if (!diag) return [];
  const out: Issue[] = [];
  for (const spec of ISSUE_SPECS) {
    const t = diag[spec.key] as { count?: number; samples?: string[] } | undefined;
    if (!t?.count) continue;
    out.push({
      kind: spec.key,
      what: spec.what,
      fix: spec.fix,
      count: t.count,
      samples: t.samples ?? [],
      actionable: spec.actionable,
    });
  }
  out.sort((a, b) =>
    a.actionable === b.actionable ? b.count - a.count : Number(b.actionable) - Number(a.actionable),
  );
  return out;
}

function renderDiagnostics(d: Digest): void {
  const diag = d.diagnostics;
  if (!diag) {
    el("diag").innerHTML = `<p class="muted" style="padding:14px">
      This digest predates parsing reports.</p>`;
    return;
  }
  const issues = issuesFrom(d);
  const notes: string[] = [];
  if (diag.lookup_budget_hit) {
    notes.push("The lookup budget was reached — later runs will place the rest.");
  }
  if (diag.openalex_degraded) {
    notes.push("OpenAlex was throttled, so lookups stopped early.");
  }

  const header = `<div class="diag-head">
    <div>${d.stats.harvested} papers from ${diag.feeds_attempted} feeds · ${diag.enrich_attempted} read in full</div>
    ${notes.map((n) => `<div class="diag-note">${escapeHtml(n)}</div>`).join("")}
  </div>`;

  if (issues.length === 0) {
    el("diag").innerHTML =
      header + `<p class="muted" style="padding:14px">Nothing failed to parse.</p>`;
    return;
  }

  el("diag").innerHTML =
    header +
    issues
      .map((i) => {
        const samples = (i.samples ?? []).slice(0, 12);
        return `<div class="issue">
        <div class="issue-top">
          <span class="badge${i.actionable ? " badge-fix" : ""}">${i.actionable ? "fix" : "expected"}</span>
          <span class="issue-count">${i.count}</span>
          <span class="issue-what">${escapeHtml(i.what)}</span>
        </div>
        ${i.fix ? `<div class="issue-fix">${escapeHtml(i.fix)}</div>` : ""}
        ${
          samples.length
            ? `<details class="issue-samples">
                 <summary>${samples.length} example${samples.length === 1 ? "" : "s"}</summary>
                 <ul>${samples.map((s) => `<li>${escapeHtml(s)}</li>`).join("")}</ul>
               </details>`
            : ""
        }
      </div>`;
      })
      .join("");
}

// --- detail sheet -------------------------------------------------------

function renderSheet(p: Paper): void {
  const emails = allEmails(p);
  const pins = pinsFor(p);

  const authors = p.authors
    .map((a) => {
      const bits: string[] = [];
      if (a.institution) {
        bits.push(
          `${escapeHtml(a.institution)}${a.city ? `, ${escapeHtml(a.city)}` : ""} · ${formatKm(a.distance_km)}`,
        );
      }
      const mail = a.email
        ? `<a class="mail-link" href="#" data-mail="${escapeHtml(a.email)}">${escapeHtml(a.email)}</a>`
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
            `<a class="mail-link" href="#" data-mail="${escapeHtml(e)}">${escapeHtml(e)}</a>`,
        )
        .join("<br>")}</p>`
    : "";

  const mailAll = emails.length
    ? `<p><button class="mailall" data-mail-all="1">Email all ${emails.length} authors</button></p>`
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

  const mailSubject = `Re: ${p.title}`;
  for (const a of el("sheet-body").querySelectorAll<HTMLElement>("[data-mail]")) {
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      void openMail([a.dataset.mail ?? ""], mailSubject);
    });
  }
  el("sheet-body")
    .querySelector<HTMLElement>("[data-mail-all]")
    ?.addEventListener("click", (ev) => {
      ev.preventDefault();
      void openMail(emails, mailSubject);
    });

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
  nav.go({ view: "paper", tab: baseTab(), id: p.id });
}

function closeSheet(): void {
  nav.back();
}

// --- day loading --------------------------------------------------------

async function show(date: string): Promise<void> {
  el("list").innerHTML = `<p class="spinner" style="padding:14px">Loading ${date}…</p>`;
  try {
    digest = await loadDigest(date);
    paperMap.refit();
    // Ranks against the chosen origin, which may not be the one the pipeline
    // built with. Also renders the stats, list and map.
    applyOrigin(digest);
    renderTimeline();
    if (!el("pane-diag").hidden) renderDiagnostics(digest);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    el("list").innerHTML = `<p class="err" style="padding:14px">Could not load ${date}: ${escapeHtml(msg)}</p>`;
  }
}

/** Show a tab. Pure rendering: navigation is the Nav's job. */
function showTab(which: Tab): void {
  for (const t of ["list", "map", "diag"] as Tab[]) {
    el(`pane-${t}`).hidden = t !== which;
    el(`tab-${t}`).setAttribute("aria-selected", String(t === which));
  }
  if (which === "map" && digest) {
    paperMap.render(digest);
    paperMap.resize();
  }
  if (which === "diag" && digest) {
    renderDiagnostics(digest);
  }
}

/**
 * Draw a whole view. Called by the navigator, including on back, so every
 * overlay is closed unless the state says it is open.
 */
function renderView(s: View): void {
  showTab(s.tab);

  const paper =
    s.view === "paper" ? (digest?.papers.find((p) => p.id === s.id) ?? null) : null;
  if (paper) {
    current = paper;
    renderSheet(paper);
    el("sheet-body").scrollTop = 0;
  } else if (s.view !== "paper") {
    current = null;
  }

  el("sheet").hidden = paper === null;
  el("origin-sheet").hidden = s.view !== "origin";
  el("settings-sheet").hidden = s.view !== "settings";
}

let nav: Nav;

/** The tab an overlay sits on top of, so back returns to it. */
function baseTab(): Tab {
  return nav.current.tab;
}

// --- offline ------------------------------------------------------------

/** Dates held on the phone, refreshed whenever storage changes. */
let offlineDays = new Set<string>();

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} kB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

async function refreshSaved(): Promise<void> {
  const days = await savedDays();
  offlineDays = new Set(days.map((d) => d.date));
  renderTimeline();
  renderOffline(days);
}

function renderOffline(days: SavedDay[]): void {
  const total = days.reduce((n, d) => n + d.bytes, 0);
  const missing = index.filter((e) => !offlineDays.has(e.date)).length;

  if (!canFetchOnDemand()) {
    el("offline-summary").textContent =
      "Saving days for offline only applies to the app.";
    el("offline-days").innerHTML = "";
    el<HTMLButtonElement>("save-all").disabled = true;
    return;
  }

  el("offline-summary").textContent =
    `${days.length} of ${index.length} day${index.length === 1 ? "" : "s"} saved` +
    ` · ${formatBytes(total)}` +
    (missing ? ` · ${missing} not saved yet` : " · nothing left to fetch");
  el<HTMLButtonElement>("save-all").disabled = missing === 0;

  el("offline-days").innerHTML = index
    .map((e) => {
      const day = days.find((d) => d.date === e.date);
      const sub = day
        ? `saved · ${formatBytes(day.bytes)}`
        : `${e.papers} papers · not saved`;
      return (
        `<button class="place-row${day ? " place-current" : ""}" data-day="${e.date}">` +
        `<span class="place-name">${e.date}</span>` +
        `<span class="place-sub">${sub}</span>` +
        (day
          ? `<span class="place-forget" data-drop="${e.date}" role="button" aria-label="Remove">×</span>`
          : "") +
        `</button>`
      );
    })
    .join("");

  for (const b of el("offline-days").querySelectorAll<HTMLElement>("[data-day]")) {
    b.addEventListener("click", (ev) => {
      const date = b.dataset.day;
      if (!date) return;
      const drop = (ev.target as HTMLElement).dataset.drop;
      if (drop) {
        ev.stopPropagation();
        void removeDay(drop).then(refreshSaved);
        return;
      }
      void saveDays([date]);
    });
  }
}

async function saveDays(dates: string[]): Promise<void> {
  const status = el("offline-summary");
  status.className = "muted";
  status.textContent = `Saving ${dates.length} day${dates.length === 1 ? "" : "s"}…`;
  try {
    const report = await downloadDays(dates);
    await refreshSaved();
    if (report.failed.length) {
      status.className = "err";
      status.textContent = `Could not save: ${report.failed.join(", ")}`;
    }
  } catch (e) {
    status.className = "err";
    status.textContent = e instanceof Error ? e.message : String(e);
  }
}

// --- settings -----------------------------------------------------------

const CHANNEL_KEY = "nearfield-channel";
type Channel = "stable" | "nightly";

/** The channel to follow: the user's choice, else the one this build came from. */
function activeChannel(): Channel | undefined {
  const stored = localStorage.getItem(CHANNEL_KEY);
  return stored === "stable" || stored === "nightly" ? stored : undefined;
}

function renderChannelChoice(buildChannel: string | undefined): void {
  const chosen = activeChannel() ?? (buildChannel as Channel | undefined);
  for (const c of ["stable", "nightly"] as Channel[]) {
    el(`ch-${c}`).setAttribute("aria-checked", String(chosen === c));
  }
}

async function openSettings(): Promise<void> {
  nav.go({ view: "settings", tab: baseTab() });
  el("update-status").textContent = "";
  el("update-status").className = "muted";

  void refreshSaved();
  const info = await appInfo();
  el("build-info").textContent = info
    ? `Nearfield ${info.version} · built on the ${info.channel} channel`
    : "Running in a browser — updates and channels only apply to the app.";
  renderChannelChoice(info?.channel);
}

function closeSettings(): void {
  nav.back();
}

function setChannel(c: Channel, buildChannel: string | undefined): void {
  localStorage.setItem(CHANNEL_KEY, c);
  renderChannelChoice(buildChannel);
  void runUpdateCheck();
}

/** The manual "Check for updates" button. */
async function runUpdateCheck(): Promise<void> {
  const status = el("update-status");
  status.className = "muted";

  if (!canFetchOnDemand()) {
    status.className = "err";
    status.textContent = "Updates only work in the app.";
    return;
  }
  status.textContent = "Checking…";
  try {
    const update = await checkUpdate(activeChannel());
    if (!update) {
      status.className = "muted";
      status.textContent = "You are on the latest build for this channel.";
      return;
    }
    status.className = "muted";
    status.innerHTML =
      `${escapeHtml(update.version)} is available. ` +
      `<button id="install-now" class="inline-btn">Install</button>`;
    document.getElementById("install-now")?.addEventListener("click", () => {
      void openExternal(update.url);
    });
  } catch (e) {
    status.className = "err";
    status.textContent = e instanceof Error ? e.message : String(e);
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
    update = await checkUpdate(activeChannel());
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

  // The deep-linked view is the first history entry, so back from it exits
  // rather than stepping through states the user never visited.
  const initial = viewFromHash(location.hash);
  nav = new Nav(renderView, initial);

  for (const t of ["list", "map", "diag"] as Tab[]) {
    el(`tab-${t}`).addEventListener("click", () => nav.go({ view: "tab", tab: t }));
  }
  el("sheet-close").addEventListener("click", closeSheet);
  el("origin-btn").addEventListener("click", openOriginSheet);
  el("settings-btn").addEventListener("click", () => void openSettings());
  el("settings-close").addEventListener("click", closeSettings);
  el("check-updates").addEventListener("click", () => void runUpdateCheck());
  el("save-all").addEventListener("click", () => {
    void saveDays(index.filter((e) => !offlineDays.has(e.date)).map((e) => e.date));
  });
  for (const c of ["stable", "nightly"] as Channel[]) {
    el(`ch-${c}`).addEventListener("click", () => {
      void appInfo().then((i) => setChannel(c, i?.channel));
    });
  }
  el("origin-close").addEventListener("click", closeOriginSheet);
  el("use-gps").addEventListener("click", () => void useCurrentLocation());
  el<HTMLFormElement>("place-form").addEventListener("submit", (ev) => {
    ev.preventDefault();
    void runPlaceSearch();
  });

  el("sheet-pdf").addEventListener("click", () => {
    if (current) void openExternal(`https://arxiv.org/pdf/${current.id}`);
  });
  el("sheet-abs").addEventListener("click", () => {
    if (current) void openExternal(`https://arxiv.org/abs/${current.id}`);
  });
  el("sheet-locate").addEventListener("click", () => {
    if (!current) return;
    const pin = nearestPin(current, pinsFor(current)) ?? pinsFor(current)[0];
    if (!pin) return;
    // Replace rather than push: the paper is where we came from, so one back
    // should leave the map, not drop straight back into the paper.
    nav.replace({ view: "tab", tab: "map" });
    paperMap.flyTo(pin.lat, pin.lon);
  });

  // Escape behaves like the Android back gesture.
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && nav.current.view !== "tab") nav.back();
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
  await refreshSaved();
  await show(latest.date);
  // The digest is only now available, so a #p/<id> deep link can finally be
  // drawn; re-render whatever state we are in.
  nav.refresh();
  void offerUpdate();
}

void boot();
