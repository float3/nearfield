//! Distance arithmetic.
//!
//! The origin is a parameter, not a constant: the same digest is read from
//! home in Munich and from wherever you happen to be, and a trip is worth
//! preparing for in advance. Munich is only the default.

use serde::{Deserialize, Serialize};

/// The default origin.
pub const MUNICH: Point = Point { lat: 48.1372, lon: 11.5756 };
pub const MUNICH_LABEL: &str = "Munich";

const EARTH_RADIUS_KM: f64 = 6371.0088;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

impl Point {
    /// Great-circle distance in kilometres.
    pub fn distance_km(&self, other: &Point) -> f64 {
        let (lat1, lat2) = (self.lat.to_radians(), other.lat.to_radians());
        let dlat = lat2 - lat1;
        let dlon = (other.lon - self.lon).to_radians();
        let a = (dlat / 2.0).sin().powi(2)
            + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
    }

    /// Reject coordinates that cannot be real, so a bad `--origin` or a
    /// confused GPS fix fails loudly instead of silently ranking nonsense.
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }

    /// Parse a `"lat,lon"` pair.
    pub fn parse(s: &str) -> Option<Point> {
        let (a, b) = s.split_once(',')?;
        let p = Point { lat: a.trim().parse().ok()?, lon: b.trim().parse().ok()? };
        p.is_valid().then_some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_is_zero_from_itself() {
        assert!(MUNICH.distance_km(&MUNICH) < 0.001);
    }

    #[test]
    fn tum_is_walking_distance_from_munich() {
        // Technical University of Munich, per ROR 02kkvpp62.
        let tum = Point { lat: 48.13743, lon: 11.57549 };
        assert!(tum.distance_km(&MUNICH) < 2.0, "got {}", tum.distance_km(&MUNICH));
    }

    #[test]
    fn matches_published_distances() {
        // Cross-checks of the formula against known great-circle distances.
        let mit = Point { lat: 42.3601, lon: -71.0942 };
        assert!((mit.distance_km(&MUNICH) - 6184.0).abs() < 20.0);

        let london = Point { lat: 51.5074, lon: -0.1278 };
        let nyc = Point { lat: 40.7128, lon: -74.0060 };
        assert!((london.distance_km(&nyc) - 5570.0).abs() < 25.0);

        let tokyo = Point { lat: 35.6762, lon: 139.6503 };
        assert!((tokyo.distance_km(&MUNICH) - 9369.0).abs() < 40.0);
    }

    #[test]
    fn distance_is_symmetric() {
        let a = Point { lat: 35.6762, lon: 139.6503 };
        let b = Point { lat: -33.8688, lon: 151.2093 };
        assert!((a.distance_km(&b) - b.distance_km(&a)).abs() < 1e-9);
    }

    #[test]
    fn the_origin_actually_changes_the_answer() {
        // The whole point of making this a parameter: MIT is far from Munich
        // and next door to Boston.
        let mit = Point { lat: 42.3601, lon: -71.0942 };
        let boston = Point { lat: 42.3601, lon: -71.0589 };
        assert!(mit.distance_km(&MUNICH) > 6000.0);
        assert!(mit.distance_km(&boston) < 5.0);
    }

    #[test]
    fn antipodal_sanity() {
        let nz = Point { lat: -48.1372, lon: -168.4244 };
        assert!(nz.distance_km(&MUNICH) > 19_900.0);
    }

    #[test]
    fn parses_a_coordinate_pair() {
        assert_eq!(Point::parse("48.1372, 11.5756"), Some(MUNICH));
        assert_eq!(Point::parse("35.6762,139.6503").map(|p| p.lat), Some(35.6762));
    }

    #[test]
    fn rejects_impossible_coordinates() {
        assert!(Point::parse("91,0").is_none(), "latitude past the pole");
        assert!(Point::parse("0,181").is_none(), "longitude past the date line");
        assert!(Point::parse("not,a,point").is_none());
        assert!(Point::parse("48.1372").is_none(), "needs both halves");
        assert!(!Point { lat: f64::NAN, lon: 0.0 }.is_valid());
    }
}
