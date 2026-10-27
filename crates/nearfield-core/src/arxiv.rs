//! Harvesting the day's new submissions from arXiv's RSS feeds.
//!
//! Why per-category feeds and not the archive-level ones: the archive feed for
//! `cs` returns exactly 2000 items, which is a cap, so it silently drops
//! papers. The ~146 category feeds together are complete. Every genuinely new
//! paper appears with `announce_type: new` in its primary category, so we keep
//! only those and dedupe by arXiv id (papers cross-listed into several
//! categories otherwise appear more than once).

use crate::diagnostics::Diagnostics;
use crate::http::Http;
use crate::model::{Author, GeoSource, Paper};
use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::BTreeMap;

pub const FEED_BASE: &str = "https://rss.arxiv.org/rss";

/// One RSS `<item>`, before it becomes a `Paper`.
#[derive(Debug, Default)]
struct Item {
    title: String,
    description: String,
    creators: String,
    categories: Vec<String>,
    announce_type: String,
    guid: String,
}

/// Pull the bare arXiv id out of the strings arXiv puts it in:
/// `oai:arXiv.org:2610.31702v1` or `arXiv:2610.31702v1 Announce Type: new`.
fn extract_id(item: &Item) -> Option<String> {
    let raw = if !item.guid.is_empty() {
        item.guid.rsplit(':').next().unwrap_or_default().to_string()
    } else {
        let d = item.description.strip_prefix("arXiv:")?;
        d.split_whitespace().next()?.to_string()
    };
    let bare = raw.split('v').next().unwrap_or(&raw).trim().to_string();
    if bare.is_empty() {
        None
    } else {
        Some(bare)
    }
}

/// The description field is `arXiv:ID Announce Type: new \nAbstract: <text>`.
fn extract_abstract(description: &str) -> String {
    match description.find("Abstract:") {
        Some(i) => description[i + "Abstract:".len()..].trim().to_string(),
        None => description.trim().to_string(),
    }
}

/// `dc:creator` is a comma-separated author list. Splitting on commas is the
/// only option the feed gives us; we repair the common name-suffix case.
fn split_authors(creators: &str) -> Vec<Author> {
    const SUFFIXES: [&str; 8] = ["Jr.", "Jr", "Sr.", "Sr", "II", "III", "IV", "PhD"];
    let mut out: Vec<String> = Vec::new();
    for part in creators.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if SUFFIXES.contains(&p) {
            if let Some(last) = out.last_mut() {
                last.push_str(", ");
                last.push_str(p);
                continue;
            }
        }
        out.push(p.to_string());
    }
    out.into_iter().map(Author::new).collect()
}

fn parse_feed(xml: &str, announced: &str, diag: &mut Diagnostics) -> Vec<Paper> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut papers = Vec::new();
    let mut item: Option<Item> = None;
    let mut buf_text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" {
                    item = Some(Item::default());
                }
                buf_text.clear();
            }
            Ok(Event::Text(t)) => {
                buf_text.push_str(&t.unescape().unwrap_or_default());
            }
            Ok(Event::CData(t)) => {
                buf_text.push_str(&String::from_utf8_lossy(&t));
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let text = buf_text.trim().to_string();
                if let Some(it) = item.as_mut() {
                    match name.as_str() {
                        "title" => {
                            if it.title.is_empty() {
                                it.title = text;
                            }
                        }
                        "description" => it.description = text,
                        "creator" | "dc:creator" => it.creators = text,
                        "category" => {
                            if !text.is_empty() {
                                it.categories.push(text);
                            }
                        }
                        "announce_type" | "arxiv:announce_type" => it.announce_type = text,
                        "guid" => it.guid = text,
                        "item" => {
                            let it = item.take().expect("inside an item");
                            if it.announce_type == "new" {
                                let parsed_id = extract_id(&it);
                                if parsed_id.is_none() {
                                    // Dropped silently would mean quietly
                                    // losing papers, so say so.
                                    diag.items_unidentified.record(if it.title.is_empty() {
                                        "<untitled item>".to_string()
                                    } else {
                                        it.title.clone()
                                    });
                                }
                                if let Some(id) = parsed_id {
                                    let authors = split_authors(&it.creators);
                                    if authors.is_empty() {
                                        // Without names the paper can never be
                                        // placed, so it is worth reporting.
                                        diag.papers_without_authors
                                            .record(format!("{id}: {}", it.title));
                                    }
                                    papers.push(Paper {
                                        id,
                                        title: it.title.clone(),
                                        abstract_text: extract_abstract(&it.description),
                                        categories: it.categories.clone(),
                                        announced: announced.to_string(),
                                        authors,
                                        affiliations: Vec::new(),
                                        contact_emails: Vec::new(),
                                        distance_km: None,
                                        geo_source: GeoSource::None,
                                        intro: None,
                                        enriched: false,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                buf_text.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::warn!(error = %e, "malformed RSS, keeping what we parsed");
                break;
            }
            _ => {}
        }
    }
    papers
}

/// Harvest every listed category, dedupe by id, and merge category lists so a
/// cross-listed paper keeps all of its categories.
pub async fn harvest(
    http: &Http,
    categories: &[String],
    announced: &str,
    diag: &mut Diagnostics,
) -> Result<Vec<Paper>> {
    let mut merged: BTreeMap<String, Paper> = BTreeMap::new();
    diag.feeds_attempted = categories.len();

    for cat in categories {
        let url = format!("{FEED_BASE}/{cat}");
        match http.get_text(&url).await {
            Ok(xml) => {
                let found = parse_feed(&xml, announced, diag);
                tracing::info!(cat, count = found.len(), "harvested");
                for p in found {
                    merged
                        .entry(p.id.clone())
                        .and_modify(|existing| {
                            for c in &p.categories {
                                if !existing.categories.contains(c) {
                                    existing.categories.push(c.clone());
                                }
                            }
                        })
                        .or_insert(p);
                }
            }
            Err(e) => {
                diag.feeds_failed.record(format!("{cat}: {e}"));
                tracing::warn!(cat, error = %e, "feed failed, skipping");
            }
        }
    }
    if diag.feeds_failed.count > 0 {
        tracing::warn!(
            failed = diag.feeds_failed.count,
            total = categories.len(),
            "some feeds unavailable"
        );
    }
    Ok(merged.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:arxiv="http://arxiv.org/schemas/atom">
<channel><title>cs.LG</title>
<item>
  <title>Collaborative In-Context Learning</title>
  <link>https://arxiv.org/abs/2610.31702</link>
  <description>arXiv:2610.31702v1 Announce Type: new
Abstract: Indoor radio map estimation is essential.</description>
  <guid isPermaLink="false">oai:arXiv.org:2610.31702v1</guid>
  <category>cs.LG</category>
  <category>cs.AI</category>
  <arxiv:announce_type>new</arxiv:announce_type>
  <dc:creator>Yuanzhe Peng, Jie Xu</dc:creator>
</item>
<item>
  <title>An older paper, revised</title>
  <description>arXiv:2501.00001v3 Announce Type: replace
Abstract: Nope.</description>
  <guid isPermaLink="false">oai:arXiv.org:2501.00001v3</guid>
  <category>cs.LG</category>
  <arxiv:announce_type>replace</arxiv:announce_type>
  <dc:creator>Someone Else</dc:creator>
</item>
</channel></rss>"#;

    #[test]
    fn keeps_new_drops_replacements() {
        let ps = parse_feed(FEED, "2026-10-27", &mut Diagnostics::default());
        assert_eq!(ps.len(), 1, "replacements must be dropped");
        assert_eq!(ps[0].id, "2610.31702");
    }

    #[test]
    fn parses_the_fields_we_depend_on() {
        let ps = parse_feed(FEED, "2026-10-27", &mut Diagnostics::default());
        let p = &ps[0];
        assert_eq!(p.title, "Collaborative In-Context Learning");
        assert_eq!(p.abstract_text, "Indoor radio map estimation is essential.");
        assert_eq!(p.categories, vec!["cs.LG", "cs.AI"]);
        assert_eq!(p.announced, "2026-10-27");
        let names: Vec<_> = p.authors.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Yuanzhe Peng", "Jie Xu"]);
    }

    #[test]
    fn author_suffixes_do_not_become_separate_people() {
        let a = split_authors("Ada Lovelace, John Smith, Jr., Grace Hopper");
        let names: Vec<_> = a.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Ada Lovelace", "John Smith, Jr.", "Grace Hopper"]);
    }

    #[test]
    fn abstract_without_marker_survives() {
        assert_eq!(extract_abstract("just text"), "just text");
    }

    #[test]
    fn garbage_xml_does_not_panic() {
        let _ = parse_feed("<rss><channel><item><title>x", "d", &mut Diagnostics::default());
    }
}
