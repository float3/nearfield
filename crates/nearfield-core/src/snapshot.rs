//! The offline author index, built from the OpenAlex snapshot.
//!
//! Why it exists: placing an author through the API costs $0.001 per fuzzy
//! search, a real arXiv day has ~10,000 distinct names, and a free key allows
//! $1 — so the API alone can never cover a day. The same data is published as
//! a free, CC0 snapshot on S3. The authors entity is 101 million rows and
//! 50 GB of parquet, far too much to download whole, but the five columns we
//! need are 3–5 % of each file and parquet lets them be fetched on their own
//! (see `snapshot_fetch`). This module takes those extracts and turns them
//! into a map from folded name to institution that is consulted *before* the
//! API, so paid lookups become the exception.
//!
//! The file format is deliberately dumb: a sorted array of 64-bit name
//! hashes, a parallel array of set ids, a table of institution-id sets, and
//! the institution records as JSON at the tail. Loading is one read and a
//! lookup is one binary search, no dependencies, and the whole thing is
//! rebuilt from the extracts rather than updated in place. It is not committed
//! — at tens of millions of keys it is hundreds of megabytes — so it lives
//! under `cache/snapshot/`, gitignored.
//!
//! Staleness is fine. Releases are quarterly, and people change institution
//! on a scale of years; the API path stays as the fallback for whoever the
//! snapshot has not heard of.

use crate::names;
use crate::openalex::InstitutionRecord;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Where `build` writes the index, relative to the snapshot directory.
pub const INDEX_FILE: &str = "authors.idx";
pub const INSTITUTIONS_EXTRACT: &str = "institutions.jsonl.gz";
pub const AUTHORS_DIR: &str = "authors";
pub const MANIFEST_FILE: &str = "manifest.json";

const MAGIC: &[u8; 4] = b"NFAI";
const VERSION: u32 = 1;
/// How many of an author's institutions an entry keeps. Matches the four
/// `place_author` already considers; the rest are historic appointments.
const SET_WIDTH: usize = 4;
const NO_INSTITUTION: u32 = u32::MAX;

/// One author row as pulled out of the snapshot: only what the index needs.
/// Short field names because there are a hundred million of these on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorExtract {
    #[serde(rename = "i")]
    pub id: u64,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "a", default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(rename = "w", default)]
    pub works: u32,
    /// OpenAlex institution ids, numeric, most recent first.
    #[serde(rename = "l")]
    pub institutions: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstitutionExtract {
    pub id: u64,
    #[serde(flatten)]
    pub record: InstitutionRecord,
}

/// What goes into the index, and what is left out.
#[derive(Debug, Clone)]
pub struct BuildPolicy {
    /// Authors with fewer works than this are dropped. The long tail of
    /// single-work authors is a third of the snapshot and almost never the
    /// person on today's preprint; and when a name collides, the more
    /// prolific author wins anyway.
    pub min_works: u32,
    /// File each author under `display_name_alternatives` too. That is what
    /// catches "R. Gaur" against "Rahul Gaur", at the price of more keys.
    pub alternatives: bool,
}

impl Default for BuildPolicy {
    fn default() -> Self {
        Self {
            min_works: 2,
            alternatives: true,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct BuildReport {
    pub release: String,
    pub authors_seen: u64,
    pub authors_kept: u64,
    pub keys: u64,
    pub sets: u64,
    pub institutions: u64,
    pub bytes: u64,
}

/// FNV-1a. Stable across Rust versions, which `DefaultHasher` is not, and
/// at 64 bits a collision among 10^8 keys is a once-in-a-build event whose
/// cost is one wrong prior.
pub fn hash_key(key: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `https://openalex.org/I123` or `I123` → 123.
pub fn numeric_id(s: &str) -> Option<u64> {
    let tail = s.rsplit('/').next()?;
    let digits = tail.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    digits.parse().ok()
}

pub fn openalex_institution_id(n: u64) -> String {
    format!("https://openalex.org/I{n}")
}

fn open_gz(path: &Path) -> Result<BufReader<flate2::read::GzDecoder<std::fs::File>>> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(BufReader::new(flate2::read::GzDecoder::new(f)))
}

/// Append-only gzip JSONL writer that only becomes visible once complete, so
/// an interrupted extract never masquerades as a finished one.
pub struct ExtractWriter {
    tmp: PathBuf,
    path: PathBuf,
    out: BufWriter<flate2::write::GzEncoder<std::fs::File>>,
    pub rows: u64,
}

impl ExtractWriter {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        let f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        Ok(Self {
            tmp,
            path: path.to_path_buf(),
            out: BufWriter::new(enc),
            rows: 0,
        })
    }

    pub fn write<T: Serialize>(&mut self, row: &T) -> Result<()> {
        serde_json::to_writer(&mut self.out, row)?;
        self.out.write_all(b"\n")?;
        self.rows += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        let enc = self
            .out
            .into_inner()
            .map_err(|e| anyhow::anyhow!("flushing: {e}"))?;
        enc.finish()?.sync_all()?;
        std::fs::rename(&self.tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        Ok(())
    }
}

/// Every author extract under the snapshot directory, in a stable order.
pub fn author_extracts(dir: &Path) -> Result<Vec<PathBuf>> {
    let root = dir.join(AUTHORS_DIR);
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for part in std::fs::read_dir(&root)? {
        let part = part?.path();
        if !part.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&part)? {
            let f = f?.path();
            if f.extension().is_some_and(|e| e == "gz") {
                out.push(f);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The release date recorded by the fetch, or "unknown" for hand-made dirs.
fn release_of(dir: &Path) -> String {
    let p = dir.join(AUTHORS_DIR).join(MANIFEST_FILE);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("date").and_then(|d| d.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

/// Build `authors.idx` from the extracts in `dir`.
///
/// Everything is held in memory — a 16-byte triple per key — then sorted
/// once. That is a few gigabytes for the full snapshot, which is fine for the
/// machine that builds it and irrelevant to the machine that reads it.
pub fn build(dir: &Path, policy: &BuildPolicy) -> Result<BuildReport> {
    let mut report = BuildReport {
        release: release_of(dir),
        ..Default::default()
    };

    // Institutions first: entries reference them by position.
    let mut institutions: Vec<InstitutionExtract> = Vec::new();
    let mut inst_index: HashMap<u64, u32> = HashMap::new();
    let inst_path = dir.join(INSTITUTIONS_EXTRACT);
    for line in open_gz(&inst_path)?.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let inst: InstitutionExtract = serde_json::from_str(&line)
            .with_context(|| format!("parsing {}", inst_path.display()))?;
        inst_index.insert(inst.id, institutions.len() as u32);
        institutions.push(inst);
    }
    if institutions.is_empty() {
        bail!(
            "no institutions in {}; fetch the snapshot first",
            inst_path.display()
        );
    }
    report.institutions = institutions.len() as u64;

    // (hash, works, set) — sorted so the most prolific author of each key
    // comes first and the rest can be dropped.
    let mut entries: Vec<(u64, u32, u32)> = Vec::new();
    let mut sets: Vec<[u32; SET_WIDTH]> = Vec::new();
    let mut set_index: HashMap<[u32; SET_WIDTH], u32> = HashMap::new();

    let files = author_extracts(dir)?;
    if files.is_empty() {
        bail!(
            "no author extracts under {}; fetch the snapshot first",
            dir.display()
        );
    }
    for (n, path) in files.iter().enumerate() {
        for line in open_gz(path)?.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let a: AuthorExtract = serde_json::from_str(&line)
                .with_context(|| format!("parsing {}", path.display()))?;
            report.authors_seen += 1;
            if a.works < policy.min_works {
                continue;
            }
            let mut set = [NO_INSTITUTION; SET_WIDTH];
            let mut n_inst = 0;
            for id in &a.institutions {
                if let Some(&ix) = inst_index.get(id) {
                    set[n_inst] = ix;
                    n_inst += 1;
                    if n_inst == SET_WIDTH {
                        break;
                    }
                }
            }
            if n_inst == 0 {
                continue;
            }
            report.authors_kept += 1;
            let set_id = *set_index.entry(set).or_insert_with(|| {
                sets.push(set);
                (sets.len() - 1) as u32
            });

            let mut keys = names::index_keys(&a.name);
            if policy.alternatives {
                for alt in &a.alternatives {
                    for k in names::index_keys(alt) {
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                }
            }
            for k in keys {
                entries.push((hash_key(&k), a.works, set_id));
            }
        }
        if (n + 1) % 25 == 0 || n + 1 == files.len() {
            tracing::info!(
                files = format!("{}/{}", n + 1, files.len()),
                seen = report.authors_seen,
                kept = report.authors_kept,
                keys = entries.len(),
                "indexing"
            );
        }
    }

    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    entries.dedup_by_key(|e| e.0);
    report.keys = entries.len() as u64;
    report.sets = sets.len() as u64;

    let tail = serde_json::to_vec(&Tail {
        release: report.release.clone(),
        institutions,
    })?;
    let path = dir.join(INDEX_FILE);
    let tmp = path.with_extension("idx.tmp");
    {
        let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&(entries.len() as u64).to_le_bytes())?;
        w.write_all(&(sets.len() as u64).to_le_bytes())?;
        w.write_all(&(tail.len() as u64).to_le_bytes())?;
        for (h, _, _) in &entries {
            w.write_all(&h.to_le_bytes())?;
        }
        for (_, _, s) in &entries {
            w.write_all(&s.to_le_bytes())?;
        }
        for set in &sets {
            for ix in set {
                w.write_all(&ix.to_le_bytes())?;
            }
        }
        w.write_all(&tail)?;
        w.flush()?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    report.bytes = std::fs::metadata(&path)?.len();
    Ok(report)
}

#[derive(Serialize, Deserialize)]
struct Tail {
    release: String,
    institutions: Vec<InstitutionExtract>,
}

const HEADER: usize = 4 + 4 + 8 + 8 + 8;

/// The loaded index. Hashes and set ids stay in the raw buffer and are read
/// in place; only the institution table is deserialised.
pub struct AuthorIndex {
    data: Vec<u8>,
    n_keys: usize,
    n_sets: usize,
    release: String,
    institutions: Vec<InstitutionExtract>,
    by_id: HashMap<u64, u32>,
    /// Folded institution name -> row, so an affiliation read off a paper
    /// resolves offline instead of costing a search.
    by_name: HashMap<u64, u32>,
}

impl AuthorIndex {
    pub fn load(path: &Path) -> Result<Self> {
        let mut f =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut data = Vec::with_capacity(f.metadata()?.len() as usize);
        f.read_to_end(&mut data)?;
        if data.len() < HEADER || &data[..4] != MAGIC {
            bail!("{} is not an author index", path.display());
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != VERSION {
            bail!(
                "{} is index version {version}; this build reads {VERSION}",
                path.display()
            );
        }
        let n_keys = u64::from_le_bytes(data[8..16].try_into().unwrap()) as usize;
        let n_sets = u64::from_le_bytes(data[16..24].try_into().unwrap()) as usize;
        let tail_len = u64::from_le_bytes(data[24..32].try_into().unwrap()) as usize;
        let expect = HEADER + n_keys * 12 + n_sets * SET_WIDTH * 4 + tail_len;
        if data.len() != expect {
            bail!(
                "{} is truncated: {} bytes, expected {expect}",
                path.display(),
                data.len()
            );
        }
        let tail: Tail = serde_json::from_slice(&data[expect - tail_len..])
            .context("reading institution table")?;
        let by_id = tail
            .institutions
            .iter()
            .enumerate()
            .map(|(i, inst)| (inst.id, i as u32))
            .collect();
        // Folded names, built at load rather than stored: 143k entries costs
        // milliseconds and keeps the on-disk format unchanged.
        let mut by_name: HashMap<u64, u32> = HashMap::with_capacity(tail.institutions.len());
        for (i, inst) in tail.institutions.iter().enumerate() {
            let k = names::key(&inst.record.display_name);
            if k.len() >= 6 {
                // First writer wins: the institution table is ordered by id,
                // and a lower id is the older, more canonical record.
                by_name.entry(hash_key(&k)).or_insert(i as u32);
            }
        }
        Ok(Self {
            data,
            n_keys,
            n_sets,
            release: tail.release,
            institutions: tail.institutions,
            by_id,
            by_name,
        })
    }

    pub fn len(&self) -> usize {
        self.n_keys
    }

    pub fn is_empty(&self) -> bool {
        self.n_keys == 0
    }

    pub fn institutions_len(&self) -> usize {
        self.institutions.len()
    }

    /// The snapshot release date this was built from.
    pub fn release(&self) -> &str {
        &self.release
    }

    fn key_at(&self, i: usize) -> u64 {
        let off = HEADER + i * 8;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    fn set_at(&self, i: usize) -> u32 {
        let off = HEADER + self.n_keys * 8 + i * 4;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    fn set_members(&self, set: u32) -> impl Iterator<Item = u32> + '_ {
        let base = HEADER + self.n_keys * 12 + set as usize * SET_WIDTH * 4;
        (0..SET_WIDTH)
            .map(move |j| {
                let off = base + j * 4;
                u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
            })
            .take_while(|&ix| ix != NO_INSTITUTION)
    }

    fn find(&self, hash: u64) -> Option<u32> {
        let (mut lo, mut hi) = (0usize, self.n_keys);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.key_at(mid).cmp(&hash) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(self.set_at(mid)),
            }
        }
        None
    }

    /// The institutions filed under a name, most recent first, as full
    /// OpenAlex ids — the same shape the API path produces, so the caller
    /// cannot tell which one answered.
    pub fn lookup(&self, name: &str) -> Option<Vec<String>> {
        for k in names::lookup_keys(name) {
            if let Some(set) = self.find(hash_key(&k)) {
                if set as usize >= self.n_sets {
                    return None;
                }
                let ids = self
                    .set_members(set)
                    .filter_map(|ix| self.institutions.get(ix as usize))
                    .map(|i| openalex_institution_id(i.id))
                    .collect();
                return Some(ids);
            }
        }
        None
    }

    /// Resolve an institution id (either form) without the network.
    pub fn institution(&self, id: &str) -> Option<&InstitutionRecord> {
        let n = numeric_id(id)?;
        self.by_id
            .get(&n)
            .map(|&ix| &self.institutions[ix as usize].record)
    }

    /// Resolve an affiliation string to an institution, without the network.
    ///
    /// The snapshot already carries all 143,455 institution names; only ids
    /// were indexed, so an affiliation read off a paper still cost a search.
    /// That is what left 219 of them unresolved the moment the daily
    /// allowance ran out, including plain ones like "University of
    /// Regensburg, 93053 Regensburg, Germany".
    ///
    /// Affiliations are written as a trail from the specific to the general —
    /// institution, city, postcode, country — so each comma-separated segment
    /// is tried in turn and the first that names an institution wins. The
    /// whole string is tried first, for the rare name that contains a comma.
    pub fn institution_by_name(&self, affiliation: &str) -> Option<&InstitutionRecord> {
        let whole = crate::names::key(affiliation);
        if let Some(&ix) = self.by_name.get(&hash_key(&whole)) {
            return Some(&self.institutions[ix as usize].record);
        }
        for segment in affiliation.split(',') {
            let k = crate::names::key(segment);
            // One- and two-letter fragments match noise, not institutions.
            if k.len() < 6 {
                continue;
            }
            if let Some(&ix) = self.by_name.get(&hash_key(&k)) {
                return Some(&self.institutions[ix as usize].record);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(id: u64, name: &str, lat: f64, lon: f64) -> InstitutionExtract {
        InstitutionExtract {
            id,
            record: InstitutionRecord {
                display_name: name.into(),
                ror: None,
                country: Some("DE".into()),
                city: None,
                lat: Some(lat),
                lon: Some(lon),
            },
        }
    }

    fn author(id: u64, name: &str, alts: &[&str], works: u32, insts: &[u64]) -> AuthorExtract {
        AuthorExtract {
            id,
            name: name.into(),
            alternatives: alts.iter().map(|s| s.to_string()).collect(),
            works,
            institutions: insts.to_vec(),
        }
    }

    /// A tiny snapshot directory with one extract file each.
    fn fixture(authors: &[AuthorExtract]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nf-snap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(AUTHORS_DIR).join("updated_date=2026-01-01")).unwrap();
        let mut w = ExtractWriter::create(&dir.join(INSTITUTIONS_EXTRACT)).unwrap();
        w.write(&inst(
            162_018_573,
            "Technical University of Munich",
            48.149,
            11.568,
        ))
        .unwrap();
        w.write(&inst(
            63_966_007,
            "Massachusetts Institute of Technology",
            42.36,
            -71.09,
        ))
        .unwrap();
        w.finish().unwrap();
        let mut w = ExtractWriter::create(
            &dir.join(AUTHORS_DIR)
                .join("updated_date=2026-01-01")
                .join("part_0000.jsonl.gz"),
        )
        .unwrap();
        for a in authors {
            w.write(a).unwrap();
        }
        w.finish().unwrap();
        dir
    }

    #[test]
    fn an_affiliation_resolves_by_name_without_the_network() {
        // A real sample that went unresolved the moment the allowance ran
        // out, even though the snapshot has held the institution all along.
        let dir = fixture(&[author(1, "Someone", &[], 5, &[162_018_573])]);
        build(&dir, &BuildPolicy::default()).unwrap();
        let idx = AuthorIndex::load(&dir.join(INDEX_FILE)).unwrap();

        let got = idx
            .institution_by_name("Technical University of Munich, 80333 Munich, Germany")
            .expect("the institution segment should match");
        assert_eq!(got.display_name, "Technical University of Munich");

        // The whole string matches too when it happens to be the bare name.
        assert!(idx
            .institution_by_name("Technical University of Munich")
            .is_some());
        assert!(idx.institution_by_name("Somewhere Entirely Else").is_none());
        // Short fragments must not match noise.
        assert!(idx.institution_by_name("DE, 80333, Germany").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn numeric_ids_parse_both_forms() {
        assert_eq!(
            numeric_id("https://openalex.org/I162018573"),
            Some(162_018_573)
        );
        assert_eq!(numeric_id("A5023888391"), Some(5_023_888_391));
        assert_eq!(numeric_id("nope"), None);
    }

    #[test]
    fn hash_is_stable() {
        // The on-disk format depends on this never changing.
        assert_eq!(hash_key(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash_key("nilsthuerey"), hash_key("nilsthuerey"));
        assert_ne!(hash_key("nilsthuerey"), hash_key("nilsthurey"));
    }

    #[test]
    fn builds_loads_and_resolves_the_thuerey_case() {
        let dir = fixture(&[author(
            1,
            "Nils Thürey",
            &["N. Thürey"],
            200,
            &[162_018_573],
        )]);
        let report = build(&dir, &BuildPolicy::default()).unwrap();
        assert_eq!(report.authors_kept, 1);
        assert_eq!(report.institutions, 2);

        let idx = AuthorIndex::load(&dir.join(INDEX_FILE)).unwrap();
        for q in [
            "Nils Thuerey",
            "Nils Thürey",
            "Nils Thurey",
            "N. Thuerey",
            "nils thuerey",
        ] {
            let ids = idx.lookup(q).unwrap_or_else(|| panic!("{q} not found"));
            assert_eq!(
                ids,
                vec!["https://openalex.org/I162018573".to_string()],
                "{q}"
            );
        }
        assert!(idx.lookup("Someone Else").is_none());
        let tum = idx.institution("https://openalex.org/I162018573").unwrap();
        assert_eq!(tum.display_name, "Technical University of Munich");
        assert!(idx.institution("I999").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_collision_goes_to_the_more_prolific_author() {
        let dir = fixture(&[
            author(1, "Wei Wang", &[], 12, &[63_966_007]),
            author(2, "Wei Wang", &[], 340, &[162_018_573]),
            author(3, "Wei Wang", &[], 5, &[63_966_007]),
        ]);
        build(&dir, &BuildPolicy::default()).unwrap();
        let idx = AuthorIndex::load(&dir.join(INDEX_FILE)).unwrap();
        assert_eq!(idx.len(), 1, "one key for one name");
        assert_eq!(
            idx.lookup("Wei Wang").unwrap(),
            vec!["https://openalex.org/I162018573".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn policy_drops_the_tail_and_unknown_institutions() {
        let dir = fixture(&[
            author(1, "One Hit", &[], 1, &[162_018_573]),
            author(2, "Nowhere Known", &[], 50, &[424_242]),
            author(3, "Kept Author", &["K. Author"], 3, &[424_242, 63_966_007]),
        ]);
        let report = build(
            &dir,
            &BuildPolicy {
                min_works: 2,
                alternatives: false,
            },
        )
        .unwrap();
        assert_eq!(report.authors_seen, 3);
        assert_eq!(report.authors_kept, 1);
        let idx = AuthorIndex::load(&dir.join(INDEX_FILE)).unwrap();
        assert!(idx.lookup("One Hit").is_none());
        assert!(idx.lookup("Nowhere Known").is_none());
        assert_eq!(
            idx.lookup("Kept Author").unwrap(),
            vec!["https://openalex.org/I63966007".to_string()]
        );
        assert!(
            idx.lookup("K. Author").is_none(),
            "alternatives were switched off"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn alternatives_catch_the_initials_form() {
        let dir = fixture(&[author(
            1,
            "Rahul Gaur",
            &["R. Gaur", "R Gaur"],
            30,
            &[63_966_007],
        )]);
        build(&dir, &BuildPolicy::default()).unwrap();
        let idx = AuthorIndex::load(&dir.join(INDEX_FILE)).unwrap();
        assert!(idx.lookup("R. Gaur").is_some());
        assert_eq!(idx.len(), 2, "display name plus one distinct alternative");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_a_foreign_file() {
        let p = std::env::temp_dir().join(format!("nf-notidx-{}", std::process::id()));
        std::fs::write(&p, b"{}").unwrap();
        assert!(AuthorIndex::load(&p).is_err());
        std::fs::remove_file(&p).ok();
    }
}
