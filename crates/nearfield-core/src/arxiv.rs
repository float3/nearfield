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
    pub_date: String,
}

/// The day a feed item's announcement went out, which names the digest.
///
/// arXiv sends each announcement at 20:00 US Eastern and stamps it with the
/// following day: the list mailed on the evening of Thursday the 29th, of
/// papers submitted up to 14:00 that afternoon, carries
/// "Fri, 30 Oct 2026 00:00:00 -0400". A digest named after that stamp was a
/// day ahead of the papers in it, and appeared before its own date had begun
/// in Europe. So the digest is named after the evening it went out: the stamp's
/// date in arXiv's own timezone, less one day.
///
/// Taking the date from the feed rather than the wall clock still matters: a
/// run before the feeds are rebuilt reads the previous announcement, and
/// naming it after the clock once published 2447 papers under two dates.
fn announced_on(pub_date: &str) -> Option<String> {
    let dt = chrono::DateTime::parse_from_rfc2822(pub_date.trim()).ok()?;
    let sent = dt.date_naive().pred_opt()?;
    Some(sent.format("%Y-%m-%d").to_string())
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

/// Undo the TeX escapes arXiv leaves in `dc:creator`.
///
/// Real samples: `B{\l}a\.zej Osi\'nski`, `Micha{\l} Zaj\k{a}c`,
/// `Gu{\dh}laugur Johannesson`. Left alone these never match anything.
/// Exact accents matter less than they look, because the index folds
/// diacritics anyway — the point is to recover the letters.
fn decode_tex(s: &str) -> String {
    // Keyed without the leading backslash; the caller has already consumed it.
    const LIGATURES: [(&str, &str); 12] = [
        ("ss", "ß"),
        ("ae", "æ"),
        ("AE", "Æ"),
        ("dh", "ð"),
        ("dj", "đ"),
        ("aa", "å"),
        ("AA", "Å"),
        ("l", "ł"),
        ("L", "Ł"),
        ("o", "ø"),
        ("O", "Ø"),
        ("i", "i"),
    ];
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\\' {
            // Braces are grouping, not content.
            if c != '{' && c != '}' {
                out.push(c);
            }
            continue;
        }
        // A named ligature, longest first so \ae beats \a.
        let rest: String = chars.clone().collect();
        if let Some((seq, rep)) = LIGATURES
            .iter()
            .filter(|(seq, _)| rest.starts_with(*seq))
            .max_by_key(|(seq, _)| seq.len())
        {
            for _ in 0..seq.len() {
                chars.next();
            }
            out.push_str(rep);
            continue;
        }
        // An accent command: \'e, \\"o, \c{c}, \k{a}. Drop the
        // accent and keep the letter it decorates.
        match chars.next() {
            Some(acc) => {
                if acc.is_ascii_alphabetic() {
                    // \c{c} style: skip to the braced letter.
                    while matches!(chars.peek(), Some('{')) {
                        chars.next();
                    }
                }
                for c in chars.by_ref() {
                    if c == '{' {
                        continue;
                    }
                    if c == '}' {
                        break;
                    }
                    out.push(c);
                    break;
                }
                let _ = acc;
            }
            None => break,
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does this look like an institution rather than a person?
///
/// `dc:creator` mixes them: "Haishen Peng, Univ. of Waterloo, New Haven"
/// splits into three "authors", two of which are places. Dropping them stops
/// the digest inventing people and stops them counting as unresolved.
fn looks_like_a_place(s: &str) -> bool {
    const MARKERS: [&str; 20] = [
        "univ",
        "institut",
        "college",
        "school",
        "laborator",
        " lab",
        "labs",
        "academy",
        "hospital",
        "center",
        "centre",
        "department",
        "dept",
        "faculty",
        "cnrs",
        "observator",
        "agency",
        "society",
        "division",
        "corporation",
    ];
    let l = s.to_lowercase();
    MARKERS.iter().any(|m| l.contains(m)) || s.chars().any(|c| c.is_ascii_digit())
}

/// Split `dc:creator` into people.
///
/// The field is comma-separated, but a comma inside a parenthesised
/// affiliation is not a separator — "Yun Wang (Caltech/IPAC), Massimo
/// Robberto (STScI)" is two authors, and splitting blindly produced
/// fragments like "Gregory D. Wirth (BAE Systems -- Space &". So the split
/// tracks parenthesis depth, then strips the affiliation each name carries.
fn split_authors(creators: &str) -> Vec<Author> {
    const SUFFIXES: [&str; 8] = ["Jr.", "Jr", "Sr.", "Sr", "II", "III", "IV", "PhD"];

    let mut pieces: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in creators.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth = (depth - 1).max(0);
                cur.push(c);
            }
            ',' if depth == 0 => {
                pieces.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    pieces.push(cur);

    let mut out: Vec<String> = Vec::new();
    for piece in pieces {
        // Drop the parenthesised affiliation, balanced or not — arXiv
        // truncates these, leaving an unclosed bracket.
        let name = match piece.find(['(', '[']) {
            Some(i) => &piece[..i],
            None => &piece[..],
        };
        let name = decode_tex(name);
        let name = name
            .trim()
            .trim_matches(|c: char| c == '-' || c == ';')
            .trim();
        if name.is_empty() {
            continue;
        }
        if SUFFIXES.contains(&name) {
            if let Some(last) = out.last_mut() {
                last.push_str(", ");
                last.push_str(name);
            }
            continue;
        }
        if looks_like_a_place(name) {
            continue;
        }
        out.push(name.to_string());
    }
    out.into_iter().map(Author::new).collect()
}

fn parse_feed(xml: &str, fallback: &str, diag: &mut Diagnostics) -> Vec<Paper> {
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
                        "pubDate" => it.pub_date = text,
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
                                        announced: announced_on(&it.pub_date)
                                            .unwrap_or_else(|| fallback.to_string()),
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
/// Harvest result: the papers, and which announcement they came from.
pub struct Harvest {
    pub papers: Vec<Paper>,
    /// The date arXiv stamped on the feeds, which is what the digest is named
    /// after. `None` if no feed carried a parseable date.
    pub announced: Option<String>,
}

pub async fn harvest(
    http: &Http,
    categories: &[String],
    fallback: &str,
    diag: &mut Diagnostics,
) -> Result<Harvest> {
    let mut merged: BTreeMap<String, Paper> = BTreeMap::new();
    diag.feeds_attempted = categories.len();

    for cat in categories {
        let url = format!("{FEED_BASE}/{cat}");
        match http.get_text(&url).await {
            Ok(xml) => {
                let found = parse_feed(&xml, fallback, diag);
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
    let papers: Vec<Paper> = merged.into_values().collect();
    // Feeds should agree, but take the most common date rather than trusting
    // whichever category happened to be harvested first.
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &papers {
        *tally.entry(p.announced.as_str()).or_default() += 1;
    }
    let announced = tally
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(d, _)| d.to_string());

    Ok(Harvest { papers, announced })
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
  <pubDate>Tue, 27 Oct 2026 00:00:00 -0400</pubDate>
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
    fn the_announcement_date_comes_from_the_feed() {
        // Regression: a run at 00:34 UTC named an announcement after the
        // clock and published 2447 duplicate papers under two dates.
        let ps = parse_feed(FEED, "2099-01-01", &mut Diagnostics::default());
        assert_eq!(ps[0].announced, "2026-10-26", "the clock must not win");
    }

    #[test]
    fn a_digest_is_named_after_the_evening_it_went_out() {
        // Thursday evening's list is stamped Friday; it is Thursday's digest.
        assert_eq!(
            announced_on("Fri, 30 Oct 2026 00:00:00 -0400").as_deref(),
            Some("2026-10-29")
        );
        // Monday's feed holds Sunday evening's list.
        assert_eq!(
            announced_on("Mon, 02 Nov 2026 00:00:00 -0500").as_deref(),
            Some("2026-11-01")
        );
        // Across a month boundary.
        assert_eq!(
            announced_on("Sun, 01 Nov 2026 00:00:00 -0500").as_deref(),
            Some("2026-10-31")
        );
    }

    #[test]
    fn an_unparseable_date_falls_back() {
        assert!(announced_on("not a date").is_none());
    }

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
        assert_eq!(p.announced, "2026-10-26");
        let names: Vec<_> = p.authors.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Yuanzhe Peng", "Jie Xu"]);
    }

    #[test]
    fn a_parenthesised_affiliation_is_not_a_separator() {
        // Real sample: splitting on every comma produced fragments like
        // "Gregory D. Wirth (BAE Systems -- Space &".
        let a = split_authors(
            "Yun Wang (Caltech/IPAC), Massimo Robberto (STScI),              Gregory D. Wirth (BAE Systems -- Space & Defense, Boulder)",
        );
        let names: Vec<_> = a.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Yun Wang", "Massimo Robberto", "Gregory D. Wirth"]
        );
    }

    #[test]
    fn an_unclosed_affiliation_still_yields_the_name() {
        let a = split_authors("Aditi Vijayan (Arizona State University");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "Aditi Vijayan");
    }

    #[test]
    fn places_are_not_people() {
        // "Haishen Peng, Univ. of Waterloo, New Haven" invented two authors.
        let names: Vec<String> = split_authors("Haishen Peng, Univ. of Waterloo")
            .iter()
            .map(|a| a.name.clone())
            .collect();
        assert_eq!(names, vec!["Haishen Peng"]);
        assert!(
            split_authors("Lanzhou 730000").is_empty(),
            "a postcode is not a person"
        );
        assert!(split_authors("Perimeter Institute for Theoretical Physics").is_empty());
    }

    #[test]
    fn tex_escapes_are_decoded() {
        // Real names from the digest that matched nothing while still escaped.
        // A raw string keeps the TeX exactly as arXiv sends it.
        let got: Vec<String> =
            split_authors(r"B{\l}a\.zej Osi\'nski, Micha{\l} Zaj\k{a}c, Fran\c{c}ois Visconti")
                .iter()
                .map(|a| a.name.clone())
                .collect();

        assert_eq!(got.len(), 3, "got {got:?}");
        assert!(
            !got.iter().any(|n| n.contains('\\')),
            "escape syntax survived: {got:?}"
        );
        assert!(
            !got.iter().any(|n| n.contains('{') || n.contains('}')),
            "braces survived: {got:?}"
        );
        // The letters must come through; the exact accent matters less,
        // because the index folds diacritics before matching.
        assert!(got[0].starts_with("B"), "got {got:?}");
        assert!(got[1].to_lowercase().contains("zaj"), "got {got:?}");
        assert!(got[2].to_lowercase().starts_with("fran"), "got {got:?}");
    }

    #[test]
    fn author_suffixes_do_not_become_separate_people() {
        let a = split_authors("Ada Lovelace, John Smith, Jr., Grace Hopper");
        let names: Vec<_> = a.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Ada Lovelace", "John Smith, Jr.", "Grace Hopper"]
        );
    }

    #[test]
    fn abstract_without_marker_survives() {
        assert_eq!(extract_abstract("just text"), "just text");
    }

    #[test]
    fn garbage_xml_does_not_panic() {
        let _ = parse_feed(
            "<rss><channel><item><title>x",
            "d",
            &mut Diagnostics::default(),
        );
    }
}
