//! Pulling the needed columns out of the OpenAlex parquet snapshot.
//!
//! The snapshot is anonymous-readable over plain HTTPS: no credentials, no
//! AWS tooling. Each entity is a set of parquet files listed in a
//! `manifest.json`, and a parquet file stores every column of a row group in
//! its own contiguous byte range. So instead of downloading the 50 GB
//! authors entity we read each file's footer (~150 KB), ask the parquet
//! reader for five leaf columns, and it fetches only those ranges — measured
//! at 3–5 % of each large file. Rows with no institution are dropped on the
//! fly, since the index could do nothing with them.
//!
//! Each file's extract is written atomically and skipped on the next run,
//! so a download that takes an hour over a slow link can be interrupted and
//! resumed at the file boundary. A new release (different manifest date)
//! starts the authors over, because the files are re-partitioned each time.

use crate::http::Http;
use crate::snapshot::{
    numeric_id, AuthorExtract, ExtractWriter, InstitutionExtract, AUTHORS_DIR,
    INSTITUTIONS_EXTRACT, MANIFEST_FILE,
};
use crate::openalex::InstitutionRecord;
use anyhow::{bail, Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int32Type};
use arrow_array::{Array, RecordBatch};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryStreamExt};
use parquet::arrow::arrow_reader::ArrowReaderOptions;
use parquet::arrow::async_reader::{AsyncFileReader, ParquetRecordBatchStreamBuilder};
use parquet::arrow::ProjectionMask;
use parquet::errors::ParquetError;
use parquet::file::metadata::{ParquetMetaData, ParquetMetaDataReader};
use serde::Deserialize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BUCKET: &str = "https://openalex.s3.amazonaws.com";
pub const BUCKET_HOST: &str = "openalex.s3.amazonaws.com";

/// S3 serves range requests from many connections happily; the gate exists
/// so the request *rate* stays civil, not to serialise the transfers.
pub const REQUEST_GAP: Duration = Duration::from_millis(20);

/// A footer this large covers every authors file seen (max ~150 KB), so the
/// metadata costs one request rather than two.
const FOOTER_PREFETCH: usize = 512 * 1024;

const AUTHOR_LEAVES: [&str; 5] = [
    "id",
    "display_name",
    "display_name_alternatives.list.element",
    "works_count",
    "last_known_institutions.list.element.id",
];

const INSTITUTION_LEAVES: [&str; 7] = [
    "id",
    "display_name",
    "ror",
    "geo.city",
    "geo.country_code",
    "geo.latitude",
    "geo.longitude",
];

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub date: String,
    pub entity: String,
    pub record_count: u64,
    pub content_length: u64,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    /// `s3://openalex/data/parquet/...`
    pub url: String,
    pub meta: FileMeta,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileMeta {
    pub content_length: u64,
    pub record_count: u64,
}

impl ManifestFile {
    pub fn https(&self) -> String {
        format!("{BUCKET}/{}", self.url.trim_start_matches("s3://openalex/"))
    }

    /// `updated_date=2026-10-13/part_0018`, the part of the key that is
    /// unique within an entity.
    pub fn stem(&self) -> String {
        let mut parts = self.url.rsplit('/');
        let file = parts.next().unwrap_or_default().trim_end_matches(".parquet");
        let partition = parts.next().unwrap_or_default();
        format!("{partition}/{file}")
    }
}

pub async fn manifest(http: &Http, entity: &str) -> Result<Manifest> {
    http.get_json(&format!("{BUCKET}/data/parquet/{entity}/manifest.json"))
        .await
        .with_context(|| format!("fetching the {entity} manifest"))
}

#[derive(Debug, Default, Clone)]
pub struct FetchReport {
    pub release: String,
    pub files_total: usize,
    pub files_fetched: usize,
    pub rows_scanned: u64,
    pub rows_kept: u64,
    pub bytes_fetched: u64,
    pub elapsed: Duration,
}

/// A parquet file on S3, read through the shared polite client.
struct RemoteFile {
    http: Http,
    url: String,
    size: u64,
    fetched: Arc<AtomicU64>,
}

impl AsyncFileReader for RemoteFile {
    fn get_bytes(&mut self, range: Range<u64>) -> BoxFuture<'_, parquet::errors::Result<Bytes>> {
        async move {
            let b = self
                .http
                .get_range(&self.url, range.start, range.end)
                .await
                .map_err(|e| ParquetError::External(e.into()))?;
            self.fetched.fetch_add(b.len() as u64, Ordering::Relaxed);
            Ok(b)
        }
        .boxed()
    }

    fn get_metadata<'a>(
        &'a mut self,
        _options: Option<&'a ArrowReaderOptions>,
    ) -> BoxFuture<'a, parquet::errors::Result<Arc<ParquetMetaData>>> {
        async move {
            let size = self.size;
            let md = ParquetMetaDataReader::new()
                .with_prefetch_hint(Some(FOOTER_PREFETCH))
                .load_and_finish(self, size)
                .await?;
            Ok(Arc::new(md))
        }
        .boxed()
    }
}

/// Stream the projected columns of one remote file, batch by batch.
async fn open(
    http: &Http,
    file: &ManifestFile,
    leaves: &[&str],
    fetched: Arc<AtomicU64>,
) -> Result<parquet::arrow::async_reader::ParquetRecordBatchStream<RemoteFile>> {
    let reader = RemoteFile {
        http: http.clone(),
        url: file.https(),
        size: file.meta.content_length,
        fetched,
    };
    let builder = ParquetRecordBatchStreamBuilder::new_with_options(reader, ArrowReaderOptions::new())
        .await
        .with_context(|| format!("reading parquet footer of {}", file.https()))?;
    let schema = builder.parquet_schema();
    let mut indices = Vec::with_capacity(leaves.len());
    for want in leaves {
        let ix = (0..schema.num_columns())
            .find(|&i| schema.column(i).path().string() == *want)
            .with_context(|| format!("{want} is not a column of {}", file.https()))?;
        indices.push(ix);
    }
    let mask = ProjectionMask::leaves(schema, indices);
    Ok(builder.with_projection(mask).with_batch_size(16 * 1024).build()?)
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a arrow_array::StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("no column {name}"))?
        .as_string_opt::<i32>()
        .with_context(|| format!("column {name} is not utf8"))
}

/// Pull `AuthorExtract`s out of one batch, dropping rows with no institution.
fn author_rows(batch: &RecordBatch, out: &mut ExtractWriter) -> Result<u64> {
    let ids = str_col(batch, "id")?;
    let names = str_col(batch, "display_name")?;
    let works = batch
        .column_by_name("works_count")
        .context("no works_count")?
        .as_primitive::<Int32Type>();
    let alts = batch
        .column_by_name("display_name_alternatives")
        .context("no display_name_alternatives")?
        .as_list::<i32>();
    let alt_vals = alts.values().as_string::<i32>();
    let insts = batch
        .column_by_name("last_known_institutions")
        .context("no last_known_institutions")?
        .as_list::<i32>();
    let inst_ids = insts
        .values()
        .as_struct()
        .column_by_name("id")
        .context("last_known_institutions has no id")?
        .as_string::<i32>();

    let mut kept = 0;
    for row in 0..batch.num_rows() {
        if insts.is_null(row) || names.is_null(row) || ids.is_null(row) {
            continue;
        }
        let (s, e) = (insts.value_offsets()[row] as usize, insts.value_offsets()[row + 1] as usize);
        let institutions: Vec<u64> = (s..e)
            .filter(|&j| !inst_ids.is_null(j))
            .filter_map(|j| numeric_id(inst_ids.value(j)))
            .collect();
        if institutions.is_empty() {
            continue;
        }
        let Some(id) = numeric_id(ids.value(row)) else { continue };
        let name = names.value(row).trim();
        if name.is_empty() {
            continue;
        }
        let alternatives = if alts.is_null(row) {
            Vec::new()
        } else {
            let (s, e) = (alts.value_offsets()[row] as usize, alts.value_offsets()[row + 1] as usize);
            (s..e)
                .filter(|&j| !alt_vals.is_null(j))
                .map(|j| alt_vals.value(j).trim())
                .filter(|a| !a.is_empty() && *a != name)
                .map(String::from)
                .collect()
        };
        out.write(&AuthorExtract {
            id,
            name: name.to_string(),
            alternatives,
            works: if works.is_null(row) { 0 } else { works.value(row).max(0) as u32 },
            institutions,
        })?;
        kept += 1;
    }
    Ok(kept)
}

fn institution_rows(batch: &RecordBatch, out: &mut ExtractWriter) -> Result<u64> {
    let ids = str_col(batch, "id")?;
    let names = str_col(batch, "display_name")?;
    let rors = str_col(batch, "ror")?;
    let geo = batch.column_by_name("geo").context("no geo")?.as_struct();
    let city = geo.column_by_name("city").context("no geo.city")?.as_string::<i32>();
    let country = geo
        .column_by_name("country_code")
        .context("no geo.country_code")?
        .as_string::<i32>();
    let lat = geo
        .column_by_name("latitude")
        .context("no geo.latitude")?
        .as_primitive::<Float64Type>();
    let lon = geo
        .column_by_name("longitude")
        .context("no geo.longitude")?
        .as_primitive::<Float64Type>();

    let opt = |a: &arrow_array::StringArray, row: usize| {
        (!a.is_null(row) && !a.value(row).is_empty()).then(|| a.value(row).to_string())
    };
    let mut kept = 0;
    for row in 0..batch.num_rows() {
        if ids.is_null(row) || names.is_null(row) {
            continue;
        }
        let Some(id) = numeric_id(ids.value(row)) else { continue };
        let has_geo = !geo.is_null(row);
        out.write(&InstitutionExtract {
            id,
            record: InstitutionRecord {
                display_name: names.value(row).to_string(),
                ror: opt(rors, row),
                country: if has_geo { opt(country, row) } else { None },
                city: if has_geo { opt(city, row) } else { None },
                lat: (has_geo && !lat.is_null(row)).then(|| lat.value(row)),
                lon: (has_geo && !lon.is_null(row)).then(|| lon.value(row)),
            },
        })?;
        kept += 1;
    }
    Ok(kept)
}

/// Fetch every institution — it is 94 MB whole, and with projection far
/// less — into one extract. Skipped when it already exists.
pub async fn fetch_institutions(http: &Http, dir: &Path) -> Result<FetchReport> {
    let path = dir.join(INSTITUTIONS_EXTRACT);
    let started = Instant::now();
    let m = manifest(http, "institutions").await?;
    let mut report = FetchReport {
        release: m.date.clone(),
        files_total: m.files.len(),
        ..Default::default()
    };
    if path.exists() {
        tracing::info!(path = %path.display(), "institutions extract present; keeping it");
        return Ok(report);
    }
    let fetched = Arc::new(AtomicU64::new(0));
    let mut w = ExtractWriter::create(&path)?;
    for file in &m.files {
        let mut stream = open(http, file, &INSTITUTION_LEAVES, fetched.clone()).await?;
        while let Some(batch) = stream.try_next().await? {
            report.rows_scanned += batch.num_rows() as u64;
            report.rows_kept += institution_rows(&batch, &mut w)?;
        }
        report.files_fetched += 1;
    }
    w.finish()?;
    report.bytes_fetched = fetched.load(Ordering::Relaxed);
    report.elapsed = started.elapsed();
    tracing::info!(
        rows = report.rows_kept,
        mb = report.bytes_fetched / 1_000_000,
        secs = report.elapsed.as_secs(),
        "institutions extracted"
    );
    Ok(report)
}

fn extract_path(dir: &Path, file: &ManifestFile) -> PathBuf {
    dir.join(AUTHORS_DIR).join(format!("{}.jsonl.gz", file.stem()))
}

/// Fetch the author columns, `concurrency` files at a time, skipping any
/// extract already on disk.
///
/// Per-connection throughput from the bucket was measured at ~250 KB/s, and
/// aggregate throughput scales with connections up to the link, so the
/// concurrency is what makes a multi-gigabyte pull finish in under an hour.
pub async fn fetch_authors(http: &Http, dir: &Path, concurrency: usize) -> Result<FetchReport> {
    let started = Instant::now();
    let root = dir.join(AUTHORS_DIR);
    std::fs::create_dir_all(&root)?;
    let m = manifest(http, "authors").await?;

    // A different release re-partitions the files; old extracts would be
    // silently mixed with new ones.
    let manifest_path = root.join(MANIFEST_FILE);
    let previous: Option<Manifest> = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    if let Some(prev) = previous {
        if prev.date != m.date {
            tracing::warn!(
                from = %prev.date,
                to = %m.date,
                "snapshot release changed; discarding the old extracts"
            );
            std::fs::remove_dir_all(&root)?;
            std::fs::create_dir_all(&root)?;
        }
    }
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&serde_json::json!({
        "date": m.date,
        "entity": m.entity,
        "record_count": m.record_count,
        "content_length": m.content_length,
        "files": m.files.len(),
    }))?)?;

    let todo: Vec<&ManifestFile> =
        m.files.iter().filter(|f| !extract_path(dir, f).exists()).collect();
    let mut report = FetchReport {
        release: m.date.clone(),
        files_total: m.files.len(),
        ..Default::default()
    };
    tracing::info!(
        release = %m.date,
        files = m.files.len(),
        pending = todo.len(),
        rows = m.record_count,
        gb = format!("{:.1}", m.content_length as f64 / 1e9),
        "authors snapshot"
    );
    if todo.is_empty() {
        return Ok(report);
    }

    let fetched = Arc::new(AtomicU64::new(0));
    let mut done = 0usize;
    let mut tasks = futures::stream::iter(todo.into_iter().map(|file| {
        let fetched = fetched.clone();
        async move {
            let path = extract_path(dir, file);
            let mut w = ExtractWriter::create(&path)?;
            let mut stream = open(http, file, &AUTHOR_LEAVES, fetched).await?;
            let (mut scanned, mut kept) = (0u64, 0u64);
            while let Some(batch) = stream.try_next().await? {
                scanned += batch.num_rows() as u64;
                kept += author_rows(&batch, &mut w)?;
            }
            w.finish()?;
            Ok::<_, anyhow::Error>((file.stem(), scanned, kept))
        }
    }))
    .buffer_unordered(concurrency.max(1));

    while let Some(r) = tasks.next().await {
        let (stem, scanned, kept) = r?;
        done += 1;
        report.files_fetched += 1;
        report.rows_scanned += scanned;
        report.rows_kept += kept;
        let bytes = fetched.load(Ordering::Relaxed);
        let secs = started.elapsed().as_secs_f64().max(1.0);
        tracing::info!(
            file = %stem,
            done = format!("{done}/{}", report.files_total),
            scanned = report.rows_scanned,
            kept = report.rows_kept,
            mb = bytes / 1_000_000,
            mb_s = format!("{:.2}", bytes as f64 / 1e6 / secs),
            "extracted"
        );
    }
    report.bytes_fetched = fetched.load(Ordering::Relaxed);
    report.elapsed = started.elapsed();
    Ok(report)
}

/// Sanity check that the bucket layout still matches what this reader
/// expects, before committing to a long download.
pub async fn probe(http: &Http) -> Result<(Manifest, Manifest)> {
    let a = manifest(http, "authors").await?;
    let i = manifest(http, "institutions").await?;
    if a.files.is_empty() || i.files.is_empty() {
        bail!("snapshot manifests list no files");
    }
    Ok((a, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_urls_become_https_and_stems() {
        let f = ManifestFile {
            url: "s3://openalex/data/parquet/authors/updated_date=2026-10-13/part_0018.parquet".into(),
            meta: FileMeta { content_length: 1, record_count: 1 },
        };
        assert_eq!(
            f.https(),
            "https://openalex.s3.amazonaws.com/data/parquet/authors/updated_date=2026-10-13/part_0018.parquet"
        );
        assert_eq!(f.stem(), "updated_date=2026-10-13/part_0018");
    }
}
