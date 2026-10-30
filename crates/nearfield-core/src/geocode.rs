//! Turning a place name into an origin.
//!
//! Uses Nominatim (OpenStreetMap). Their usage policy asks for an identifying
//! User-Agent and at most one request a second — both handled by the shared
//! [`Http`], which is also why this lives in Rust rather than the webview:
//! Nominatim sends no permissive CORS headers.

use crate::geo::Point;
use crate::http::Http;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const NOMINATIM: &str = "https://nominatim.openstreetmap.org/search";

/// One candidate place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Place {
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    /// What kind of thing this is: city, university,町 — straight from OSM.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
}

impl Place {
    pub fn point(&self) -> Point {
        Point {
            lat: self.lat,
            lon: self.lon,
        }
    }

    /// A short name for the UI: Nominatim's display names are long.
    pub fn short_label(&self) -> String {
        let parts: Vec<&str> = self.label.split(',').map(str::trim).collect();
        match (parts.first(), parts.last()) {
            (Some(first), Some(last)) if parts.len() > 1 && first != last => {
                format!("{first}, {last}")
            }
            (Some(first), _) => (*first).to_string(),
            _ => self.label.clone(),
        }
    }
}

#[derive(Deserialize)]
struct Hit {
    lat: String,
    lon: String,
    #[serde(default)]
    display_name: String,
    #[serde(default, rename = "type")]
    kind: String,
}

/// Look a place up by name. An empty result is not an error.
pub async fn search(http: &Http, query: &str, limit: usize) -> Result<Vec<Place>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    // A coordinate pair is not worth a network round trip.
    if let Some(p) = Point::parse(query) {
        return Ok(vec![Place {
            label: format!("{:.4}, {:.4}", p.lat, p.lon),
            lat: p.lat,
            lon: p.lon,
            kind: "coordinates".into(),
        }]);
    }

    let url = reqwest::Url::parse_with_params(
        NOMINATIM,
        &[
            ("q", query),
            ("format", "jsonv2"),
            ("limit", &limit.to_string()),
        ],
    )?;
    let hits: Vec<Hit> = http
        .get_json(url.as_str())
        .await
        .with_context(|| format!("looking up {query:?}"))?;

    Ok(hits
        .into_iter()
        .filter_map(|h| {
            let lat: f64 = h.lat.parse().ok()?;
            let lon: f64 = h.lon.parse().ok()?;
            let p = Point { lat, lon };
            p.is_valid().then_some(Place {
                label: h.display_name,
                lat,
                lon,
                kind: h.kind,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(label: &str) -> Place {
        Place {
            label: label.into(),
            lat: 0.0,
            lon: 0.0,
            kind: String::new(),
        }
    }

    #[test]
    fn short_label_keeps_the_place_and_the_country() {
        let p = place("Garching bei München, Landkreis München, Bayern, 85748, Deutschland");
        assert_eq!(p.short_label(), "Garching bei München, Deutschland");
    }

    #[test]
    fn short_label_survives_a_single_part_name() {
        assert_eq!(place("Tokyo").short_label(), "Tokyo");
    }

    #[tokio::test]
    async fn an_empty_query_does_not_hit_the_network() {
        let http = Http::new("test@example.invalid").unwrap();
        assert!(search(&http, "   ", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_coordinate_pair_resolves_without_the_network() {
        let http = Http::new("test@example.invalid").unwrap();
        let hits = search(&http, "35.6762, 139.6503", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!((hits[0].lat - 35.6762).abs() < 1e-9);
        assert_eq!(hits[0].kind, "coordinates");
    }
}
