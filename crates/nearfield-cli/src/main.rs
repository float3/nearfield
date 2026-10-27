//! `nearfield` — build the daily, distance-ranked preprint digest.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nearfield_core::{paper, pipeline, Paper};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "nearfield",
    about = "Rank each day's preprints by how far their authors are from Munich"
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

fn contact() -> String {
    std::env::var("NEARFIELD_CONTACT").unwrap_or_else(|_| "nearfield@example.invalid".into())
}

fn api_key() -> Option<String> {
    std::env::var("OPENALEX_API_KEY").ok().filter(|k| !k.trim().is_empty())
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
            out_dir,
            cache_dir,
        } => {
            let date = date
                .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
            let categories = load_categories(categories)?;
            tracing::info!(
                %date,
                categories = categories.len(),
                prefetch,
                max_lookups,
                "starting run"
            );
            let digest = pipeline::run(pipeline::Config {
                date,
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
            // Written for the workflow to pick up as the push notification body.
            std::fs::write("work/summary.txt", pipeline::summarise(&digest)).ok();
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

        Command::Index { out_dir } => {
            let entries = pipeline::update_index(&out_dir)?;
            println!("{} day(s) indexed", entries.len());
        }
    }
    Ok(())
}
