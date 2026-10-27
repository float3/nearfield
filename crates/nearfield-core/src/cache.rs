//! Disk-backed JSON caches.
//!
//! These are committed to the repo on purpose: the author->institution map is
//! what lets a GitHub Actions run geolocate same-day papers without
//! re-querying OpenAlex for authors it has already seen. It warms up over time.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct JsonCache<V> {
    path: PathBuf,
    map: HashMap<String, V>,
    dirty: bool,
}

impl<V: Serialize + DeserializeOwned + Clone> JsonCache<V> {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let map = if path.exists() {
            let s = std::fs::read_to_string(&path)
                .with_context(|| format!("reading cache {}", path.display()))?;
            if s.trim().is_empty() {
                HashMap::new()
            } else {
                serde_json::from_str(&s)
                    .with_context(|| format!("parsing cache {}", path.display()))?
            }
        } else {
            HashMap::new()
        };
        Ok(Self { path, map, dirty: false })
    }

    pub fn get(&self, k: &str) -> Option<&V> {
        self.map.get(k)
    }

    pub fn contains(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }

    pub fn put(&mut self, k: impl Into<String>, v: V) {
        self.map.insert(k.into(), v);
        self.dirty = true;
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Sorted output so commits show real changes, not reordering noise.
        let sorted: std::collections::BTreeMap<_, _> = self.map.iter().collect();
        let s = serde_json::to_string_pretty(&sorted)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, s).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("nf-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.json");

        let mut c: JsonCache<String> = JsonCache::load(&p).unwrap();
        assert!(c.is_empty());
        c.put("a", "alpha".to_string());
        c.save().unwrap();

        let c2: JsonCache<String> = JsonCache::load(&p).unwrap();
        assert_eq!(c2.get("a").map(String::as_str), Some("alpha"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_empty_cache() {
        let c: JsonCache<String> =
            JsonCache::load("definitely/not/here/nope.json").unwrap();
        assert!(c.is_empty());
    }
}
