//! What the night's ingestion could not parse.
//!
//! Extraction from arXiv's HTML is heuristic — LaTeXML's output varies by
//! template, and every publisher's author block is laid out differently. The
//! only way to improve it is to see what actually failed, so each run records
//! the *raw strings* it could not make sense of rather than just a count.
//! Tomorrow's fix comes from reading yesterday's samples.
//!
//! Samples are capped: a systematic failure shows up in the first handful, and
//! an uncapped list would bloat every digest the app downloads.

use serde::{Deserialize, Serialize};

/// How many example strings to keep per issue.
pub const SAMPLE_CAP: usize = 25;

/// One class of failure, with enough detail to act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Stable machine-readable key, e.g. `affiliation_unresolved`.
    pub kind: String,
    /// What went wrong, in a sentence.
    pub what: String,
    /// What a fix would look like. Empty when it is not actionable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
    pub count: usize,
    /// Up to `SAMPLE_CAP` examples.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
    /// Whether this is worth acting on, or just how the world is.
    pub actionable: bool,
}

/// A counter plus capped samples.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tally {
    pub count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
}

impl Tally {
    pub fn record(&mut self, sample: impl Into<String>) {
        self.count += 1;
        if self.samples.len() < SAMPLE_CAP {
            let s = sample.into();
            if !self.samples.contains(&s) {
                self.samples.push(s);
            }
        }
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Everything a run noticed going wrong.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Diagnostics {
    pub feeds_attempted: usize,
    /// Category feeds that did not return. Papers in them are simply missing.
    #[serde(default)]
    pub feeds_failed: Tally,
    /// Items whose arXiv id could not be read — these are dropped entirely.
    #[serde(default)]
    pub items_unidentified: Tally,
    /// Papers where the feed listed no authors.
    #[serde(default)]
    pub papers_without_authors: Tally,

    pub enrich_attempted: usize,
    /// No HTML rendering exists (non-LaTeX submission). Expected, not a bug.
    #[serde(default)]
    pub html_absent: Tally,
    /// The HTML fetch itself failed.
    #[serde(default)]
    pub html_errors: Tally,
    /// HTML parsed but no introduction section was recognised.
    #[serde(default)]
    pub intro_not_found: Tally,
    /// HTML parsed but no contact address was found anywhere in the front matter.
    #[serde(default)]
    pub emails_not_found: Tally,
    /// Affiliation strings that were searched for and matched no institution
    /// — the richest signal for improving `normalise_affiliation`.
    #[serde(default)]
    pub affiliations_unresolved: Tally,
    /// Affiliations never looked up because the allowance ran out. Counting
    /// these as parse failures overstated them: "Technical University of
    /// Munich, Germany" resolves perfectly well when there is budget to ask.
    #[serde(default)]
    pub affiliations_unchecked: Tally,
    /// Institutions that resolved but carry no coordinates upstream.
    #[serde(default)]
    pub institutions_without_geo: Tally,
    /// Authors OpenAlex had never heard of.
    #[serde(default)]
    pub authors_unknown: Tally,

    /// The run stopped looking things up before finishing.
    pub lookup_budget_hit: bool,
    /// What the run spent at OpenAlex, by their own accounting.
    #[serde(default)]
    pub spent_usd: f64,
    #[serde(default)]
    pub lookups: usize,
    /// Authors placed from the offline snapshot index, at no cost.
    #[serde(default)]
    pub authors_from_snapshot: usize,
    /// Which snapshot release the index was built from, when one was loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_release: Option<String>,
    /// OpenAlex failed repeatedly and the breaker tripped.
    pub openalex_degraded: bool,
}

impl Diagnostics {
    /// Render as a list of issues, worst first, for the app and the CLI.
    pub fn issues(&self) -> Vec<Issue> {
        let mut out = Vec::new();

        let mut add = |kind: &str, what: String, fix: &str, t: &Tally, actionable: bool| {
            if t.count > 0 {
                out.push(Issue {
                    kind: kind.to_string(),
                    what,
                    fix: fix.to_string(),
                    count: t.count,
                    samples: t.samples.clone(),
                    actionable,
                });
            }
        };

        add(
            "affiliation_unresolved",
            "Affiliation text was read off the paper but matched no institution"
                .to_string(),
            "Look at the samples: if they share a shape, teach normalise_affiliation \
             to strip it. These papers lose their authoritative location and fall \
             back to the author prior.",
            &self.affiliations_unresolved,
            true,
        );
        add(
            "affiliation_unchecked",
            "Affiliation was read off the paper but never looked up".to_string(),
            "The allowance ran out before these. Not a parsing problem — they              are placed by a later run, or for free once the offline author              index is built.",
            &self.affiliations_unchecked,
            false,
        );
        add(
            "intro_not_found",
            "The paper had HTML but no section was recognised as the introduction"
                .to_string(),
            "Check the heading these papers use; intro_re may need another form \
             (e.g. 'Background', 'Motivation', or an unnumbered heading).",
            &self.intro_not_found,
            true,
        );
        add(
            "emails_not_found",
            "The paper had HTML but no contact address was found".to_string(),
            "Some papers genuinely print no address. If the samples do show one \
             on arXiv, the front-matter cut is landing in the wrong place.",
            &self.emails_not_found,
            true,
        );
        add(
            "papers_without_authors",
            "The feed entry carried no author names".to_string(),
            "dc:creator was missing or unparseable; these papers cannot be placed \
             at all.",
            &self.papers_without_authors,
            true,
        );
        add(
            "items_unidentified",
            "A feed item had no readable arXiv id and was dropped".to_string(),
            "Check the feed's guid/description format — this silently loses papers.",
            &self.items_unidentified,
            true,
        );
        add(
            "feed_failed",
            "A category feed could not be fetched".to_string(),
            "Usually transient. If one category fails every night, its feed name \
             may have changed.",
            &self.feeds_failed,
            true,
        );
        add(
            "institution_without_geo",
            "An institution resolved but has no coordinates in OpenAlex".to_string(),
            "Upstream data gap. Could be filled from ROR, which sometimes has \
             coordinates where OpenAlex does not.",
            &self.institutions_without_geo,
            true,
        );
        add(
            "html_error",
            "Fetching the paper's HTML failed".to_string(),
            "Usually transient rate limiting.",
            &self.html_errors,
            true,
        );
        add(
            "author_unknown",
            "OpenAlex had no record of this author".to_string(),
            "Expected for first-time authors and students; nothing to fix.",
            &self.authors_unknown,
            false,
        );
        add(
            "html_absent",
            "arXiv has no HTML rendering of the paper".to_string(),
            "Expected for non-LaTeX submissions; only the PDF exists.",
            &self.html_absent,
            false,
        );

        out.sort_by(|a, b| b.actionable.cmp(&a.actionable).then(b.count.cmp(&a.count)));
        out
    }

    /// Does anything here deserve attention today?
    pub fn needs_attention(&self) -> bool {
        self.openalex_degraded
            || self.feeds_failed.count > 0
            || self.items_unidentified.count > 0
            || self.affiliations_unresolved.count > 0
    }

    /// A short human summary for the console and the push notification.
    pub fn summary(&self) -> String {
        let mut bits: Vec<String> = Vec::new();
        if self.feeds_failed.count > 0 {
            bits.push(format!("{} feeds failed", self.feeds_failed.count));
        }
        if self.affiliations_unresolved.count > 0 {
            bits.push(format!(
                "{} affiliations unresolved",
                self.affiliations_unresolved.count
            ));
        }
        if self.intro_not_found.count > 0 {
            bits.push(format!("{} without an intro", self.intro_not_found.count));
        }
        if self.emails_not_found.count > 0 {
            bits.push(format!("{} without an email", self.emails_not_found.count));
        }
        if self.openalex_degraded {
            bits.push("OpenAlex throttled".to_string());
        }
        if self.lookup_budget_hit {
            bits.push(format!("budget spent (${:.2})", self.spent_usd));
        }
        if bits.is_empty() {
            "nothing failed to parse".to_string()
        } else {
            bits.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tally_caps_samples_but_keeps_counting() {
        let mut t = Tally::default();
        for i in 0..100 {
            t.record(format!("sample {i}"));
        }
        assert_eq!(t.count, 100);
        assert_eq!(t.samples.len(), SAMPLE_CAP);
    }

    #[test]
    fn tally_does_not_repeat_identical_samples() {
        let mut t = Tally::default();
        t.record("same");
        t.record("same");
        assert_eq!(t.count, 2, "both occurrences still count");
        assert_eq!(t.samples.len(), 1, "but only one example is kept");
    }

    #[test]
    fn actionable_issues_sort_above_expected_ones() {
        let mut d = Diagnostics::default();
        for _ in 0..500 {
            d.html_absent.bump();
        }
        d.affiliations_unresolved.record("1Some Place");

        let issues = d.issues();
        assert_eq!(
            issues[0].kind, "affiliation_unresolved",
            "a fixable issue outranks a large expected one"
        );
        assert!(!issues.iter().find(|i| i.kind == "html_absent").unwrap().actionable);
    }

    #[test]
    fn a_clean_run_reports_nothing() {
        let d = Diagnostics::default();
        assert!(d.issues().is_empty());
        assert!(!d.needs_attention());
        assert_eq!(d.summary(), "nothing failed to parse");
    }

    #[test]
    fn summary_names_what_went_wrong() {
        let mut d = Diagnostics::default();
        d.affiliations_unresolved.record("x");
        d.openalex_degraded = true;
        let s = d.summary();
        assert!(s.contains("1 affiliations unresolved"), "{s}");
        assert!(s.contains("OpenAlex throttled"), "{s}");
    }
}
