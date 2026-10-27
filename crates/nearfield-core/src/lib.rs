//! Nearfield: rank each day's preprints by how far their authors are from home.

pub mod arxiv;
pub mod cache;
pub mod diagnostics;
pub mod geo;
pub mod geocode;
pub mod http;
pub mod model;
pub mod openalex;
pub mod paper;
pub mod pipeline;
pub mod version;

pub use geo::{Point, MUNICH, MUNICH_LABEL};
pub use geocode::Place;
pub use diagnostics::Diagnostics;
pub use model::{Affiliation, Author, Digest, GeoSource, Home, Paper, Stats};
