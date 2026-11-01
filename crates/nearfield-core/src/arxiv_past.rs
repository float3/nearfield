//! Rebuilding a past day's arXiv list, for digests before the first one.
//!
//! The RSS feeds carry only the current list, so older ones are rebuilt from
//! arXiv's search API by submission time. arXiv closes each day's list at
//! 14:00 US Eastern, Monday to Friday, and sends it out at 20:00 Sunday to
//! Thursday:
//!
//! | list sent | papers submitted between      |
//! |-----------|-------------------------------|
//! | Sunday    | Thursday 14:00 – Friday 14:00 |
//! | Monday    | Friday 14:00 – Monday 14:00   |
//! | Tue–Thu   | day before 14:00 – 14:00      |
//!
//! Checked against published digests: every paper in the window for
//! 2026-10-29 was in it, and 85% of 2026-10-26's. The rest are arXiv's own
//! irregularities, such as papers held for moderation and announced days
//! late, which a submission window cannot see.

use crate::arxiv::{split_authors, Harvest};
use crate::diagnostics::Diagnostics;
use crate::http::Http;
use crate::model::{GeoSource, Paper};
use anyhow::{bail, Context, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashSet;

pub const API_BASE: &str = "https://export.arxiv.org/api/query";
/// The most the API returns in one page.
const PAGE: usize = 2000;

/// Is US Eastern on daylight time at 14:00 on this date? From the second
/// Sunday in March to the first Sunday in November; both switch at 02:00,
/// so by 14:00 the day has its new offset.
fn eastern_dst(day: NaiveDate) -> bool {
    let nth_sunday = |month: u32, n: u8| {
        NaiveDate::from_weekday_of_month_opt(day.year(), month, Weekday::Sun, n)
            .expect("every month has a first and second Sunday")
    };
    day >= nth_sunday(3, 2) && day < nth_sunday(11, 1)
}

/// 14:00 US Eastern on `day`, in UTC.
fn cutoff(day: NaiveDate) -> NaiveDateTime {
    let offset = if eastern_dst(day) { 4 } else { 5 };
    day.and_hms_opt(14 + offset, 0, 0).expect("a valid time")
}

/// The submissions the list sent on the evening of `sent` holds, as a UTC
/// interval closed at the start and open at the end. `None` on Friday and
/// Saturday, when arXiv sends no list.
pub fn submission_window(sent: NaiveDate) -> Option<(NaiveDateTime, NaiveDateTime)> {
    let (from, to) = match sent.weekday() {
        Weekday::Fri | Weekday::Sat => return None,
        Weekday::Sun => (sent - Duration::days(3), sent - Duration::days(2)),
        Weekday::Mon => (sent - Duration::days(3), sent),
        _ => (sent - Duration::days(1), sent),
    };
    Some((cutoff(from), cutoff(to)))
}

/// The newest day before `day` on which arXiv sends a list.
pub fn previous_list_day(day: NaiveDate) -> NaiveDate {
    let mut d = day - Duration::days(1);
    while submission_window(d).is_none() {
        d -= Duration::days(1);
    }
    d
}

/// The API's own range syntax, to the minute and inclusive at both ends.
fn query_url(from: NaiveDateTime, to: NaiveDateTime, start: usize) -> String {
    let last = to - Duration::minutes(1);
    format!(
        "{API_BASE}?search_query=submittedDate:%5B{}+TO+{}%5D&sortBy=submittedDate&sortOrder=ascending&start={start}&max_results={PAGE}",
        from.format("%Y%m%d%H%M"),
        last.format("%Y%m%d%H%M"),
    )
}

/// One Atom `<entry>`, before it becomes a `Paper`.
#[derive(Debug, Default)]
struct Entry {
    id: String,
    title: String,
    summary: String,
    authors: Vec<String>,
    primary: String,
    categories: Vec<String>,
}

/// Collapse the line breaks and runs of spaces the API wraps text in.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one page of results: the entries, and how many the query matched.
fn parse_page(xml: &str) -> (Vec<Entry>, Option<usize>) {
    let mut reader = Reader::from_str(xml);
    let mut entries = Vec::new();
    let mut entry: Option<Entry> = None;
    let mut total = None;
    let mut text = String::new();

    let attr = |e: &quick_xml::events::BytesStart, key: &str| {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == key)
            .and_then(|a| {
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(|v| v.to_string())
            })
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == "entry" {
                    entry = Some(Entry::default());
                }
                text.clear();
            }
            Ok(Event::Empty(e)) => {
                if let Some(en) = entry.as_mut() {
                    match e.name().as_ref() {
                        "arxiv:primary_category" => {
                            en.primary = attr(&e, "term").unwrap_or_default();
                        }
                        "category" => {
                            if let Some(t) = attr(&e, "term") {
                                en.categories.push(t);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(t)) => text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => match r.resolve_char_ref() {
                Ok(Some(c)) => text.push(c),
                _ => match resolve_predefined_entity(r.as_ref()) {
                    Some(s) => text.push_str(s),
                    None => {
                        text.push('&');
                        text.push_str(r.as_ref());
                        text.push(';');
                    }
                },
            },
            Ok(Event::CData(t)) => text.push_str(&t),
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                let t = squash(&text);
                text.clear();
                match (name.as_str(), entry.as_mut()) {
                    ("opensearch:totalResults", _) => total = t.parse().ok(),
                    ("entry", Some(_)) => entries.push(entry.take().expect("inside an entry")),
                    ("id", Some(en)) => {
                        // http://arxiv.org/abs/2610.38659v1
                        let raw = t.rsplit("/abs/").next().unwrap_or_default();
                        en.id = raw.split('v').next().unwrap_or_default().to_string();
                    }
                    ("title", Some(en)) => en.title = t,
                    ("summary", Some(en)) => en.summary = t,
                    ("name", Some(en)) if !t.is_empty() => en.authors.push(t),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::warn!(error = %e, "malformed API response, keeping what we parsed");
                break;
            }
            _ => {}
        }
    }
    (entries, total)
}

/// Rebuild the list arXiv sent on the evening of `date`: every new paper
/// whose primary category is one of `categories`.
pub async fn harvest(
    http: &Http,
    categories: &[String],
    date: &str,
    diag: &mut Diagnostics,
) -> Result<Harvest> {
    let sent = NaiveDate::parse_from_str(date, "%Y-%m-%d").context("--date must be YYYY-MM-DD")?;
    let Some((from, to)) = submission_window(sent) else {
        bail!("arXiv sends no list on a {}", sent.weekday());
    };
    tracing::info!(%from, %to, "rebuilding a past list from submissions (UTC)");
    let wanted: HashSet<&str> = categories.iter().map(String::as_str).collect();

    let mut entries = Vec::new();
    let mut total = None;
    let mut empty_pages = 0;
    while total.is_none_or(|t| entries.len() < t) {
        let url = query_url(from, to, entries.len());
        let (page, t) = parse_page(&http.get_text(&url).await?);
        tracing::info!(start = entries.len(), got = page.len(), total = ?t, "API page");
        if t.is_some() {
            total = t;
        }
        if page.is_empty() {
            // The API now and then returns an empty page mid-query; it
            // answers on a second try.
            empty_pages += 1;
            if empty_pages > 3 {
                bail!(
                    "arXiv API kept returning empty pages at {} of {:?}",
                    entries.len(),
                    total
                );
            }
            continue;
        }
        entries.extend(page);
    }

    let mut seen = HashSet::new();
    let mut papers = Vec::new();
    for en in entries {
        if en.id.is_empty() || !wanted.contains(en.primary.as_str()) || !seen.insert(en.id.clone())
        {
            continue;
        }
        let authors = split_authors(&en.authors.join(", "));
        if authors.is_empty() {
            diag.papers_without_authors
                .record(format!("{}: {}", en.id, en.title));
        }
        papers.push(Paper {
            id: en.id,
            title: en.title,
            abstract_text: en.summary,
            categories: en.categories,
            announced: date.to_string(),
            authors,
            affiliations: Vec::new(),
            contact_emails: Vec::new(),
            distance_km: None,
            geo_source: GeoSource::None,
            intro: None,
            enriched: false,
        });
    }
    tracing::info!(count = papers.len(), "rebuilt the list");
    Ok(Harvest {
        papers,
        announced: Some(date.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn window(s: &str) -> Option<(String, String)> {
        submission_window(d(s)).map(|(a, b)| {
            (
                a.format("%a %d %H:%M").to_string(),
                b.format("%a %d %H:%M").to_string(),
            )
        })
    }

    #[test]
    fn a_weekday_list_holds_one_day_of_submissions() {
        // Thursday 29 October, still on daylight time: 14:00 EDT is 18:00 UTC.
        assert_eq!(
            window("2026-10-29"),
            Some(("Wed 28 18:00".into(), "Thu 29 18:00".into()))
        );
    }

    #[test]
    fn monday_holds_the_weekend_and_sunday_holds_thursday_to_friday() {
        assert_eq!(
            window("2026-10-26"),
            Some(("Fri 23 18:00".into(), "Mon 26 18:00".into()))
        );
        assert_eq!(
            window("2026-10-25"),
            Some(("Thu 22 18:00".into(), "Fri 23 18:00".into()))
        );
    }

    #[test]
    fn no_list_goes_out_on_friday_or_saturday() {
        assert_eq!(window("2026-10-30"), None);
        assert_eq!(window("2026-10-31"), None);
        assert_eq!(previous_list_day(d("2026-11-01")), d("2026-10-29"));
        assert_eq!(previous_list_day(d("2026-10-26")), d("2026-10-25"));
    }

    #[test]
    fn the_cutoff_follows_eastern_daylight_time() {
        // DST ended Sunday 1 November 2026: 14:00 EST is 19:00 UTC.
        assert_eq!(
            window("2026-11-03"),
            Some(("Mon 02 19:00".into(), "Tue 03 19:00".into()))
        );
        // Monday 2 November: Friday was still EDT, Monday is EST.
        assert_eq!(
            window("2026-11-02"),
            Some(("Fri 30 18:00".into(), "Mon 02 19:00".into()))
        );
        // It began Sunday 8 March 2026.
        assert!(!eastern_dst(d("2026-03-07")));
        assert!(eastern_dst(d("2026-03-09")));
    }

    #[test]
    fn the_query_closes_a_minute_before_the_cutoff() {
        let (a, b) = submission_window(d("2026-10-29")).unwrap();
        let url = query_url(a, b, 2000);
        assert!(
            url.contains("submittedDate:%5B202610281800+TO+202610291759%5D"),
            "{url}"
        );
        assert!(url.contains("start=2000"), "{url}");
    }

    const PAGE_XML: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<feed xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/" xmlns:arxiv="http://arxiv.org/schemas/atom" xmlns="http://www.w3.org/2005/Atom">
  <id>https://arxiv.org/api/x</id>
  <title>arXiv Query: search_query=submittedDate</title>
  <opensearch:totalResults>2003</opensearch:totalResults>
  <entry>
    <id>http://arxiv.org/abs/2610.38663v2</id>
    <title>Strings &amp; Branes:
      a  Review</title>
    <summary>  We review
      things.
    </summary>
    <author>
      <name>Øystein Hansen</name>
    </author>
    <author>
      <name>Jane Doe</name>
    </author>
    <arxiv:primary_category term="hep-th"/>
    <category term="hep-th" scheme="http://arxiv.org/schemas/atom"/>
    <category term="gr-qc" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

    #[test]
    fn an_api_page_is_read_into_entries() {
        let (entries, total) = parse_page(PAGE_XML);
        assert_eq!(total, Some(2003));
        let e = &entries[0];
        assert_eq!(e.id, "2610.38663");
        assert_eq!(e.title, "Strings & Branes: a Review");
        assert_eq!(e.summary, "We review things.");
        assert_eq!(e.authors, ["Øystein Hansen", "Jane Doe"]);
        assert_eq!(e.primary, "hep-th");
        assert_eq!(e.categories, ["hep-th", "gr-qc"]);
    }
}
