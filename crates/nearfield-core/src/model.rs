//! The shapes the pipeline produces and the app consumes.

use serde::{Deserialize, Serialize};

/// Where a location came from. Surfaced in the UI so a ranking built on a
/// guess is never mistaken for one read off the paper itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoSource {
    /// Affiliation parsed from the paper's own HTML. Authoritative.
    PaperHtml,
    /// The author's last known institution in OpenAlex. Approximate: they may
    /// have moved, and name collisions are possible.
    AuthorPrior,
    /// OpenAlex has indexed the work itself and lists the affiliation.
    OpenalexWork,
    /// Could not be placed.
    None,
}

impl GeoSource {
    /// Rough trust ordering, used when two sources disagree.
    pub fn rank(self) -> u8 {
        match self {
            GeoSource::PaperHtml => 3,
            GeoSource::OpenalexWork => 2,
            GeoSource::AuthorPrior => 1,
            GeoSource::None => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Raw affiliation string as printed on the paper, when we have it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ror: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
    pub geo_source: GeoSource,
}

impl Author {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: None,
            affiliation: None,
            institution: None,
            ror: None,
            country: None,
            city: None,
            lat: None,
            lon: None,
            distance_km: None,
            geo_source: GeoSource::None,
        }
    }

    pub fn located(&self) -> bool {
        self.lat.is_some() && self.lon.is_some()
    }
}

/// An affiliation as printed on the paper, once geocoded.
///
/// LaTeXML output varies too much to reliably tie every affiliation to a
/// specific author, so affiliations we cannot attribute are kept at the paper
/// level rather than guessed onto a person.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Affiliation {
    pub raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ror: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
}

impl Affiliation {
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            institution: None,
            ror: None,
            country: None,
            city: None,
            lat: None,
            lon: None,
            distance_km: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    /// Bare arXiv id, e.g. "2610.31702".
    pub id: String,
    pub title: String,
    #[serde(rename = "abstract")]
    pub abstract_text: String,
    pub categories: Vec<String>,
    /// Date the paper was announced, as YYYY-MM-DD.
    pub announced: String,
    pub authors: Vec<Author>,
    /// Affiliations read off the paper that we could not tie to one author.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affiliations: Vec<Affiliation>,
    /// Emails found on the paper that we could not tie to one author. The
    /// corresponding author is usually in here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contact_emails: Vec<String>,
    /// Distance of the *closest* author to the digest's origin.
    /// `None` if nobody was placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
    pub geo_source: GeoSource,
    /// Introduction section, only present for prefetched papers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intro: Option<String>,
    pub enriched: bool,
}

impl Paper {
    pub fn abs_url(&self) -> String {
        format!("https://arxiv.org/abs/{}", self.id)
    }
    pub fn pdf_url(&self) -> String {
        format!("https://arxiv.org/pdf/{}", self.id)
    }
    pub fn html_url(&self) -> String {
        format!("https://arxiv.org/html/{}", self.id)
    }

    /// Recompute the paper-level distance: the closest placed author or
    /// unattributed affiliation. A paper is ranked by its nearest person,
    /// since the question is whether anyone on it is reachable from here.
    pub fn resolve_distance(&mut self) {
        let mut best: Option<(f64, GeoSource)> = None;
        for a in &self.authors {
            if let Some(d) = a.distance_km {
                match best {
                    Some((bd, _)) if bd <= d => {}
                    _ => best = Some((d, a.geo_source)),
                }
            }
        }
        // Affiliations come off the paper itself, so they are PaperHtml-grade.
        for aff in &self.affiliations {
            if let Some(d) = aff.distance_km {
                match best {
                    Some((bd, _)) if bd <= d => {}
                    _ => best = Some((d, GeoSource::PaperHtml)),
                }
            }
        }
        match best {
            Some((d, src)) => {
                self.distance_km = Some(d);
                self.geo_source = src;
            }
            None => {
                self.distance_km = None;
                self.geo_source = GeoSource::None;
            }
        }
    }

    /// Recompute every distance against a different origin.
    ///
    /// Coordinates travel with each author and affiliation, so moving the
    /// origin is pure arithmetic — no re-harvest, and it works offline on a
    /// digest already on the phone.
    pub fn reproject(&mut self, origin: &crate::geo::Point) {
        for a in self.authors.iter_mut() {
            a.distance_km = match (a.lat, a.lon) {
                (Some(lat), Some(lon)) => {
                    Some(crate::geo::Point { lat, lon }.distance_km(origin))
                }
                _ => None,
            };
        }
        for f in self.affiliations.iter_mut() {
            f.distance_km = match (f.lat, f.lon) {
                (Some(lat), Some(lon)) => {
                    Some(crate::geo::Point { lat, lon }.distance_km(origin))
                }
                _ => None,
            };
        }
        self.resolve_distance();
    }

    /// Every address we found, author-attributed first, deduped.
    pub fn emails(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for e in self
            .authors
            .iter()
            .filter_map(|a| a.email.as_deref())
            .chain(self.contact_emails.iter().map(String::as_str))
        {
            if !out.contains(&e) {
                out.push(e);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Home {
    pub lat: f64,
    pub lon: f64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Digest {
    pub date: String,
    pub generated_at: String,
    pub home: Home,
    pub stats: Stats,
    /// What this run could not parse. Travels with the digest so the app can
    /// show it without a second request.
    #[serde(default)]
    pub diagnostics: crate::diagnostics::Diagnostics,
    pub papers: Vec<Paper>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub harvested: usize,
    pub located: usize,
    pub unlocated: usize,
    pub enriched: usize,
    pub with_email: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper_with(authors: Vec<Author>) -> Paper {
        Paper {
            id: "2610.00001".into(),
            title: "t".into(),
            abstract_text: "a".into(),
            categories: vec!["cs.LG".into()],
            announced: "2026-10-27".into(),
            authors,
            affiliations: Vec::new(),
            contact_emails: Vec::new(),
            distance_km: None,
            geo_source: GeoSource::None,
            intro: None,
            enriched: false,
        }
    }

    #[test]
    fn distance_is_the_nearest_author() {
        let mut far = Author::new("Far");
        far.distance_km = Some(6000.0);
        far.geo_source = GeoSource::AuthorPrior;
        let mut near = Author::new("Near");
        near.distance_km = Some(4.0);
        near.geo_source = GeoSource::PaperHtml;

        let mut p = paper_with(vec![far, near]);
        p.resolve_distance();
        assert_eq!(p.distance_km, Some(4.0));
        assert_eq!(p.geo_source, GeoSource::PaperHtml);
    }

    #[test]
    fn a_tum_mit_collaboration_ranks_as_tum() {
        // One nearby author is enough: a paper with a TUM author and an MIT
        // author is a paper you could walk to a seminar about.
        let mut tum = Author::new("Someone at TUM");
        tum.lat = Some(48.13743);
        tum.lon = Some(11.57549);
        tum.distance_km =
            Some(crate::geo::Point { lat: 48.13743, lon: 11.57549 }.distance_km(&crate::geo::MUNICH));
        tum.geo_source = GeoSource::AuthorPrior;

        let mut mit = Author::new("Someone at MIT");
        mit.lat = Some(42.3601);
        mit.lon = Some(-71.0942);
        mit.distance_km =
            Some(crate::geo::Point { lat: 42.3601, lon: -71.0942 }.distance_km(&crate::geo::MUNICH));
        mit.geo_source = GeoSource::AuthorPrior;

        // Author order must not matter.
        for authors in [vec![mit.clone(), tum.clone()], vec![tum.clone(), mit.clone()]] {
            let mut p = paper_with(authors);
            p.resolve_distance();
            assert!(
                p.distance_km.unwrap() < 2.0,
                "should rank as TUM-near, got {:?}",
                p.distance_km
            );
        }
    }

    #[test]
    fn unplaced_authors_leave_paper_unlocated() {
        let mut p = paper_with(vec![Author::new("Nobody")]);
        p.resolve_distance();
        assert!(p.distance_km.is_none());
        assert_eq!(p.geo_source, GeoSource::None);
    }

    #[test]
    fn paper_html_outranks_a_prior() {
        assert!(GeoSource::PaperHtml.rank() > GeoSource::AuthorPrior.rank());
    }
}
