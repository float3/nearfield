// Where distances are measured from.
//
// Every author and affiliation in a digest carries its own coordinates, so
// changing the origin is pure arithmetic on data already on the phone — no
// network, no re-harvest, works on a train. The digest's own `home` is only
// the default the pipeline happened to rank against.

import type { Digest, Paper } from "./types";

export type OriginKind = "home" | "current" | "custom";

export interface Origin {
  lat: number;
  lon: number;
  label: string;
  kind: OriginKind;
}

const EARTH_RADIUS_KM = 6371.0088;
const STORE_KEY = "nearfield-origin";
const SAVED_KEY = "nearfield-saved-places";

/** Great-circle distance in km. Mirrors geo::Point::distance_km in the core. */
export function distanceKm(
  a: { lat: number; lon: number },
  b: { lat: number; lon: number },
): number {
  const lat1 = (a.lat * Math.PI) / 180;
  const lat2 = (b.lat * Math.PI) / 180;
  const dLat = lat2 - lat1;
  const dLon = ((b.lon - a.lon) * Math.PI) / 180;
  const h =
    Math.sin(dLat / 2) ** 2 +
    Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLon / 2) ** 2;
  return 2 * EARTH_RADIUS_KM * Math.asin(Math.sqrt(h));
}

export function isValid(p: { lat: number; lon: number }): boolean {
  return (
    Number.isFinite(p.lat) &&
    Number.isFinite(p.lon) &&
    p.lat >= -90 &&
    p.lat <= 90 &&
    p.lon >= -180 &&
    p.lon <= 180
  );
}

export function homeOrigin(digest: Digest): Origin {
  return {
    lat: digest.home.lat,
    lon: digest.home.lon,
    label: digest.home.label,
    kind: "home",
  };
}

/**
 * Recompute every distance in a digest against `origin` and re-sort, nearest
 * first with unplaced papers last.
 *
 * Mirrors Paper::reproject and pipeline::sort_by_distance in the core, so the
 * phone and the nightly job agree on what "nearest" means.
 */
export function reproject(digest: Digest, origin: Origin): void {
  for (const paper of digest.papers) {
    let best: number | null = null;
    let bestSource = paper.geo_source;

    for (const a of paper.authors) {
      if (a.lat == null || a.lon == null) {
        a.distance_km = undefined;
        continue;
      }
      a.distance_km = distanceKm(origin, { lat: a.lat, lon: a.lon });
      if (best == null || a.distance_km < best) {
        best = a.distance_km;
        bestSource = a.geo_source;
      }
    }
    for (const f of paper.affiliations ?? []) {
      if (f.lat == null || f.lon == null) {
        f.distance_km = undefined;
        continue;
      }
      f.distance_km = distanceKm(origin, { lat: f.lat, lon: f.lon });
      if (best == null || f.distance_km < best) {
        best = f.distance_km;
        // Affiliations are read off the paper itself.
        bestSource = "paper_html";
      }
    }

    paper.distance_km = best ?? undefined;
    paper.geo_source = best == null ? "none" : bestSource;
  }

  digest.papers.sort((x: Paper, y: Paper) => {
    const dx = x.distance_km ?? Infinity;
    const dy = y.distance_km ?? Infinity;
    return dx === dy ? x.id.localeCompare(y.id) : dx - dy;
  });

  digest.stats.located = digest.papers.filter((p) => p.distance_km != null).length;
  digest.stats.unlocated = digest.papers.length - digest.stats.located;
}

// --- persistence --------------------------------------------------------

export function loadOrigin(): Origin | null {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (!raw) return null;
    const o = JSON.parse(raw) as Origin;
    return isValid(o) ? o : null;
  } catch {
    return null;
  }
}

export function saveOrigin(o: Origin | null): void {
  try {
    if (o === null) localStorage.removeItem(STORE_KEY);
    else localStorage.setItem(STORE_KEY, JSON.stringify(o));
  } catch {
    // A private-mode browser with no storage is not a reason to fail.
  }
}

export interface SavedPlace {
  lat: number;
  lon: number;
  label: string;
}

export function loadSaved(): SavedPlace[] {
  try {
    const raw = localStorage.getItem(SAVED_KEY);
    if (!raw) return [];
    const list = JSON.parse(raw) as SavedPlace[];
    return Array.isArray(list) ? list.filter(isValid) : [];
  } catch {
    return [];
  }
}

export function savePlace(p: SavedPlace): SavedPlace[] {
  const list = loadSaved().filter(
    (q) => !(Math.abs(q.lat - p.lat) < 1e-6 && Math.abs(q.lon - p.lon) < 1e-6),
  );
  list.unshift(p);
  const trimmed = list.slice(0, 12);
  try {
    localStorage.setItem(SAVED_KEY, JSON.stringify(trimmed));
  } catch {
    // ignore
  }
  return trimmed;
}

export function forgetPlace(p: SavedPlace): SavedPlace[] {
  const list = loadSaved().filter(
    (q) => !(Math.abs(q.lat - p.lat) < 1e-6 && Math.abs(q.lon - p.lon) < 1e-6),
  );
  try {
    localStorage.setItem(SAVED_KEY, JSON.stringify(list));
  } catch {
    // ignore
  }
  return list;
}
