//! Geolocation via OpenAlex.
//!
//! The constraint that shapes this module: OpenAlex does not index an arXiv
//! paper for roughly one to three weeks after it appears, so looking up the
//! *work* is useless for a same-day digest. Looking up the *author* is not —
//! most authors have published before, and their last known institution
//! carries a ROR id and coordinates. That stands in as "where this person
//! probably is" until the paper's own HTML can confirm it.
//!
//! Both lookups are cached on disk and the caches are committed, so a warm
//! repo geolocates most of a day's papers without touching the network.

use crate::cache::JsonCache;
use crate::diagnostics::Diagnostics;
use crate::geo::Point;
use crate::http::Http;
use crate::model::{Author, GeoSource};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

const API: &str = "https://api.openalex.org";

/// What we keep about an author, keyed by normalised name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorRecord {
    /// OpenAlex institution ids, most recent first. Empty means "looked up,
    /// found nothing" — cached so we don't ask again tomorrow.
    pub institution_ids: Vec<String>,
    pub works_count: u32,
}

/// What we keep about an institution, keyed by OpenAlex institution id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstitutionRecord {
    pub display_name: String,
    pub ror: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl InstitutionRecord {
    pub fn point(&self) -> Option<Point> {
        match (self.lat, self.lon) {
            (Some(lat), Some(lon)) => Some(Point { lat, lon }),
            _ => None,
        }
    }
}

// --- wire types ---------------------------------------------------------

#[derive(Deserialize)]
struct AuthorsResponse {
    #[serde(default)]
    results: Vec<AuthorHit>,
}

#[derive(Deserialize)]
struct AuthorHit {
    #[serde(default)]
    works_count: u32,
    #[serde(default)]
    last_known_institutions: Vec<InstitutionRef>,
}

#[derive(Deserialize)]
struct InstitutionRef {
    id: String,
}

#[derive(Deserialize)]
struct InstitutionResponse {
    #[serde(default)]
    display_name: String,
    ror: Option<String>,
    geo: Option<Geo>,
}

#[derive(Deserialize)]
struct InstitutionsResponse {
    #[serde(default)]
    results: Vec<InstitutionResponse>,
}

#[derive(Deserialize)]
struct Geo {
    city: Option<String>,
    country_code: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

// --- client -------------------------------------------------------------

/// A key that has been issued but not yet activated is *refused* by OpenAlex,
/// so we notice that once and fall back to keyless for the rest of the run
/// instead of failing every lookup.
#[derive(Debug, Clone, PartialEq)]
enum KeyState {
    Use(String),
    Keyless,
}

/// Consecutive failures after which we stop asking OpenAlex for the rest of
/// the run.
///
/// Without a key, OpenAlex throttles the whole IP to a shared daily budget and
/// answers 429. Each one costs several seconds of backoff, so a full night's
/// worth of lookups would crawl for hours and blow the CI timeout while
/// achieving nothing. Better to stop, keep what the cache already knows, and
/// let the next run continue.
const FAILURE_LIMIT: usize = 12;

pub struct OpenAlex {
    http: Http,
    key: Arc<Mutex<KeyState>>,
    pub authors: JsonCache<AuthorRecord>,
    pub institutions: JsonCache<InstitutionRecord>,
    pub lookups: usize,
    consecutive_failures: usize,
    tripped: bool,
}

/// Did this failure come from rate limiting rather than a bad request?
fn is_rate_limited(e: &anyhow::Error) -> bool {
    e.to_string().contains("429")
}

/// Cache key for an author name: case- and whitespace-insensitive.
pub fn normalise_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl OpenAlex {
    pub fn new(http: Http, key: Option<String>, cache_dir: &std::path::Path) -> Result<Self> {
        let state = match key.filter(|k| !k.trim().is_empty()) {
            Some(k) => KeyState::Use(k),
            None => KeyState::Keyless,
        };
        Ok(Self {
            http,
            key: Arc::new(Mutex::new(state)),
            authors: JsonCache::load(cache_dir.join("authors.json"))?,
            institutions: JsonCache::load(cache_dir.join("institutions.json"))?,
            lookups: 0,
            consecutive_failures: 0,
            tripped: false,
        })
    }

    async fn url(&self, path: &str, params: &[(&str, &str)]) -> Result<String> {
        let mut all: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if let KeyState::Use(k) = &*self.key.lock().await {
            all.push(("api_key".to_string(), k.clone()));
        }
        let u = reqwest::Url::parse_with_params(&format!("{API}{path}"), &all)?;
        Ok(u.to_string())
    }

    /// Run a GET, and if the key is rejected, drop it and retry keyless once.
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = self.url(path, params).await?;
        match self.http.get_json::<T>(&url).await {
            Ok(v) => Ok(v),
            Err(e) => {
                let had_key = matches!(&*self.key.lock().await, KeyState::Use(_));
                let msg = e.to_string();
                let auth_problem = msg.contains("401") || msg.contains("403");
                if had_key && auth_problem {
                    tracing::warn!(
                        "OpenAlex rejected the API key (not activated yet?); \
                         continuing keyless for this run"
                    );
                    *self.key.lock().await = KeyState::Keyless;
                    let url = self.url(path, params).await?;
                    return self.http.get_json::<T>(&url).await;
                }
                Err(e)
            }
        }
    }

    /// Has the breaker tripped? Once it has, no further network calls are made.
    pub fn degraded(&self) -> bool {
        self.tripped
    }

    fn note_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// A rate-limit refusal is terminal for this run: the daily budget does
    /// not refill while we wait, so every further lookup would pay the same
    /// cost to fail the same way. Stop immediately rather than after a dozen.
    fn note_rate_limited(&mut self) {
        if !self.tripped {
            self.tripped = true;
            tracing::warn!(
                "OpenAlex is rate limiting (daily budget spent?); stopping                  lookups for this run — cached locations still apply"
            );
        }
    }

    fn note_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= FAILURE_LIMIT && !self.tripped {
            self.tripped = true;
            tracing::warn!(
                failures = self.consecutive_failures,
                "OpenAlex keeps failing (throttled?); stopping lookups for                  this run — cached locations apply and the next run continues"
            );
        }
    }

    /// Resolve an institution id to coordinates, caching the result.
    pub async fn institution(&mut self, id: &str) -> Option<InstitutionRecord> {
        if let Some(rec) = self.institutions.get(id) {
            return Some(rec.clone());
        }
        if self.tripped {
            return None;
        }
        let short = id.rsplit('/').next().unwrap_or(id).to_string();
        let path = format!("/institutions/{short}");
        self.lookups += 1;
        let resp: InstitutionResponse = match self
            .get(&path, &[("select", "id,display_name,ror,geo")])
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(id, error = %e, "institution lookup failed");
                if is_rate_limited(&e) {
                    self.note_rate_limited();
                } else {
                    self.note_failure();
                }
                return None;
            }
        };
        self.note_success();
        let geo = resp.geo.unwrap_or(Geo {
            city: None,
            country_code: None,
            latitude: None,
            longitude: None,
        });
        let rec = InstitutionRecord {
            display_name: resp.display_name,
            ror: resp.ror,
            country: geo.country_code,
            city: geo.city,
            lat: geo.latitude,
            lon: geo.longitude,
        };
        self.institutions.put(id.to_string(), rec.clone());
        Some(rec)
    }

    /// Look up an author by name, caching both hits and misses.
    pub async fn author_record(&mut self, name: &str) -> Option<AuthorRecord> {
        let key = normalise_name(name);
        if let Some(rec) = self.authors.get(&key) {
            return Some(rec.clone());
        }
        if self.tripped {
            return None;
        }
        self.lookups += 1;
        let resp: AuthorsResponse = match self
            .get(
                "/authors",
                &[
                    ("search", name),
                    ("per-page", "1"),
                    ("select", "display_name,works_count,last_known_institutions"),
                ],
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(name, error = %e, "author lookup failed");
                if is_rate_limited(&e) {
                    self.note_rate_limited();
                } else {
                    self.note_failure();
                }
                return None;
            }
        };
        self.note_success();
        let rec = match resp.results.into_iter().next() {
            Some(hit) => AuthorRecord {
                institution_ids: hit.last_known_institutions.into_iter().map(|i| i.id).collect(),
                works_count: hit.works_count,
            },
            None => AuthorRecord { institution_ids: Vec::new(), works_count: 0 },
        };
        self.authors.put(key, rec.clone());
        Some(rec)
    }

    /// Fill in an author's location from their publication history.
    ///
    /// Does nothing if the author already has a location from a more
    /// trustworthy source. When an author has several appointments we take the
    /// one closest to Munich: a joint LMU/MIT affiliation means they are
    /// reachable in Munich, which is the question being asked.
    pub async fn place_author(&mut self, author: &mut Author, diag: &mut Diagnostics) {
        if author.geo_source.rank() >= GeoSource::AuthorPrior.rank() && author.located() {
            return;
        }
        let Some(rec) = self.author_record(&author.name).await else { return };
        if rec.institution_ids.is_empty() {
            diag.authors_unknown.record(author.name.clone());
            return;
        }

        let mut best: Option<(f64, InstitutionRecord)> = None;
        for id in rec.institution_ids.iter().take(4) {
            if let Some(inst) = self.institution(id).await {
                match inst.point() {
                    Some(p) => {
                        let d = p.from_munich_km();
                        if best.as_ref().map_or(true, |(bd, _)| d < *bd) {
                            best = Some((d, inst));
                        }
                    }
                    // Resolved, but upstream has no coordinates for it.
                    None if !inst.display_name.is_empty() => {
                        diag.institutions_without_geo.record(inst.display_name.clone());
                    }
                    None => {}
                }
            }
        }
        if let Some((d, inst)) = best {
            author.institution = Some(inst.display_name);
            author.ror = inst.ror;
            author.country = inst.country;
            author.city = inst.city;
            author.lat = inst.lat;
            author.lon = inst.lon;
            author.distance_km = Some(d);
            author.geo_source = GeoSource::AuthorPrior;
        }
    }

    /// Resolve an affiliation string printed on a paper to a located
    /// institution. Cached by the raw string, since the same departments
    /// recur daily.
    pub async fn place_affiliation(&mut self, aff: &mut crate::model::Affiliation) {
        let key = format!("aff:{}", normalise_name(&aff.raw));
        if let Some(rec) = self.institutions.get(&key).cloned() {
            self.fill_affiliation(aff, &rec);
            return;
        }
        if self.tripped {
            return;
        }
        self.lookups += 1;
        let resp: InstitutionsResponse = match self
            .get(
                "/institutions",
                &[
                    ("search", aff.raw.as_str()),
                    ("per-page", "1"),
                    ("select", "id,display_name,ror,geo"),
                ],
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(raw = %aff.raw, error = %e, "affiliation lookup failed");
                if is_rate_limited(&e) {
                    self.note_rate_limited();
                } else {
                    self.note_failure();
                }
                return;
            }
        };
        self.note_success();
        let Some(hit) = resp.results.into_iter().next() else {
            // Cache the miss so tomorrow does not repeat it.
            self.institutions.put(
                key,
                InstitutionRecord {
                    display_name: String::new(),
                    ror: None,
                    country: None,
                    city: None,
                    lat: None,
                    lon: None,
                },
            );
            return;
        };
        let geo = hit.geo.unwrap_or(Geo {
            city: None,
            country_code: None,
            latitude: None,
            longitude: None,
        });
        let rec = InstitutionRecord {
            display_name: hit.display_name,
            ror: hit.ror,
            country: geo.country_code,
            city: geo.city,
            lat: geo.latitude,
            lon: geo.longitude,
        };
        self.institutions.put(key, rec.clone());
        self.fill_affiliation(aff, &rec);
    }

    fn fill_affiliation(
        &self,
        aff: &mut crate::model::Affiliation,
        rec: &InstitutionRecord,
    ) {
        if rec.display_name.is_empty() {
            return;
        }
        aff.institution = Some(rec.display_name.clone());
        aff.ror = rec.ror.clone();
        aff.country = rec.country.clone();
        aff.city = rec.city.clone();
        aff.lat = rec.lat;
        aff.lon = rec.lon;
        aff.distance_km = rec.point().map(|p| p.from_munich_km());
    }

    pub fn save_caches(&mut self) -> Result<()> {
        self.authors.save()?;
        self.institutions.save()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalise_for_caching() {
        assert_eq!(normalise_name("  Yuanzhe   PENG "), "yuanzhe peng");
        assert_eq!(normalise_name("Jie Xu"), normalise_name("jie xu"));
    }

    fn client() -> OpenAlex {
        let dir = std::env::temp_dir().join(format!("nf-oa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        OpenAlex::new(Http::new("test@example.invalid").unwrap(), None, &dir).unwrap()
    }

    #[test]
    fn breaker_trips_after_a_run_of_failures() {
        let mut oa = client();
        assert!(!oa.degraded());
        for _ in 0..FAILURE_LIMIT {
            oa.note_failure();
        }
        assert!(oa.degraded(), "should stop after {FAILURE_LIMIT} failures");
    }

    #[test]
    fn a_success_resets_the_run() {
        let mut oa = client();
        for _ in 0..(FAILURE_LIMIT - 1) {
            oa.note_failure();
        }
        oa.note_success();
        oa.note_failure();
        assert!(!oa.degraded(), "an intermittent failure must not trip it");
    }

    #[test]
    fn institution_without_geo_has_no_point() {
        let rec = InstitutionRecord {
            display_name: "Nowhere".into(),
            ror: None,
            country: None,
            city: None,
            lat: None,
            lon: None,
        };
        assert!(rec.point().is_none());
    }

    #[test]
    fn institution_with_geo_yields_a_point() {
        let rec = InstitutionRecord {
            display_name: "TUM".into(),
            ror: Some("https://ror.org/02kkvpp62".into()),
            country: Some("DE".into()),
            city: Some("Munich".into()),
            lat: Some(48.13743),
            lon: Some(11.57549),
        };
        assert!(rec.point().unwrap().from_munich_km() < 2.0);
    }
}
