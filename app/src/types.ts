// Mirrors the Rust types in nearfield-core::model. Kept hand-synced; the
// pipeline writes these shapes verbatim into data/<date>.json.

export type GeoSource = "paper_html" | "author_prior" | "openalex_work" | "none";

export interface Author {
  name: string;
  email?: string;
  affiliation?: string;
  institution?: string;
  ror?: string;
  country?: string;
  city?: string;
  lat?: number;
  lon?: number;
  distance_km?: number;
  geo_source: GeoSource;
}

export interface Affiliation {
  raw: string;
  institution?: string;
  ror?: string;
  country?: string;
  city?: string;
  lat?: number;
  lon?: number;
  distance_km?: number;
}

export interface Paper {
  id: string;
  title: string;
  abstract: string;
  categories: string[];
  announced: string;
  authors: Author[];
  affiliations?: Affiliation[];
  contact_emails?: string[];
  distance_km?: number;
  geo_source: GeoSource;
  intro?: string;
  enriched: boolean;
}

export interface Stats {
  harvested: number;
  located: number;
  unlocated: number;
  enriched: number;
  with_email: number;
}

export interface Issue {
  kind: string;
  what: string;
  fix?: string;
  count: number;
  samples?: string[];
  actionable: boolean;
}

export interface Tally {
  count: number;
  samples?: string[];
}

export interface Diagnostics {
  feeds_attempted: number;
  enrich_attempted: number;
  lookup_budget_hit: boolean;
  openalex_degraded: boolean;
  [key: string]: unknown;
}

export interface Digest {
  date: string;
  generated_at: string;
  home: { lat: number; lon: number; label: string };
  stats: Stats;
  diagnostics?: Diagnostics;
  papers: Paper[];
}

export interface IndexEntry {
  date: string;
  papers: number;
  located: number;
  nearest_km?: number;
}

/** Every distinct point a paper can be pinned to, with who it came from. */
export interface Pin {
  lat: number;
  lon: number;
  label: string;
  city?: string;
  country?: string;
  source: GeoSource;
  distance_km: number;
}

export function pinsFor(paper: Paper): Pin[] {
  const pins: Pin[] = [];
  const seen = new Set<string>();
  const add = (p: Pin) => {
    const key = `${p.lat.toFixed(3)},${p.lon.toFixed(3)}`;
    if (!seen.has(key)) {
      seen.add(key);
      pins.push(p);
    }
  };

  for (const a of paper.authors) {
    if (a.lat != null && a.lon != null && a.distance_km != null) {
      add({
        lat: a.lat,
        lon: a.lon,
        label: a.institution ?? a.name,
        city: a.city,
        country: a.country,
        source: a.geo_source,
        distance_km: a.distance_km,
      });
    }
  }
  for (const f of paper.affiliations ?? []) {
    if (f.lat != null && f.lon != null && f.distance_km != null) {
      add({
        lat: f.lat,
        lon: f.lon,
        label: f.institution ?? f.raw,
        city: f.city,
        country: f.country,
        source: "paper_html",
        distance_km: f.distance_km,
      });
    }
  }
  return pins;
}

export function allEmails(paper: Paper): string[] {
  const out: string[] = [];
  for (const a of paper.authors) if (a.email) out.push(a.email);
  for (const e of paper.contact_emails ?? []) out.push(e);
  return [...new Set(out)];
}

export function formatKm(km: number | undefined): string {
  if (km == null) return "—";
  // Sub-10km deserves a decimal: the difference between the TUM main campus
  // and Garching is the difference between a walk and an S-Bahn ride.
  if (km < 10) return `${km.toFixed(1)} km`;
  return `${Math.round(km).toLocaleString("en-GB")} km`;
}

export const SOURCE_LABEL: Record<GeoSource, string> = {
  paper_html: "From the paper",
  author_prior: "From author history",
  openalex_work: "From OpenAlex record",
  none: "Not located",
};
