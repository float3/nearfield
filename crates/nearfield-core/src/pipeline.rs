//! The nightly run, end to end.
//!
//! Order matters. We geolocate from author history *first* so the whole day
//! can be ranked cheaply, then spend the expensive per-paper HTML fetches only
//! on the nearest few — which both fills in introductions and emails and
//! corrects the ranking where the author prior was wrong.
//!
//! Lookup budget: a cold cache needs roughly 12,000 OpenAlex author lookups
//! for a full day, which costs more than the free daily allowance. So lookups
//! are capped per run and the cache is persistent: papers left unplaced today
//! are placed by later runs, and the cache warms until most days need almost
//! no network at all.

use crate::diagnostics::Diagnostics;
use crate::geo::Point;
use crate::http::Http;
use crate::model::{Digest, Home, Paper, Stats};
use crate::openalex::OpenAlex;
use crate::{arxiv, paper, snapshot};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct Config {
    /// Announcement date, YYYY-MM-DD.
    pub date: String,
    /// Where distances are measured from. A trip is worth preparing for: build a digest against the destination and the
    /// nearest papers there get read in full ahead of time.
    pub origin: Point,
    pub origin_label: String,
    pub categories: Vec<String>,
    /// How many of the nearest papers get their introduction and emails
    /// fetched tonight.
    pub prefetch: usize,
    /// How many *unplaced* papers to read for their affiliations. This is the
    /// only lever that places a paper whose authors OpenAlex has never seen.
    pub prefetch_unplaced: usize,
    /// Ceiling on OpenAlex calls for this run. A safety net; the real limit
    /// is `budget_usd`.
    pub max_lookups: usize,
    /// What this run may spend at OpenAlex, in USD.
    pub budget_usd: f64,
    /// Extra allowance reserved for resolving affiliations read off papers.
    pub affiliation_budget_usd: f64,
    /// How many *paid* author lookups a single paper may consume.
    ///
    /// A day's biggest paper has 251 authors. Without a cap it would eat a
    /// quarter of the daily allowance on its own, so the budget is spread
    /// across many papers instead — three authors is usually enough to place
    /// one, and cached authors are free and do not count against it.
    pub max_authors_per_paper: usize,
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub contact: String,
    pub api_key: Option<String>,
    /// Skip the harvest and re-process an existing digest instead.
    pub reuse_harvest: bool,
    /// The offline author index (`nearfield index-authors`). Missing is
    /// fine: the run then pays for every new name as it always did.
    pub snapshot: Option<PathBuf>,
}

impl Config {
    pub fn digest_path(&self) -> PathBuf {
        self.out_dir.join(format!("{}.json", self.date))
    }
}

/// Sort nearest-first, with unlocated papers last. Ties break on id so the
/// output is stable and diffs stay meaningful.
fn sort_by_distance(papers: &mut [Paper]) {
    papers.sort_by(|a, b| {
        let da = a.distance_km.unwrap_or(f64::INFINITY);
        let db = b.distance_km.unwrap_or(f64::INFINITY);
        da.partial_cmp(&db)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn stats_for(papers: &[Paper]) -> Stats {
    let located = papers.iter().filter(|p| p.distance_km.is_some()).count();
    Stats {
        harvested: papers.len(),
        located,
        unlocated: papers.len() - located,
        enriched: papers.iter().filter(|p| p.enriched).count(),
        with_email: papers.iter().filter(|p| !p.emails().is_empty()).count(),
    }
}

pub async fn run(cfg: Config) -> Result<Digest> {
    let http = Http::new(&cfg.contact)?;
    // arXiv asks bulk users for one request every three seconds; OpenAlex
    // allows ten per second but we stay well under it.
    http.set_rate("rss.arxiv.org", Duration::from_secs(3)).await;
    http.set_rate("arxiv.org", Duration::from_secs(3)).await;
    http.set_rate("api.openalex.org", Duration::from_millis(120))
        .await;
    http.set_rate("nominatim.openstreetmap.org", Duration::from_secs(1))
        .await;

    std::fs::create_dir_all(&cfg.out_dir).ok();
    std::fs::create_dir_all(&cfg.cache_dir).ok();

    let mut diag = Diagnostics::default();

    // 1. Harvest the day. The digest is named after the announcement the
    //    feeds actually carry, not the local clock — see arxiv::announced_on.
    let mut announced = cfg.date.clone();
    let mut papers = if cfg.reuse_harvest && cfg.digest_path().exists() {
        let s = std::fs::read_to_string(cfg.digest_path())?;
        let d: Digest = serde_json::from_str(&s).context("reading existing digest")?;
        tracing::info!(count = d.papers.len(), "reusing existing harvest");
        let mut papers = d.papers;
        // The stored distances were measured from whatever origin built it.
        if (d.home.lat - cfg.origin.lat).abs() > f64::EPSILON
            || (d.home.lon - cfg.origin.lon).abs() > f64::EPSILON
        {
            tracing::info!(
                from = %d.home.label,
                to = %cfg.origin_label,
                "re-ranking the existing harvest against a new origin"
            );
            for p in papers.iter_mut() {
                p.reproject(&cfg.origin);
            }
        }
        papers
    } else {
        let h = arxiv::harvest(&http, &cfg.categories, &cfg.date, &mut diag).await?;
        if let Some(d) = h.announced.clone() {
            if d != cfg.date {
                tracing::info!(
                    requested = %cfg.date,
                    feed = %d,
                    "naming the digest after the feed's announcement date"
                );
            }
            announced = d;
        }
        tracing::info!(count = h.papers.len(), "harvest complete");
        if let Some(repeat) = nothing_new(&cfg.out_dir, &announced, &h.papers) {
            return Err(repeat.into());
        }
        h.papers
    };

    // 2. Place authors from publication history, within budget.
    let mut oa = OpenAlex::new(
        http.clone(),
        cfg.api_key.clone(),
        &cfg.cache_dir,
        cfg.origin,
    )?;
    oa.budget_usd = cfg.budget_usd;
    oa.max_lookups = cfg.max_lookups;
    match cfg.snapshot.as_deref().filter(|p| p.exists()) {
        Some(path) => {
            let idx = snapshot::AuthorIndex::load(path)
                .with_context(|| format!("loading author index {}", path.display()))?;
            tracing::info!(
                keys = idx.len(),
                institutions = idx.institutions_len(),
                release = idx.release(),
                "offline author index loaded"
            );
            diag.snapshot_release = Some(idx.release().to_string());
            oa.snapshot = Some(Arc::new(idx));
        }
        None => tracing::info!(
            "no offline author index; every new name costs an API search \
             (build one with `nearfield index-authors`)"
        ),
    }

    // Every paper is visited even once the budget is spent: the snapshot
    // and the cache keep answering for free, and `place_author` simply
    // declines to pay. Before the index existed the loop stopped at the
    // first refusal, which left the rest of the day unplaced.
    for p in papers.iter_mut() {
        let mut paid_here = 0usize;
        for a in p.authors.iter_mut() {
            // Only *paid* lookups count against the per-paper cap; a cached
            // or indexed author costs nothing and is always worth placing.
            let before = oa.spent_usd;
            oa.place_author(a, &mut diag).await;
            if oa.spent_usd > before {
                paid_here += 1;
                if paid_here >= cfg.max_authors_per_paper {
                    break;
                }
            }
        }
        p.resolve_distance();
    }
    diag.lookup_budget_hit = oa.refused > 0;
    if oa.refused > 0 {
        tracing::warn!(
            refused = oa.refused,
            spent_usd = format!("{:.3}", oa.spent_usd),
            budget_usd = format!("{:.2}", cfg.budget_usd),
            lookups = oa.lookups,
            "lookup budget reached; the remaining names will be tried by a later run"
        );
    }
    tracing::info!(
        from_snapshot = oa.snapshot_hits,
        api_calls = oa.lookups,
        spent_usd = format!("{:.3}", oa.spent_usd),
        "authors placed"
    );
    oa.save_caches()?;

    // 3. Rank, then spend real fetches on the nearest few.
    sort_by_distance(&mut papers);

    let n = cfg.prefetch.min(papers.len());
    diag.enrich_attempted = n;
    let mut enriched_ok = 0usize;
    for p in papers.iter_mut().take(n) {
        match paper::enrich(&http, p).await {
            Ok(true) => {
                enriched_ok += 1;
                // Affiliations off the paper override the author prior.
                for aff in p.affiliations.iter_mut() {
                    // Affiliations are authoritative and their cache saturates
                    // — there are only so many institutions — so they get a
                    // reserved slice of the allowance beyond the author sweep.
                    oa.budget_usd = cfg.budget_usd + cfg.affiliation_budget_usd;
                    // Whether we could afford to ask at all decides which
                    // bucket a failure belongs in below.
                    let asked = !oa.out_of_budget();
                    oa.place_affiliation(aff).await;
                    if aff.institution.is_none() {
                        // Distinguish a genuine miss from one we never asked
                        // about: only the former says anything about parsing.
                        if asked {
                            diag.affiliations_unresolved.record(aff.raw.clone());
                        } else {
                            diag.affiliations_unchecked.record(aff.raw.clone());
                        }
                    }
                }
                if p.intro.is_none() {
                    diag.intro_not_found.record(p.id.clone());
                }
                if p.emails().is_empty() {
                    diag.emails_not_found.record(p.id.clone());
                }
                p.resolve_distance();
            }
            Ok(false) => diag.html_absent.record(p.id.clone()),
            Err(e) => {
                diag.html_errors.record(format!("{}: {e}", p.id));
                tracing::warn!(id = %p.id, error = %e, "enrichment failed");
            }
        }
    }
    diag.openalex_degraded = oa.degraded();
    diag.spent_usd = oa.spent_usd;
    diag.lookups = oa.lookups;
    diag.authors_from_snapshot = oa.snapshot_hits;
    tracing::info!(
        attempted = n,
        succeeded = enriched_ok,
        spent_usd = format!("{:.3}", oa.spent_usd),
        "enrichment done"
    );
    oa.save_caches()?;

    // 3b. Rescue papers nothing could place.
    //
    // The prefetch above reads the *nearest* papers, which by definition are
    // already located — it can never place anything. A paper with no placed
    // author is usually one whose authors are new to OpenAlex, and the only
    // remaining evidence is the affiliation printed on the paper itself. So
    // spend a second allowance of fetches there, where it can actually change
    // the answer.
    let unplaced: Vec<usize> = papers
        .iter()
        .enumerate()
        .filter(|(_, p)| p.distance_km.is_none() && !p.enriched)
        .map(|(i, _)| i)
        .take(cfg.prefetch_unplaced)
        .collect();

    let attempted = unplaced.len();
    let mut rescued = 0usize;
    for i in unplaced {
        let p = &mut papers[i];
        match paper::enrich(&http, p).await {
            Ok(true) => {
                for aff in p.affiliations.iter_mut() {
                    oa.budget_usd = cfg.budget_usd + cfg.affiliation_budget_usd;
                    let asked = !oa.out_of_budget();
                    oa.place_affiliation(aff).await;
                    if aff.institution.is_none() {
                        if asked {
                            diag.affiliations_unresolved.record(aff.raw.clone());
                        } else {
                            diag.affiliations_unchecked.record(aff.raw.clone());
                        }
                    }
                }
                p.resolve_distance();
                if p.distance_km.is_some() {
                    rescued += 1;
                }
            }
            Ok(false) => diag.html_absent.record(p.id.clone()),
            Err(e) => diag.html_errors.record(format!("{}: {e}", p.id)),
        }
    }
    if attempted > 0 {
        tracing::info!(
            attempted,
            rescued,
            "placed from the paper's own affiliations"
        );
    }
    diag.rescued_from_paper = rescued;

    // 4. Never lose ground to a previous run of this day.
    let stored = cfg.out_dir.join(format!("{announced}.json"));
    if let Ok(text) = std::fs::read_to_string(&stored) {
        if let Ok(prev) = serde_json::from_str::<Digest>(&text) {
            let recovered = carry_forward(&mut papers, &prev, &cfg.origin);
            if recovered > 0 {
                tracing::info!(
                    recovered,
                    was_located = prev.stats.located,
                    "carried forward placements from the stored digest"
                );
            }
        }
    }

    // Correcting distances may have reordered the nearest papers.
    sort_by_distance(&mut papers);

    let digest = Digest {
        date: announced.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        home: Home {
            lat: cfg.origin.lat,
            lon: cfg.origin.lon,
            label: cfg.origin_label.clone(),
        },
        stats: stats_for(&papers),
        diagnostics: diag,
        papers,
    };

    write_digest(&cfg.out_dir, &digest)?;
    update_index(&cfg.out_dir)?;
    Ok(digest)
}

/// Carry forward anything a previous run of the same day already knew.
///
/// Re-running a day used to be a coin flip: a run whose allowance was already
/// spent would overwrite a well-placed digest with a thin one, and the day got
/// worse. It happened — 351 placements replaced by 58. A re-run should only
/// ever add, so every location and every introduction the stored copy has and
/// this run lacks is copied over before writing.
fn carry_forward(papers: &mut [Paper], previous: &Digest, origin: &Point) -> usize {
    use std::collections::HashMap;
    let old: HashMap<&str, &Paper> = previous.papers.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut recovered = 0usize;

    for p in papers.iter_mut() {
        let Some(prev) = old.get(p.id.as_str()) else {
            continue;
        };

        for a in p.authors.iter_mut() {
            if a.located() {
                continue;
            }
            if let Some(was) = prev
                .authors
                .iter()
                .find(|q| q.name == a.name && q.located())
            {
                a.lat = was.lat;
                a.lon = was.lon;
                a.institution = was.institution.clone();
                a.ror = was.ror.clone();
                a.country = was.country.clone();
                a.city = was.city.clone();
                a.geo_source = was.geo_source;
                a.email = a.email.clone().or_else(|| was.email.clone());
                recovered += 1;
            }
        }

        // Affiliations are keyed by the raw text they were read from.
        for f in p.affiliations.iter_mut() {
            if f.lat.is_some() {
                continue;
            }
            if let Some(was) = prev
                .affiliations
                .iter()
                .find(|q| q.raw == f.raw && q.lat.is_some())
            {
                f.lat = was.lat;
                f.lon = was.lon;
                f.institution = was.institution.clone();
                f.ror = was.ror.clone();
                f.country = was.country.clone();
                f.city = was.city.clone();
                recovered += 1;
            }
        }

        // An introduction already fetched is worth more than a second fetch.
        if !p.enriched && prev.enriched {
            p.intro = prev.intro.clone();
            p.affiliations = prev.affiliations.clone();
            p.contact_emails = prev.contact_emails.clone();
            p.enriched = true;
        }

        // Distances came from the stored digest's origin, so recompute.
        p.reproject(origin);
    }
    recovered
}

/// The feeds carried no new announcement, so there is no digest to write.
///
/// arXiv skips announcements on holidays and during outages, and its feeds
/// then either go empty or keep serving a list that was already announced,
/// under the new date. On 2026-10-29 every feed but `cs.AI` was empty, and
/// `cs.AI` still held the 95 papers of the day before.
#[derive(Debug)]
pub struct NothingNew {
    pub harvested: usize,
    pub repeated: usize,
    pub earlier: Option<String>,
}

impl std::fmt::Display for NothingNew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.earlier {
            Some(date) if self.harvested > 0 => write!(
                f,
                "no new announcement: {} of the {} papers harvested are already in the digest for {date}",
                self.repeated, self.harvested
            ),
            _ => write!(f, "no new announcement: the feeds held no new papers"),
        }
    }
}

impl std::error::Error for NothingNew {}

/// Whether a harvest is no new announcement: empty, or at least half made of
/// papers the most recent earlier digest already has. Consecutive
/// announcements share no new submissions, so a real day overlaps by none.
fn nothing_new(out_dir: &Path, announced: &str, papers: &[Paper]) -> Option<NothingNew> {
    let earlier = latest_digest_before(out_dir, announced);
    if papers.is_empty() {
        return Some(NothingNew {
            harvested: 0,
            repeated: 0,
            earlier: earlier.map(|d| d.date),
        });
    }
    let earlier = earlier?;
    let known: std::collections::HashSet<&str> =
        earlier.papers.iter().map(|p| p.id.as_str()).collect();
    let repeated = papers
        .iter()
        .filter(|p| known.contains(p.id.as_str()))
        .count();
    (repeated * 2 >= papers.len()).then(|| NothingNew {
        harvested: papers.len(),
        repeated,
        earlier: Some(earlier.date.clone()),
    })
}

/// The newest digest in `out_dir` dated before `date`.
fn latest_digest_before(out_dir: &Path, date: &str) -> Option<Digest> {
    let mut dates: Vec<String> = std::fs::read_dir(out_dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_string))
        .filter(|stem| stem.len() == 10 && stem.as_bytes()[4] == b'-' && stem.as_str() < date)
        .collect();
    dates.sort();
    let latest = dates.pop()?;
    let s = std::fs::read_to_string(out_dir.join(format!("{latest}.json"))).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_digest(out_dir: &Path, digest: &Digest) -> Result<()> {
    let path = out_dir.join(format!("{}.json", digest.date));
    let json = serde_json::to_string(digest)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    tracing::info!(path = %path.display(), papers = digest.papers.len(), "digest written");
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    pub date: String,
    pub papers: usize,
    pub located: usize,
    pub nearest_km: Option<f64>,
}

/// Rebuild the timeline index the app reads to know which days exist.
pub fn update_index(out_dir: &Path) -> Result<Vec<IndexEntry>> {
    let mut entries: Vec<IndexEntry> = Vec::new();
    for e in std::fs::read_dir(out_dir)? {
        let p = e?.path();
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.ends_with(".json") || name == "index.json" || name == "categories.json" {
            continue;
        }
        let Ok(s) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(d) = serde_json::from_str::<Digest>(&s) else {
            tracing::warn!(file = %name, "skipping unreadable digest");
            continue;
        };
        entries.push(IndexEntry {
            date: d.date,
            papers: d.stats.harvested,
            located: d.stats.located,
            nearest_km: d.papers.first().and_then(|p| p.distance_km),
        });
    }
    entries.sort_by(|a, b| b.date.cmp(&a.date));
    let path = out_dir.join("index.json");
    std::fs::write(&path, serde_json::to_string_pretty(&entries)?)?;
    Ok(entries)
}

/// Name the place responsible for a paper's distance.
///
/// This must agree with `resolve_distance`: the paper's distance is its
/// *closest* author or affiliation, so naming the first located author instead
/// would print a institution thousands of km from the number beside it.
fn nearest_place(paper: &Paper) -> Option<String> {
    let target = paper.distance_km?;
    let close = |d: Option<f64>| d.is_some_and(|d| (d - target).abs() < 0.5);

    paper
        .authors
        .iter()
        .find(|a| close(a.distance_km))
        .and_then(|a| a.institution.clone())
        .or_else(|| {
            paper
                .affiliations
                .iter()
                .find(|a| close(a.distance_km))
                .and_then(|a| a.institution.clone())
        })
}

/// A one-line summary for the morning push notification.
pub fn summarise(digest: &Digest) -> String {
    match digest.papers.iter().find(|p| p.distance_km.is_some()) {
        Some(nearest) => {
            let where_ = nearest_place(nearest).unwrap_or_else(|| "unknown".into());
            format!(
                "{} new papers. Nearest {}: {} ({:.0} km) — {}",
                digest.stats.harvested,
                digest.home.label,
                where_,
                nearest.distance_km.unwrap_or_default(),
                nearest.title,
            )
        }
        None => format!("{} new papers, none placed yet.", digest.stats.harvested),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{MUNICH, MUNICH_LABEL};
    use crate::model::{Author, GeoSource};

    fn p(id: &str, d: Option<f64>) -> Paper {
        let mut pa = Paper {
            id: id.into(),
            title: format!("paper {id}"),
            abstract_text: String::new(),
            categories: vec![],
            announced: "2026-10-27".into(),
            authors: vec![Author::new("A")],
            affiliations: vec![],
            contact_emails: vec![],
            distance_km: d,
            geo_source: if d.is_some() {
                GeoSource::AuthorPrior
            } else {
                GeoSource::None
            },
            intro: None,
            enriched: false,
        };
        pa.authors[0].distance_km = d;
        pa
    }

    #[test]
    fn nearest_first_unlocated_last() {
        let mut v = vec![p("c", None), p("a", Some(6000.0)), p("b", Some(4.0))];
        sort_by_distance(&mut v);
        let ids: Vec<_> = v.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
    }

    #[test]
    fn ordering_is_stable_on_ties() {
        let mut v = vec![p("z", Some(10.0)), p("a", Some(10.0))];
        sort_by_distance(&mut v);
        assert_eq!(v[0].id, "a");
    }

    #[test]
    fn stats_count_located_and_unlocated() {
        let s = stats_for(&[p("a", Some(1.0)), p("b", None)]);
        assert_eq!(s.harvested, 2);
        assert_eq!(s.located, 1);
        assert_eq!(s.unlocated, 1);
    }

    #[test]
    fn summary_names_the_place_that_set_the_distance() {
        // Regression: the summary used to name the first located author, so a
        // paper 671 km away via Delft was announced as "Laurentian University".
        let mut pa = p("a", None);
        pa.authors = vec![Author::new("Rui Wang"), Author::new("Kubilay Atasu")];
        pa.authors[0].institution = Some("Laurentian University".into());
        pa.authors[0].distance_km = Some(6526.8);
        pa.authors[0].lat = Some(46.49);
        pa.authors[0].lon = Some(-80.99);
        pa.authors[1].institution = Some("Delft University of Technology".into());
        pa.authors[1].distance_km = Some(670.7);
        pa.authors[1].lat = Some(52.00667);
        pa.authors[1].lon = Some(4.35556);
        pa.resolve_distance();

        assert_eq!(pa.distance_km, Some(670.7));
        assert_eq!(
            nearest_place(&pa).as_deref(),
            Some("Delft University of Technology")
        );
    }

    #[test]
    fn a_rerun_never_loses_placements() {
        // Regression: re-running 2026-10-27 on a spent budget replaced a
        // digest with 351 placements by one with 58.
        let mut fresh = p("a", None);
        fresh.authors = vec![Author::new("Nils Thuerey")];

        let mut stored = p("a", None);
        let mut known = Author::new("Nils Thuerey");
        known.lat = Some(48.13743);
        known.lon = Some(11.57549);
        known.institution = Some("Technical University of Munich".into());
        known.geo_source = GeoSource::AuthorPrior;
        stored.authors = vec![known];
        stored.reproject(&MUNICH);

        let previous = Digest {
            date: "2026-10-27".into(),
            generated_at: "then".into(),
            home: Home {
                lat: MUNICH.lat,
                lon: MUNICH.lon,
                label: MUNICH_LABEL.into(),
            },
            stats: stats_for(std::slice::from_ref(&stored)),
            diagnostics: Diagnostics::default(),
            papers: vec![stored],
        };

        let mut papers = vec![fresh];
        let recovered = carry_forward(&mut papers, &previous, &MUNICH);

        assert_eq!(recovered, 1);
        assert!(
            papers[0].distance_km.unwrap() < 2.0,
            "the placement came back"
        );
        assert_eq!(
            papers[0].authors[0].institution.as_deref(),
            Some("Technical University of Munich")
        );
    }

    #[test]
    fn a_rerun_does_not_overwrite_a_better_placement() {
        // The fresh run knows where someone is; the stored one is stale.
        let mut fresh = p("a", None);
        let mut now = Author::new("Someone");
        now.lat = Some(48.13743);
        now.lon = Some(11.57549);
        now.institution = Some("Fresh Institute".into());
        fresh.authors = vec![now];
        fresh.reproject(&MUNICH);

        let mut stored = p("a", None);
        let mut was = Author::new("Someone");
        was.lat = Some(42.3601);
        was.lon = Some(-71.0942);
        was.institution = Some("Stale Institute".into());
        stored.authors = vec![was];

        let previous = Digest {
            date: "2026-10-27".into(),
            generated_at: "then".into(),
            home: Home {
                lat: MUNICH.lat,
                lon: MUNICH.lon,
                label: MUNICH_LABEL.into(),
            },
            stats: stats_for(std::slice::from_ref(&stored)),
            diagnostics: Diagnostics::default(),
            papers: vec![stored],
        };

        let mut papers = vec![fresh];
        carry_forward(&mut papers, &previous, &MUNICH);
        assert_eq!(
            papers[0].authors[0].institution.as_deref(),
            Some("Fresh Institute"),
            "a fresh placement must win over a stored one"
        );
    }

    #[test]
    fn summary_handles_a_day_with_nothing_placed() {
        let d = Digest {
            date: "2026-10-27".into(),
            generated_at: "now".into(),
            home: Home {
                lat: MUNICH.lat,
                lon: MUNICH.lon,
                label: MUNICH_LABEL.into(),
            },
            stats: stats_for(&[p("a", None)]),
            diagnostics: Diagnostics::default(),
            papers: vec![p("a", None)],
        };
        assert!(summarise(&d).contains("none placed"));
    }

    fn digest_on(date: &str, papers: Vec<Paper>) -> Digest {
        Digest {
            date: date.into(),
            generated_at: "then".into(),
            home: Home {
                lat: MUNICH.lat,
                lon: MUNICH.lon,
                label: MUNICH_LABEL.into(),
            },
            stats: stats_for(&papers),
            diagnostics: Diagnostics::default(),
            papers,
        }
    }

    #[test]
    fn a_repeated_or_empty_harvest_is_no_new_announcement() {
        let dir = std::env::temp_dir().join(format!("nf-repeat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_digest(&dir, &digest_on("2026-10-27", vec![p("old", None)])).unwrap();
        write_digest(
            &dir,
            &digest_on("2026-10-28", vec![p("a", None), p("b", None)]),
        )
        .unwrap();

        let repeat = nothing_new(
            &dir,
            "2026-10-29",
            &[p("a", None), p("b", None), p("c", None)],
        )
        .expect("two of three papers are the day before's");
        assert_eq!((repeat.harvested, repeat.repeated), (3, 2));
        assert_eq!(repeat.earlier.as_deref(), Some("2026-10-28"));

        assert!(nothing_new(&dir, "2026-10-29", &[]).is_some());
        assert!(nothing_new(
            &dir,
            "2026-10-29",
            &[p("c", None), p("d", None), p("a", None)]
        )
        .is_none());
        assert!(nothing_new(&dir, "2026-10-28", &[p("a", None)]).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
