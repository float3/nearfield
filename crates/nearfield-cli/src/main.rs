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
        /// Ceiling on OpenAlex lookups this run.
        #[arg(long, default_value_t = 4000)]
        max_lookups: usize,
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
            categories,
            reuse_harvest,
            origin,
            out_dir,
            cache_dir,
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
                out_dir,
                cache_dir,
                contact: contact(),
                api_key: api_key(),
                reuse_harvest,
            })
            .await?;

            let s = &digest.stats;
            println!("{}", pipeline::summarise(&digest));
            println!(
                "harvested {} | located {} | unlocated {} | enriched {} | with email {}",
                s.harvested, s.located, s.unlocated, s.enriched, s.with_email
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
    }
    Ok(())
}
