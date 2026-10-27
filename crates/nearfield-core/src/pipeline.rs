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

use crate::geo::MUNICH;
use crate::http::Http;
use crate::model::{Digest, Home, Paper, Stats};
use crate::openalex::OpenAlex;
use crate::{arxiv, paper};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Config {
    /// Announcement date, YYYY-MM-DD.
    pub date: String,
    pub categories: Vec<String>,
    /// How many of the nearest papers get their introduction and emails
    /// fetched tonight.
    pub prefetch: usize,
    /// Ceiling on OpenAlex calls for this run.
    pub max_lookups: usize,
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub contact: String,
    pub api_key: Option<String>,
    /// Skip the harvest and re-process an existing digest instead.
    pub reuse_harvest: bool,
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
    http.set_rate("api.openalex.org", Duration::from_millis(120)).await;

    std::fs::create_dir_all(&cfg.out_dir).ok();
    std::fs::create_dir_all(&cfg.cache_dir).ok();

    // 1. Harvest the day.
    let mut papers = if cfg.reuse_harvest && cfg.digest_path().exists() {
        let s = std::fs::read_to_string(cfg.digest_path())?;
        let d: Digest = serde_json::from_str(&s).context("reading existing digest")?;
        tracing::info!(count = d.papers.len(), "reusing existing harvest");
        d.papers
    } else {
        let p = arxiv::harvest(&http, &cfg.categories, &cfg.date).await?;
        tracing::info!(count = p.len(), "harvest complete");
        p
    };

    // 2. Place authors from publication history, within budget.
    let mut oa = OpenAlex::new(http.clone(), cfg.api_key.clone(), &cfg.cache_dir)?;
    let mut budget_hit = false;
    for p in papers.iter_mut() {
        for a in p.authors.iter_mut() {
            if oa.lookups >= cfg.max_lookups {
                budget_hit = true;
                break;
            }
            oa.place_author(a).await;
        }
        p.resolve_distance();
        if budget_hit {
            break;
        }
    }
    if budget_hit {
        tracing::warn!(
            max = cfg.max_lookups,
            "lookup budget reached; remaining papers will be placed by a later run"
        );
    }
    // Papers after the budget cut still need their distance computed from
    // whatever the cache already knew.
    for p in papers.iter_mut() {
        p.resolve_distance();
    }
    oa.save_caches()?;

    // 3. Rank, then spend real fetches on the nearest few.
    sort_by_distance(&mut papers);

    let n = cfg.prefetch.min(papers.len());
    let mut enriched_ok = 0usize;
    for p in papers.iter_mut().take(n) {
        match paper::enrich(&http, p).await {
            Ok(true) => {
                enriched_ok += 1;
                // Affiliations off the paper override the author prior.
                for aff in p.affiliations.iter_mut() {
                    if oa.lookups < cfg.max_lookups.saturating_add(cfg.prefetch * 4) {
                        oa.place_affiliation(aff).await;
                    }
                }
                p.resolve_distance();
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(id = %p.id, error = %e, "enrichment failed"),
        }
    }
    tracing::info!(attempted = n, succeeded = enriched_ok, "enrichment done");
    oa.save_caches()?;

    // 4. Correcting distances may have reordered the nearest papers.
    sort_by_distance(&mut papers);

    let digest = Digest {
        date: cfg.date.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        home: Home { lat: MUNICH.lat, lon: MUNICH.lon, label: "Munich".into() },
        stats: stats_for(&papers),
        papers,
    };

    write_digest(&cfg.out_dir, &digest)?;
    update_index(&cfg.out_dir)?;
    Ok(digest)
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
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        if !name.ends_with(".json") || name == "index.json" || name == "categories.json" {
            continue;
        }
        let Ok(s) = std::fs::read_to_string(&p) else { continue };
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
                "{} new papers. Nearest: {} ({:.0} km) — {}",
                digest.stats.harvested,
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
            geo_source: if d.is_some() { GeoSource::AuthorPrior } else { GeoSource::None },
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
        assert_eq!(nearest_place(&pa).as_deref(), Some("Delft University of Technology"));
    }

    #[test]
    fn summary_handles_a_day_with_nothing_placed() {
        let d = Digest {
            date: "2026-10-27".into(),
            generated_at: "now".into(),
            home: Home { lat: MUNICH.lat, lon: MUNICH.lon, label: "Munich".into() },
            stats: stats_for(&[p("a", None)]),
            papers: vec![p("a", None)],
        };
        assert!(summarise(&d).contains("none placed"));
    }
}
