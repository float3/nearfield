//! Reading a paper's own HTML for the things no metadata service has:
//! affiliations, contact emails, and the introduction.
//!
//! arXiv renders LaTeX submissions to HTML at `arxiv.org/html/<id>` (LaTeXML),
//! which is far easier and cheaper to mine than the PDF — and crucially, a
//! truncated PDF cannot be parsed at all, since the cross-reference table
//! lives at the end of the file. Papers without an HTML rendering (non-LaTeX
//! submissions, older entries) simply return 404 and stay un-enriched.

use crate::model::{Affiliation, Author, Paper};
use anyhow::Result;
use regex::Regex;
use scraper::{Html, Selector};
use std::sync::OnceLock;

/// Matches an email address. Deliberately conservative: LaTeXML sometimes
/// emits obfuscated forms, and a false positive here becomes a wrong address
/// in a mail client.
fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,24}\b").unwrap())
}

/// Section headings that mean "the introduction", across the numbering styles
/// LaTeXML emits: "1 Introduction", "I Introduction", "Introduction".
fn intro_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*(?:(?:[ivxlc]+|\d+)[.\s]+)?introduction\s*$").unwrap())
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

/// Collapse whitespace and strip the footnote markers LaTeXML leaves inline.
fn clean(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .trim()
        .trim_matches(|c: char| c == ',' || c == ';')
        .to_string()
}

/// Does this string look like an institution rather than a street address or
/// a stray footnote? Requires an institution-ish word.
fn looks_institutional(s: &str) -> bool {
    // Acknowledgement prose mentions institutions without being one. Seen in
    // the parsing report as "The authors thank the International Max Planck
    // Research ...", filed as an affiliation and searched for verbatim.
    const PROSE: [&str; 6] = [
        "the authors thank",
        "we thank",
        "we acknowledge",
        "this work was supported",
        "funded by",
        "we are grateful",
    ];
    let lower = s.to_lowercase();
    if PROSE.iter().any(|p| lower.contains(p)) {
        return false;
    }

    const MARKERS: [&str; 14] = [
        "universit",
        "institut",
        "college",
        "school",
        "laborator",
        "labs",
        "academy",
        "hospital",
        "center",
        "centre",
        "department",
        "faculty",
        "cnrs",
        "max planck",
    ];
    s.len() >= 6 && s.len() <= 300 && MARKERS.iter().any(|m| lower.contains(m))
}

/// What a single paper's HTML yielded.
#[derive(Debug, Default)]
pub struct Extracted {
    pub emails: Vec<String>,
    pub affiliations: Vec<String>,
    pub intro: Option<String>,
}

/// Pull the author-block region: everything LaTeXML puts in `ltx_authors`.
fn extract_author_block(doc: &Html) -> String {
    let mut out = String::new();
    for el in doc.select(&sel("div.ltx_authors")) {
        out.push_str(&el.text().collect::<String>());
        out.push(' ');
    }
    out
}

/// The document's front matter: everything before the first numbered section.
///
/// Contact addresses are often not inside `div.ltx_authors` — LaTeXML drops
/// them into a footnote beside the title or the abstract. Scanning the whole
/// document instead would harvest addresses out of the bibliography and out of
/// other people's cited work, so we stop at the first section.
fn front_matter(doc: &Html, html: &str) -> String {
    let cut = html.find("<section").unwrap_or(html.len());
    let head = &html[..cut];
    // Strip tags so an address split across markup still matches.
    let text: String = Html::parse_fragment(head).root_element().text().collect();
    if text.trim().is_empty() {
        // A document with no <section> at all: fall back to its whole text.
        doc.root_element().text().collect()
    } else {
        text
    }
}

fn push_email(found: &mut Vec<String>, addr: &str) {
    let addr = addr.to_lowercase();
    if email_re().is_match(&addr) && !found.contains(&addr) {
        found.push(addr);
    }
}

fn extract_emails(doc: &Html, author_block: &str, html: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();

    // mailto: links are unambiguous, so take those first.
    for el in doc.select(&sel("a[href^='mailto:']")) {
        if let Some(href) = el.value().attr("href") {
            push_email(
                &mut found,
                href.trim_start_matches("mailto:")
                    .split('?')
                    .next()
                    .unwrap_or(""),
            );
        }
    }
    // Then the author block, then the wider front matter.
    for m in email_re().find_iter(author_block) {
        push_email(&mut found, m.as_str());
    }
    if found.is_empty() {
        let head = front_matter(doc, html);
        for m in email_re().find_iter(&head) {
            push_email(&mut found, m.as_str());
        }
    }
    found
}

/// Prefixes LaTeXML puts in front of an affiliation. `thanks:` comes from
/// `\thanks{}`, `Address:` from the AMS templates. Every one of these was
/// found by reading the parsing report's unresolved-affiliation samples.
const LABELS: [&str; 8] = [
    "Affiliation:",
    "Address:",
    "address:",
    "thanks:",
    "Thanks:",
    "Correspondence:",
    "Note:",
    "note:",
];

/// Labels that introduce a contact address inside an affiliation run.
const EMAIL_LABELS: [&str; 4] = ["E-mail:", "Email:", "email:", "e-mail:"];

/// Tidy one affiliation candidate into something an institution search can
/// actually match, or `None` if it is not an affiliation at all.
///
/// LaTeXML emits two awkward shapes. ACM/IEEE templates render structured
/// affiliations as `organization=X, city=Y, postcode=Z, country=W`, where only
/// the organisation is searchable. Other templates prefix the whole thing with
/// `Affiliation:` and sometimes fold an `Email:` into the same run of text.
fn normalise_affiliation(raw: &str) -> Option<String> {
    let mut s = clean(raw);

    // Structured form: keep the organisation, and the city if present, since
    // "X, Munich" disambiguates multi-campus institutions.
    if let Some(i) = s.find("organization=") {
        let rest = &s[i + "organization=".len()..];
        let org = rest.split(", city=").next().unwrap_or(rest);
        let org = org.split(", postcode=").next().unwrap_or(org);
        let org = org.split(", country=").next().unwrap_or(org).trim();
        let city = rest
            .split_once(", city=")
            .map(|(_, c)| c.split(',').next().unwrap_or("").trim().to_string())
            .filter(|c| !c.is_empty());
        s = match city {
            Some(c) => format!("{org}, {c}"),
            None => org.to_string(),
        };
    } else {
        // Drop an inline address and whatever labels it. Real sample:
        // "thanks: School of Mathematics, ... E-mail: wujincao@shufe.edu.cn."
        for label in EMAIL_LABELS {
            if let Some(i) = s.find(label) {
                let (before, after) = s.split_at(i);
                let after = email_re().replace(&after[label.len()..], "");
                s = clean(&format!("{before} {after}"));
            }
        }
        // Strip the leading label, keeping the last segment when several are
        // concatenated — that is where the institution ends up.
        for label in LABELS {
            while let Some(i) = s.rfind(label) {
                let tail = clean(&s[i + label.len()..]);
                if tail.is_empty() {
                    break;
                }
                s = tail;
            }
        }
    }

    // Last, because the footnote marker sits *behind* the label: LaTeXML emits
    // "Affiliation:\u{a0}1University of North Florida". Stripping the marker
    // before the label would leave the digit glued to the institution name.
    let s = strip_marker(&clean(&s));
    // `\thanks{}` text ends in a full stop that is not part of the name.
    let s = s.trim_end_matches(['.', ',', ';', ' ']).to_string();
    looks_institutional(&s).then_some(s)
}

/// Drop the footnote marker LaTeXML glues to the front of an affiliation —
/// "1University of North Florida" is the superscript 1 from the author list.
/// Only strips when a letter follows, so "3M Company" survives.
fn strip_marker(s: &str) -> String {
    let t = s.trim_start_matches([' ', '*', '†', '‡', '§', '¶', ',', ';']);
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return t.to_string();
    }
    // The marker may be glued on ("1University") or spaced off
    // ("1 Technical University of Munich"); both are footnote markers.
    let rest = t[digits.len()..].trim_start();
    // "3M Company" — a marker is followed by a capital starting a word, and a
    // real name like "3M" has no space before the rest. Require that what
    // follows begins an alphabetic run of at least three characters.
    let alpha_run = rest.chars().take_while(|c| c.is_alphabetic()).count();
    if alpha_run >= 3 && rest.chars().next().is_some_and(|c| c.is_uppercase()) {
        rest.trim().to_string()
    } else {
        t.to_string()
    }
}

/// Add a candidate affiliation if it looks like an institution and is new.
fn push_affiliation(out: &mut Vec<String>, candidate: &str) {
    let Some(c) = normalise_affiliation(candidate) else {
        return;
    };
    if !out.iter().any(|o| o == &c) {
        out.push(c);
    }
}

/// Drop candidates that merely contain another candidate. LaTeXML's author
/// block often yields both a clean "Delft University of Technology" and a
/// concatenated "Delft University of Technology Affiliation: Delft ..." run;
/// the shorter one is the real institution name.
fn drop_concatenations(mut v: Vec<String>) -> Vec<String> {
    let all = v.clone();
    v.retain(|c| !all.iter().any(|o| o != c && c.contains(o.as_str())));
    v
}

fn extract_affiliations(doc: &Html, author_block: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    for s in [
        "span.ltx_role_affiliation",
        "span.ltx_author_notes",
        "span.ltx_contact",
    ] {
        for el in doc.select(&sel(s)) {
            let t = el.text().collect::<String>();
            // An author-notes blob can hold several affiliations at once; the
            // numeric superscripts are gone by now, so split on newlines and
            // only consider the whole blob if that yielded nothing.
            let before = out.len();
            for line in t.split(['\n', '\r']) {
                push_affiliation(&mut out, line);
            }
            if out.len() == before {
                push_affiliation(&mut out, &t);
            }
        }
    }
    if out.is_empty() {
        // Fall back to scanning the author block line by line.
        for line in author_block.split(['\n', '\r']) {
            push_affiliation(&mut out, line);
        }
    }
    drop_concatenations(out)
}

fn extract_intro(doc: &Html) -> Option<String> {
    let heading = sel("h2.ltx_title_section, h2.ltx_title, h1.ltx_title_section");
    for section in doc.select(&sel("section.ltx_section")) {
        let Some(h) = section.select(&heading).next() else {
            continue;
        };
        let title = clean(&h.text().collect::<String>());
        if !intro_re().is_match(&title) {
            continue;
        }
        let mut body = String::new();
        for p in section.select(&sel("div.ltx_para, p.ltx_p")) {
            let t = clean(&p.text().collect::<String>());
            if t.is_empty() {
                continue;
            }
            if body.len() + t.len() > 12_000 {
                break;
            }
            body.push_str(&t);
            body.push_str("\n\n");
        }
        let body = body.trim().to_string();
        if !body.is_empty() {
            return Some(body);
        }
    }
    None
}

/// Parse a paper's HTML. Pure, so it can be tested without the network.
pub fn parse_html(html: &str) -> Extracted {
    let doc = Html::parse_document(html);
    let block = extract_author_block(&doc);
    Extracted {
        emails: extract_emails(&doc, &block, html),
        affiliations: extract_affiliations(&doc, &block),
        intro: extract_intro(&doc),
    }
}

/// Does an email plausibly belong to this author? Matches the surname, or
/// first-initial+surname, against the local part.
fn email_matches_author(email: &str, author_name: &str) -> bool {
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    let parts: Vec<String> = author_name
        .split_whitespace()
        .map(|p| {
            p.chars()
                .filter(|c| c.is_alphabetic())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|p| !p.is_empty())
        .collect();
    let Some(surname) = parts.last() else {
        return false;
    };
    if surname.len() < 3 {
        return false;
    }
    if local.contains(surname.as_str()) {
        return true;
    }
    if let Some(first) = parts.first() {
        if parts.len() > 1 {
            // The first character, not the first byte: slicing "øystein" at
            // byte 1 panicked and took the whole run down with it.
            let initial: String = first.chars().take(1).collect();
            if local == format!("{initial}{surname}") || local == format!("{initial}.{surname}") {
                return true;
            }
        }
    }
    false
}

/// Fold extraction results into a paper: attribute what we can, keep the rest
/// at the paper level rather than guessing.
pub fn apply(paper: &mut Paper, ex: Extracted) {
    // Start from a clean slate so re-running enrichment on an already-enriched
    // paper produces the same result rather than pushing every address into
    // `contact_emails` a second time (an author who already has an email
    // cannot re-claim it).
    for a in paper.authors.iter_mut() {
        a.email = None;
        a.affiliation = None;
    }
    paper.contact_emails.clear();
    paper.affiliations.clear();

    let mut unclaimed: Vec<String> = Vec::new();

    for email in &ex.emails {
        let mut claimed = false;
        for a in paper.authors.iter_mut() {
            if a.email.is_none() && email_matches_author(email, &a.name) {
                a.email = Some(email.clone());
                claimed = true;
                break;
            }
        }
        if !claimed {
            unclaimed.push(email.clone());
        }
    }

    // One email and one author is unambiguous even without a name match.
    if paper.authors.len() == 1 && paper.authors[0].email.is_none() && unclaimed.len() == 1 {
        paper.authors[0].email = Some(unclaimed.remove(0));
    }

    paper.contact_emails = unclaimed;
    paper.affiliations = ex.affiliations.into_iter().map(Affiliation::new).collect();
    paper.intro = ex.intro;
    paper.enriched = true;
}

/// Fetch and apply a paper's HTML. `Ok(false)` means there is no HTML
/// rendering for this paper, which is normal and not an error.
pub async fn enrich(http: &crate::http::Http, paper: &mut Paper) -> Result<bool> {
    let url = paper.html_url();
    match http.get_text(&url).await {
        Ok(html) => {
            apply(paper, parse_html(&html));
            Ok(true)
        }
        Err(e) if e.to_string().starts_with("404") => {
            tracing::debug!(id = %paper.id, "no HTML rendering");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Author names, for the geolocation step.
pub fn author_names(paper: &Paper) -> Vec<String> {
    paper
        .authors
        .iter()
        .map(|a: &Author| a.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<html><body>
<div class="ltx_authors">
  <span class="ltx_creator ltx_role_author">
    <span class="ltx_personname">Yuanzhe Peng, Jie Xu</span>
    <span class="ltx_author_notes">Affiliation:
University of North Florida, Jacksonville, FL
Technical University of Munich, Garching
</span>
  </span>
  <span>Contact: y.peng@unf.edu</span>
</div>
<section class="ltx_section" id="S1">
  <h2 class="ltx_title ltx_title_section">I Introduction</h2>
  <div class="ltx_para"><p class="ltx_p">Indoor radio map estimation matters.</p></div>
  <div class="ltx_para"><p class="ltx_p">We propose a method.</p></div>
</section>
<section class="ltx_section" id="S2">
  <h2 class="ltx_title ltx_title_section">II Related Work</h2>
  <div class="ltx_para"><p class="ltx_p">Should not appear.</p></div>
</section>
</body></html>"#;

    #[test]
    fn finds_the_email() {
        assert_eq!(parse_html(DOC).emails, vec!["y.peng@unf.edu"]);
    }

    #[test]
    fn finds_institutional_affiliations() {
        let a = parse_html(DOC).affiliations;
        assert!(a.iter().any(|x| x.contains("North Florida")), "{a:?}");
        assert!(
            a.iter()
                .any(|x| x.contains("Technical University of Munich")),
            "{a:?}"
        );
    }

    #[test]
    fn intro_stops_at_the_next_section() {
        let i = parse_html(DOC).intro.expect("intro found");
        assert!(i.contains("Indoor radio map estimation matters."));
        assert!(i.contains("We propose a method."));
        assert!(
            !i.contains("Should not appear"),
            "leaked into next section: {i}"
        );
    }

    #[test]
    fn roman_arabic_and_bare_headings_all_count_as_introduction() {
        for h in [
            "I Introduction",
            "1 Introduction",
            "Introduction",
            "1. INTRODUCTION",
        ] {
            assert!(intro_re().is_match(h), "{h} should match");
        }
        for h in ["Introduction to Topology", "II Related Work"] {
            assert!(!intro_re().is_match(h), "{h} should not match");
        }
    }

    #[test]
    fn email_attribution_uses_the_surname() {
        assert!(email_matches_author("y.peng@unf.edu", "Yuanzhe Peng"));
        assert!(email_matches_author("peng@unf.edu", "Yuanzhe Peng"));
        assert!(!email_matches_author("y.peng@unf.edu", "Jie Xu"));
    }

    #[test]
    fn a_first_name_that_starts_outside_ascii_is_not_cut_mid_letter() {
        // Regression: rerunning 27 October panicked on a first name starting
        // with "ø", sliced at byte 1.
        assert!(email_matches_author("ø.hansen@uio.no", "Øystein Hansen"));
        assert!(email_matches_author("ølund@uio.no", "Øystein Lund"));
        assert!(!email_matches_author("o.berg@uio.no", "Ågot Hansen"));
    }

    #[test]
    fn unattributable_emails_stay_at_paper_level() {
        let mut p = Paper {
            id: "x".into(),
            title: "t".into(),
            abstract_text: "a".into(),
            categories: vec![],
            announced: "2026-10-27".into(),
            authors: vec![Author::new("Yuanzhe Peng"), Author::new("Jie Xu")],
            affiliations: vec![],
            contact_emails: vec![],
            distance_km: None,
            geo_source: crate::model::GeoSource::None,
            intro: None,
            enriched: false,
        };
        apply(&mut p, parse_html(DOC));
        assert_eq!(p.authors[0].email.as_deref(), Some("y.peng@unf.edu"));
        assert!(
            p.authors[1].email.is_none(),
            "must not guess Jie Xu's address"
        );
        assert!(p.enriched);
    }

    #[test]
    fn strips_the_affiliation_label() {
        assert_eq!(
            normalise_affiliation("Affiliation: Delft University of Technology").as_deref(),
            Some("Delft University of Technology")
        );
    }

    #[test]
    fn parses_the_structured_acm_form() {
        // These reached the institution search verbatim and matched nothing.
        assert_eq!(
            normalise_affiliation(
                "organization=Northern Illinois University, city=DeKalb, postcode=60115, country=USA"
            )
            .as_deref(),
            Some("Northern Illinois University, DeKalb")
        );
        assert_eq!(
            normalise_affiliation("organization=Argonne National Laboratory").as_deref(),
            Some("Argonne National Laboratory")
        );
    }

    #[test]
    fn inline_emails_are_removed_from_affiliations() {
        let got = normalise_affiliation(
            "Email: vasil.g.vasilev.2022@elsys-bg.org Affiliation: Technological School Electronic Systems",
        );
        assert_eq!(
            got.as_deref(),
            Some("Technological School Electronic Systems")
        );
    }

    #[test]
    fn concatenated_duplicates_are_dropped() {
        let v = drop_concatenations(vec![
            "Delft University of Technology".to_string(),
            "Delft University of Technology Affiliation: Delft University".to_string(),
        ]);
        assert_eq!(v, vec!["Delft University of Technology"]);
    }

    // Both of these came out of a real parsing report as affiliations that
    // matched no institution, because the label was going into the search.
    const TALCA: &str =
        "Address: Instituto de Matemáticas, Universidad de Talca, Casilla 721, Talca, Chile";
    const SHUFE: &str = "thanks: School of Mathematics, Shanghai University of Finance and Economics, Shanghai 200433, People's Republic of China. E-mail: wujincao@shufe.edu.cn.";

    #[test]
    fn address_label_is_stripped() {
        assert_eq!(
            normalise_affiliation(TALCA).as_deref(),
            Some("Instituto de Matemáticas, Universidad de Talca, Casilla 721, Talca, Chile")
        );
    }

    #[test]
    fn thanks_label_and_trailing_email_are_stripped() {
        let got = normalise_affiliation(SHUFE).expect("should parse");
        assert!(got.starts_with("School of Mathematics"), "got {got}");
        assert!(!got.contains('@'), "address leaked into the name: {got}");
        assert!(!got.ends_with('.'), "trailing stop kept: {got}");
        assert!(got.ends_with("People's Republic of China"), "got {got}");
    }

    #[test]
    fn a_spaced_footnote_marker_is_stripped() {
        // From the parsing report: "1 Technical University of Munich, Munich,
        // Germany" went into the institution search with the marker attached.
        assert_eq!(
            normalise_affiliation("1 Technical University of Munich, Munich, Germany").as_deref(),
            Some("Technical University of Munich, Munich, Germany")
        );
        assert_eq!(
            strip_marker("2 Munich Center for Machine Learning"),
            "Munich Center for Machine Learning"
        );
    }

    #[test]
    fn a_note_label_is_stripped() {
        let got = normalise_affiliation(
            "Note: Department of Mathematics, Friedrich-Alexander-Universität Erlangen",
        );
        assert_eq!(
            got.as_deref(),
            Some("Department of Mathematics, Friedrich-Alexander-Universität Erlangen")
        );
    }

    #[test]
    fn acknowledgements_are_not_affiliations() {
        // "The authors thank the International Max Planck Research ..." was
        // being searched for verbatim as though it named an institution.
        assert!(normalise_affiliation(
            "The authors thank the International Max Planck Research School"
        )
        .is_none());
        assert!(
            normalise_affiliation("This work was supported by the Max Planck Institute").is_none()
        );
        // A real affiliation that merely contains a marker word still passes.
        assert!(normalise_affiliation("Max Planck Institute for Physics").is_some());
    }

    #[test]
    fn non_affiliation_text_is_rejected() {
        assert!(normalise_affiliation("Affiliation:").is_none());
        assert!(normalise_affiliation("Equal contribution").is_none());
    }

    #[test]
    fn enriching_twice_is_idempotent() {
        let mut p = Paper {
            id: "x".into(),
            title: "t".into(),
            abstract_text: "a".into(),
            categories: vec![],
            announced: "2026-10-27".into(),
            authors: vec![Author::new("Yuanzhe Peng"), Author::new("Jie Xu")],
            affiliations: vec![],
            contact_emails: vec![],
            distance_km: None,
            geo_source: crate::model::GeoSource::None,
            intro: None,
            enriched: false,
        };
        apply(&mut p, parse_html(DOC));
        let first = (
            p.authors[0].email.clone(),
            p.contact_emails.clone(),
            p.affiliations.len(),
        );
        apply(&mut p, parse_html(DOC));
        let second = (
            p.authors[0].email.clone(),
            p.contact_emails.clone(),
            p.affiliations.len(),
        );
        assert_eq!(first, second, "re-enrichment must not accumulate");
        assert!(
            p.contact_emails.is_empty(),
            "claimed address leaked: {:?}",
            p.contact_emails
        );
    }

    #[test]
    fn footnote_markers_are_stripped_from_affiliations() {
        assert_eq!(
            normalise_affiliation("1University of North Florida, Jacksonville, FL, USA").as_deref(),
            Some("University of North Florida, Jacksonville, FL, USA")
        );
    }

    #[test]
    fn marker_behind_the_label_is_also_stripped() {
        // Regression: arXiv:2610.31702's author block is
        // "Affiliation:\u{a0}1University of North Florida, ...". Stripping the
        // marker before the label left the "1" glued to the name, and the
        // institution lookup then matched nothing.
        assert_eq!(
            normalise_affiliation(
                "Affiliation:\u{a0}1University of North Florida, Jacksonville, FL, USA"
            )
            .as_deref(),
            Some("University of North Florida, Jacksonville, FL, USA")
        );
    }

    #[test]
    fn a_numeric_company_name_survives_marker_stripping() {
        assert_eq!(strip_marker("3M Company"), "3M Company");
        assert_eq!(
            strip_marker("2University of Munich"),
            "University of Munich"
        );
    }

    #[test]
    fn emails_outside_the_author_block_are_still_found() {
        // LaTeXML often puts the contact address in a title footnote rather
        // than inside div.ltx_authors.
        let doc = r#"<html><body>
          <div class="ltx_authors"><span class="ltx_personname">A Person</span></div>
          <div class="ltx_note_content">Correspondence: a.person@uni-muenchen.de</div>
          <section class="ltx_section"><h2 class="ltx_title ltx_title_section">1 Introduction</h2>
            <div class="ltx_para"><p class="ltx_p">Cites someone@elsewhere.edu heavily.</p></div>
          </section>
        </body></html>"#;
        let ex = parse_html(doc);
        assert_eq!(ex.emails, vec!["a.person@uni-muenchen.de"]);
    }

    #[test]
    fn addresses_in_the_body_are_not_harvested() {
        let doc = r#"<html><body>
          <div class="ltx_authors"><span class="ltx_personname">A Person</span></div>
          <section class="ltx_section"><h2 class="ltx_title ltx_title_section">1 Introduction</h2>
            <div class="ltx_para"><p class="ltx_p">Contact notmine@elsewhere.edu for data.</p></div>
          </section>
        </body></html>"#;
        assert!(
            parse_html(doc).emails.is_empty(),
            "must not scrape the body"
        );
    }

    #[test]
    fn street_addresses_are_not_mistaken_for_institutions() {
        assert!(!looks_institutional("123 Main St, Apt 4"));
        assert!(looks_institutional("Technical University of Munich"));
        assert!(!looks_institutional("short"));
    }

    #[test]
    fn empty_html_is_not_a_panic() {
        let ex = parse_html("");
        assert!(ex.emails.is_empty() && ex.intro.is_none());
    }
}
