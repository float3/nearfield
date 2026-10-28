//! `nearfield` — build the daily, distance-ranked preprint digest.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nearfield_core::{paper, pipeline, Paper};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "nearfield",
    about = "Rank each day's preprints by how far their authors are from you"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Harvest, geolocate, rank and write today's digest.
    Run {
        /// Announcement date (YYYY-MM-DD). Defaults to today, UTC.
        #[arg(long)]
        date: Option<String>,
        /// How many of the nearest papers get intro + emails fetched.
        #[arg(long, default_value_t = 50)]
        prefetch: usize,
        /// Hard ceiling on OpenAlex calls. The real limit is --budget.
        #[arg(long, default_value_t = 20000)]
        max_lookups: usize,
        /// What to spend at OpenAlex, in USD. A free key allows $1 a day, and
        /// a fuzzy author search costs $0.001 — so this buys ~900 of them.
        #[arg(long, default_value_t = nearfield_core::openalex::DEFAULT_BUDGET_USD)]
        budget: f64,
        /// Extra allowance for resolving affiliations read off papers.
        #[arg(long, default_value_t = 0.05)]
        affiliation_budget: f64,
        /// Paid author lookups a single paper may consume.
        #[arg(long, default_value_t = 3)]
        max_authors_per_paper: usize,
        /// Comma-separated categories. Defaults to data/categories.json.
        #[arg(long)]
        categories: Option<String>,
        /// Re-rank an existing digest instead of re-harvesting.
        #[arg(long)]
        reuse_harvest: bool,
        /// Where to measure from: "lat,lon" or a place name to look up.
        /// Defaults to Munich. Use this to prepare for a trip — the nearest
        /// papers *there* get read in full ahead of time.
        #[arg(long)]
        origin: Option<String>,
        #[arg(long, default_value = "data")]
        out_dir: PathBuf,
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
        /// The offline author index from `index-authors`. Consulted before
        /// any paid lookup; silently skipped if the file is not there.
        #[arg(long, default_value = "cache/snapshot/authors.idx")]
        snapshot: PathBuf,
    },
    /// Fetch one paper's introduction and emails, printed as JSON. This is
    /// what the app calls for a paper outside the nightly prefetch window.
    Fetch {
        /// Bare arXiv id, e.g. 2610.31702
        id: String,
    },
    /// Look a place up, to check what `--origin` would resolve to.
    Where {
        /// Place name or "lat,lon".
        query: String,
    },
    /// Print what a day's ingestion failed to parse.
    Report {
        /// Date to report on (YYYY-MM-DD). Defaults to the latest digest.
        #[arg(long)]
        date: Option<String>,
        /// Show the sample strings behind each issue.
        #[arg(long)]
        samples: bool,
        #[arg(long, default_value = "data")]
        out_dir: PathBuf,
    },
    /// Rebuild data/index.json from the digests on disk.
    Index {
        #[arg(long, default_value = "data")]
        out_dir: PathBuf,
    },
    /// Build the offline author index from the OpenAlex snapshot, so that
    /// placing an author costs nothing. Resumable: re-run after an
    /// interruption and it continues from the last complete file.
    IndexAuthors {
        /// Where the extracts and the index live. Gitignored; it is large.
        #[arg(long, default_value = "cache/snapshot")]
        dir: PathBuf,
        /// Files fetched at once. The bucket caps each connection at a few
        /// hundred KB/s, so this is what sets the download speed.
        #[arg(long, default_value_t = 24)]
        concurrency: usize,
        /// Drop authors with fewer works than this.
        #[arg(long, default_value_t = 2)]
        min_works: u32,
        /// Do not file authors under their alternative spellings.
        #[arg(long)]
        no_alternatives: bool,
        /// Rebuild the index from extracts already on disk, no network.
        #[arg(long)]
        skip_fetch: bool,
        /// Fetch only; leave the index as it is.
        #[arg(long)]
        skip_build: bool,
        /// After building, report the hit rate on every author name in this
        /// digest, with the misses grouped by what they look like.
        #[arg(long)]
        measure: Option<PathBuf>,
    },
    /// Resolve one author name against the offline index.
    Lookup {
        name: String,
        #[arg(long, default_value = "cache/snapshot/authors.idx")]
        index: PathBuf,
    },
}

/// How many of a digest's author names the index resolves, and what the
/// misses have in common. This is the number the whole design is for.
fn measure_hit_rate(index: &std::path::Path, digest: &std::path::Path) -> Result<()> {
    use nearfield_core::snapshot::AuthorIndex;
    let idx = AuthorIndex::load(index)?;
    let text = std::fs::read_to_string(digest)
        .with_context(|| format!("reading {}", digest.display()))?;
    let d: nearfield_core::Digest = serde_json::from_str(&text)?;
    let mut names: Vec<String> =
        d.papers.iter().flat_map(|p| p.authors.iter().map(|a| a.name.clone())).collect();
    names.sort();
    names.dedup();

    let shape = |n: &str| -> &'static str {
        let initials = n
            .split_whitespace()
            .any(|t| t.trim_end_matches('.').chars().count() == 1 || t.ends_with('.'));
        if initials {
            "has initials"
        } else if n.chars().any(|c| !c.is_ascii()) {
            "non-ASCII"
        } else if n.contains('-') {
            "hyphenated"
        } else if n.split_whitespace().count() == 1 {
            "single token"
        } else if n.split_whitespace().count() >= 4 {
            "four or more tokens"
        } else {
            "plain"
        }
    };

    let mut hits = 0usize;
    let mut by_shape: std::collections::BTreeMap<&str, (usize, usize, Vec<&str>)> =
        Default::default();
    for n in &names {
        let hit = idx.lookup(n).is_some();
        hits += hit as usize;
        let e = by_shape.entry(shape(n)).or_default();
        e.0 += 1;
        if hit {
            e.1 += 1;
        } else if e.2.len() < 8 {
            e.2.push(n);
        }
    }
    println!(
        "index {} ({} keys, release {}): {}/{} distinct names resolved = {:.1}%",
        index.display(),
        idx.len(),
        idx.release(),
        hits,
        names.len(),
        hits as f64 / names.len().max(1) as f64 * 100.0
    );
    for (shape, (total, hit, misses)) in &by_shape {
        println!(
            "  {shape:<20} {hit:>5}/{total:<5} {:5.1}%  misses e.g. {}",
            *hit as f64 / *total as f64 * 100.0,
            misses.join(" | ")
        );
    }
    Ok(())
}

fn load_categories(explicit: Option<String>) -> Result<Vec<String>> {
    if let Some(s) = explicit {
        return Ok(s
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect());
    }
    let path = PathBuf::from("data/categories.json");
    let s = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}; run `nearfield` from the repo root or pass --categories",
            path.display()
        )
    })?;
    Ok(serde_json::from_str(&s)?)
}

/// Resolve `--origin` to a point and a label.
async fn resolve_origin(
    arg: Option<String>,
) -> Result<(nearfield_core::Point, String)> {
    let Some(q) = arg else {
        return Ok((nearfield_core::MUNICH, nearfield_core::MUNICH_LABEL.to_string()));
    };
    if let Some(p) = nearfield_core::Point::parse(&q) {
        return Ok((p, format!("{:.4}, {:.4}", p.lat, p.lon)));
    }
    let http = nearfield_core::http::Http::new(&contact())?;
    http.set_rate("nominatim.openstreetmap.org", std::time::Duration::from_secs(1))
        .await;
    let hits = nearfield_core::geocode::search(&http, &q, 1).await?;
    let hit = hits
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no place matched {q:?}"))?;
    tracing::info!(place = %hit.label, "origin resolved");
    Ok((hit.point(), hit.short_label()))
}

fn contact() -> String {
    std::env::var("NEARFIELD_CONTACT").unwrap_or_else(|_| "nearfield@example.invalid".into())
}

fn api_key() -> Option<String> {
    std::env::var("OPENALEX_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

/// Print the day's parsing failures, worst first.
///
/// The point is to make extraction improvable: each actionable issue carries
/// the raw strings that defeated it, so a fix can be written against real
/// input rather than guessed at.
fn print_report(digest: &nearfield_core::Digest, samples: bool) {
    let d = &digest.diagnostics;
    println!("Parsing report for {}", digest.date);
    println!(
        "  {} papers harvested from {} feeds | {} enriched",
        digest.stats.harvested, d.feeds_attempted, digest.stats.enriched
    );
    if d.lookup_budget_hit {
        println!("  note: the lookup budget was reached; later runs will place the rest");
    }
    if d.openalex_degraded {
        println!("  note: OpenAlex was throttled and lookups stopped early");
    }

    let issues = d.issues();
    if issues.is_empty() {
        println!("  nothing failed to parse");
        return;
    }
    println!();
    for issue in issues {
        let tag = if issue.actionable { "FIX " } else { "note" };
        println!("[{tag}] {:>5}x {}", issue.count, issue.what);
        if issue.actionable && !issue.fix.is_empty() {
            println!("         -> {}", issue.fix);
        }
        if samples {
            for s in issue.samples.iter().take(10) {
                println!("         . {s}");
            }
        }
    }
    if !samples {
        println!();
        println!("Re-run with --samples to see the strings behind each issue.");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            date,
            prefetch,
            max_lookups,
            budget,
            affiliation_budget,
            max_authors_per_paper,
            categories,
            reuse_harvest,
            origin,
            out_dir,
            cache_dir,
            snapshot,
        } => {
            let date = date
                .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
            let categories = load_categories(categories)?;
            let (origin, origin_label) = resolve_origin(origin).await?;
            tracing::info!(
                %date,
                categories = categories.len(),
                prefetch,
                max_lookups,
                budget,
                origin = %origin_label,
                "starting run"
            );
            let digest = pipeline::run(pipeline::Config {
                date,
                origin,
                origin_label,
                categories,
                prefetch,
                max_lookups,
                budget_usd: budget,
                affiliation_budget_usd: affiliation_budget,
                max_authors_per_paper,
                out_dir,
                cache_dir,
                contact: contact(),
                api_key: api_key(),
                reuse_harvest,
                snapshot: Some(snapshot),
            })
            .await?;

            let s = &digest.stats;
            println!("{}", pipeline::summarise(&digest));
            println!(
                "harvested {} | located {} | unlocated {} | enriched {} | with email {}",
                s.harvested, s.located, s.unlocated, s.enriched, s.with_email
            );
            println!(
                "openalex: {} API calls, ${:.3} spent | {} authors from the snapshot index{}",
                digest.diagnostics.lookups,
                digest.diagnostics.spent_usd,
                digest.diagnostics.authors_from_snapshot,
                digest
                    .diagnostics
                    .snapshot_release
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
            println!("parsing: {}", digest.diagnostics.summary());
            if digest.diagnostics.needs_attention() {
                println!("run `nearfield report --samples` to see what to fix");
            }
            // Written for the workflow to pick up as the push notification body.
            std::fs::write(
                "work/summary.txt",
                format!(
                    "{}

Parsing: {}",
                    pipeline::summarise(&digest),
                    digest.diagnostics.summary()
                ),
            )
            .ok();
        }

        Command::Where { query } => {
            let http = nearfield_core::http::Http::new(&contact())?;
            http.set_rate("nominatim.openstreetmap.org", std::time::Duration::from_secs(1))
                .await;
            let hits = nearfield_core::geocode::search(&http, &query, 5).await?;
            if hits.is_empty() {
                anyhow::bail!("no place matched {query:?}");
            }
            for h in hits {
                println!("{:9.4},{:9.4}  {}  [{}]", h.lat, h.lon, h.short_label(), h.kind);
            }
        }

        Command::Fetch { id } => {
            let http = nearfield_core::http::Http::new(&contact())?;
            http.set_rate("arxiv.org", std::time::Duration::from_secs(3)).await;
            let mut p = Paper {
                id: id.clone(),
                title: String::new(),
                abstract_text: String::new(),
                categories: vec![],
                announced: String::new(),
                authors: vec![],
                affiliations: vec![],
                contact_emails: vec![],
                distance_km: None,
                geo_source: nearfield_core::GeoSource::None,
                intro: None,
                enriched: false,
            };
            let found = paper::enrich(&http, &mut p).await?;
            if !found {
                anyhow::bail!("no HTML rendering available for arXiv:{id}");
            }
            println!("{}", serde_json::to_string_pretty(&p)?);
        }

        Command::Report { date, samples, out_dir } => {
            let date = match date {
                Some(d) => d,
                None => {
                    let entries = pipeline::update_index(&out_dir)?;
                    entries
                        .first()
                        .map(|e| e.date.clone())
                        .ok_or_else(|| anyhow::anyhow!("no digests in {}", out_dir.display()))?
                }
            };
            let path = out_dir.join(format!("{date}.json"));
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let digest: nearfield_core::Digest = serde_json::from_str(&text)?;
            print_report(&digest, samples);
        }

        Command::Index { out_dir } => {
            let entries = pipeline::update_index(&out_dir)?;
            println!("{} day(s) indexed", entries.len());
        }

        Command::IndexAuthors {
            dir,
            concurrency,
            min_works,
            no_alternatives,
            skip_fetch,
            skip_build,
            measure,
        } => {
            use nearfield_core::{snapshot, snapshot_fetch};
            std::fs::create_dir_all(&dir)?;
            if !skip_fetch {
                let http = nearfield_core::http::Http::new(&contact())?;
                http.set_rate(snapshot_fetch::BUCKET_HOST, snapshot_fetch::REQUEST_GAP).await;
                let (a, i) = snapshot_fetch::probe(&http).await?;
                tracing::info!(
                    authors_release = %a.date,
                    authors_rows = a.record_count,
                    authors_gb = format!("{:.1}", a.content_length as f64 / 1e9),
                    institutions_release = %i.date,
                    institutions_rows = i.record_count,
                    "snapshot on offer"
                );
                snapshot_fetch::fetch_institutions(&http, &dir).await?;
                let r = snapshot_fetch::fetch_authors(&http, &dir, concurrency).await?;
                println!(
                    "authors: {} of {} files fetched this run, {} rows scanned, {} kept, \
                     {:.2} GB in {:.0} min",
                    r.files_fetched,
                    r.files_total,
                    r.rows_scanned,
                    r.rows_kept,
                    r.bytes_fetched as f64 / 1e9,
                    r.elapsed.as_secs_f64() / 60.0
                );
            }
            if !skip_build {
                let policy =
                    snapshot::BuildPolicy { min_works, alternatives: !no_alternatives };
                let started = std::time::Instant::now();
                let build_dir = dir.clone();
                let r = tokio::task::spawn_blocking(move || snapshot::build(&build_dir, &policy))
                    .await??;
                println!(
                    "index: release {}, {} of {} authors kept, {} keys, {} institution sets, \
                     {} institutions, {:.0} MB, built in {:.0} s",
                    r.release,
                    r.authors_kept,
                    r.authors_seen,
                    r.keys,
                    r.sets,
                    r.institutions,
                    r.bytes as f64 / 1e6,
                    started.elapsed().as_secs_f64()
                );
            }
            if let Some(digest) = measure {
                measure_hit_rate(&dir.join(snapshot::INDEX_FILE), &digest)?;
            }
        }

        Command::Lookup { name, index } => {
            let idx = nearfield_core::snapshot::AuthorIndex::load(&index)?;
            println!("keys tried: {:?}", nearfield_core::names::lookup_keys(&name));
            match idx.lookup(&name) {
                None => println!("not in the index (release {})", idx.release()),
                Some(ids) => {
                    for id in ids {
                        match idx.institution(&id) {
                            Some(i) => println!(
                                "{id}  {}  [{}, {}]  {:?},{:?}",
                                i.display_name,
                                i.city.as_deref().unwrap_or("-"),
                                i.country.as_deref().unwrap_or("-"),
                                i.lat,
                                i.lon
                            ),
                            None => println!("{id}  (not in institution table)"),
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
