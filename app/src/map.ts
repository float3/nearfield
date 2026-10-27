// The map pane. Markers carry *identity* (where the location came from), not
// magnitude — the map's geometry already encodes distance from Munich, so
// colouring by distance would say the same thing twice. Each source also has a
// distinct shape, so the encoding survives colour-vision deficiency and the
// aqua slot's low contrast on the light surface.

// maplibre-gl v6 dropped its default export, so everything is named.
import {
  LngLatBounds,
  Map as MlMap,
  Marker,
  NavigationControl,
  Popup,
  type LngLatLike,
  type StyleSpecification,
} from "maplibre-gl";
import type { Digest, GeoSource, Paper } from "./types";
import { pinsFor, formatKm, SOURCE_LABEL } from "./types";

const OSM_STYLE: StyleSpecification = {
  version: 8,
  sources: {
    osm: {
      type: "raster",
      tiles: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png"],
      tileSize: 256,
      maxzoom: 19,
      attribution: "© OpenStreetMap contributors",
    },
  },
  layers: [{ id: "osm", type: "raster", source: "osm" }],
};

const SOURCE_ORDER: GeoSource[] = ["paper_html", "author_prior", "openalex_work"];

export class PaperMap {
  private map: MlMap | null = null;
  private markers: Marker[] = [];
  /** Only auto-fit once, so a view you panned is not yanked back. */
  private fitted = false;

  constructor(
    private container: HTMLElement,
    private onPick: (paper: Paper) => void,
  ) {}

  /** MapLibre must size itself against a visible container, so creation is
   *  deferred until the pane is first shown. */
  ensure(home: { lat: number; lon: number }): MlMap {
    if (this.map) {
      this.map.resize();
      return this.map;
    }
    const map = new MlMap({
      container: this.container,
      style: OSM_STYLE,
      center: [home.lon, home.lat] as LngLatLike,
      zoom: 3,
      attributionControl: { compact: true },
    });
    map.addControl(new NavigationControl({ showCompass: false }), "top-right");
    this.map = map;
    return map;
  }

  private marker(color: string, shape: GeoSource): HTMLElement {
    const el = document.createElement("div");
    el.style.width = "13px";
    el.style.height = "13px";
    el.style.cursor = "pointer";
    // 2px surface ring keeps overlapping marks separable.
    el.style.boxShadow = "0 0 0 2px var(--surface-1)";
    switch (shape) {
      case "author_prior":
        el.style.border = `3px solid ${color}`;
        el.style.borderRadius = "50%";
        el.style.background = "transparent";
        break;
      case "openalex_work":
        el.style.background = color;
        break;
      default:
        el.style.background = color;
        el.style.borderRadius = "50%";
    }
    return el;
  }

  private colorFor(s: GeoSource): string {
    const styles = getComputedStyle(document.querySelector(".viz-root") ?? document.body);
    const idx = SOURCE_ORDER.indexOf(s);
    if (idx < 0) return styles.getPropertyValue("--text-muted").trim() || "#888";
    return styles.getPropertyValue(`--series-${idx + 1}`).trim();
  }

  render(digest: Digest): void {
    const map = this.ensure(digest.home);
    for (const m of this.markers) m.remove();
    this.markers = [];
    const bounds = new LngLatBounds();
    bounds.extend([digest.home.lon, digest.home.lat]);

    // Home marker is deliberately not a series colour — it is not a category.
    const homeEl = document.createElement("div");
    homeEl.style.width = "15px";
    homeEl.style.height = "15px";
    homeEl.style.border = "3px solid var(--home)";
    homeEl.style.borderRadius = "50%";
    homeEl.style.boxShadow = "0 0 0 2px var(--surface-1)";
    homeEl.title = digest.home.label;
    this.markers.push(
      new Marker({ element: homeEl })
        .setLngLat([digest.home.lon, digest.home.lat])
        .setPopup(new Popup({ offset: 14 }).setText(digest.home.label))
        .addTo(map),
    );

    // Farthest first, so the nearest papers end up drawn on top.
    const ordered = [...digest.papers]
      .filter((p) => p.distance_km != null)
      .sort((a, b) => (b.distance_km ?? 0) - (a.distance_km ?? 0));

    for (const paper of ordered) {
      for (const pin of pinsFor(paper)) {
        const el = this.marker(this.colorFor(pin.source), pin.source);
        const where = [pin.label, pin.city, pin.country].filter(Boolean).join(", ");
        const popup = new Popup({ offset: 14, maxWidth: "260px" }).setHTML(
          `<div style="font:13px/1.4 system-ui">
             <div style="font-weight:600;margin-bottom:3px">${escapeHtml(paper.title)}</div>
             <div>${escapeHtml(where)}</div>
             <div style="color:#666">${formatKm(pin.distance_km)} · ${SOURCE_LABEL[pin.source]}</div>
           </div>`,
        );
        el.addEventListener("click", (e) => {
          e.stopPropagation();
          this.onPick(paper);
        });
        this.markers.push(
          new Marker({ element: el })
            .setLngLat([pin.lon, pin.lat])
            .setPopup(popup)
            .addTo(map),
        );
        bounds.extend([pin.lon, pin.lat]);
      }
    }

    // Open on a view that actually contains the day's papers — a fixed zoom
    // on Munich leaves everything outside Europe off-screen. Only on the
    // first render, so switching tabs does not yank a view you panned.
    if (!this.fitted && !bounds.isEmpty()) {
      this.fitted = true;
      map.fitBounds(bounds, { padding: 48, maxZoom: 9, duration: 0 });
    }
  }

  /** Let the next render re-fit, e.g. after loading a different day. */
  refit(): void {
    this.fitted = false;
  }

  resize(): void {
    this.map?.resize();
  }

  flyTo(lat: number, lon: number): void {
    this.map?.flyTo({ center: [lon, lat], zoom: 9 });
  }
}

export function escapeHtml(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] ?? c,
  );
}

/** Legend: identity is never colour-alone, so every slot is labelled. */
export function legendHtml(): string {
  const row = (cls: string, text: string) =>
    `<div><span class="dot dot-${cls}"></span>${text}</div>`;
  return (
    row("paper_html", SOURCE_LABEL.paper_html) +
    row("author_prior", SOURCE_LABEL.author_prior) +
    row("openalex_work", SOURCE_LABEL.openalex_work)
  );
}
