//! Distance arithmetic. The home point is Munich; everything is ranked by how
//! far an author's institution sits from it.

use serde::{Deserialize, Serialize};

pub const MUNICH: Point = Point { lat: 48.1372, lon: 11.5756 };
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

    pub fn from_munich_km(&self) -> f64 {
        self.distance_km(&MUNICH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn munich_to_itself_is_zero() {
        assert!(MUNICH.from_munich_km() < 0.001);
    }

    #[test]
    fn tum_is_walking_distance() {
        // Technical University of Munich, per ROR 02kkvpp62.
        let tum = Point { lat: 48.13743, lon: 11.57549 };
        assert!(tum.from_munich_km() < 2.0, "got {}", tum.from_munich_km());
    }

    #[test]
    fn known_long_haul() {
        // Munich -> MIT (Cambridge, MA), great-circle ~6184 km. Cross-checked
        // against London->NYC (5570 km) and Munich->Tokyo (9369 km), both of
        // which this formula reproduces to within a few km of published values.
        let mit = Point { lat: 42.3601, lon: -71.0942 };
        let d = mit.from_munich_km();
        assert!((d - 6184.0).abs() < 20.0, "got {d}");
    }

    #[test]
    fn matches_published_distances() {
        let london = Point { lat: 51.5074, lon: -0.1278 };
        let nyc = Point { lat: 40.7128, lon: -74.0060 };
        assert!((london.distance_km(&nyc) - 5570.0).abs() < 25.0);

        let tokyo = Point { lat: 35.6762, lon: 139.6503 };
        assert!((tokyo.from_munich_km() - 9369.0).abs() < 40.0);
    }

    #[test]
    fn antipodal_sanity() {
        let nz = Point { lat: -48.1372, lon: -168.4244 };
        assert!(nz.from_munich_km() > 19_900.0);
    }
}
