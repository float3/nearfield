//! The app's Rust side.
//!
//! It exists for three reasons the webview cannot cover on its own: arxiv.org
//! sends no cross-origin headers, so on-demand introduction fetching has to
//! happen outside the webview; every digest is cached to disk so the list is
//! readable with no signal; and handing a PDF URL to the phone's own reader is
//! an Android intent, not a navigation.

use nearfield_core::http::Http;
use nearfield_core::model::{Digest, GeoSource, Paper};
use nearfield_core::geocode::{self, Place};
use nearfield_core::{paper, pipeline::IndexEntry};
use std::path::PathBuf;
use std::time::Duration;
use tauri::Manager;

const DATA_BASE: &str = "https://hilll.dev/nearfield/data";
const UPDATE_BASE: &str = "https://hilll.dev/nearfield/updates";
/// Contact address sent to arXiv in the User-Agent, as their acceptable-use
/// policy asks. Injected at build time from `NEARFIELD_CONTACT` so a real
/// address never sits in public source; an unset build falls back to a
/// placeholder rather than silently claiming to be someone.
const CONTACT: &str = match option_env!("NEARFIELD_CONTACT") {
    Some(c) => c,
    None => "nearfield@example.invalid",
};

/// Which release channel this build follows. Set at build time by CI; a local
/// build is treated as stable.
const CHANNEL: &str = match option_env!("NEARFIELD_CHANNEL") {
    Some(c) => c,
    None => "stable",
};

/// The full version string, including a nightly build stamp when CI set one.
const VERSION: &str = match option_env!("NEARFIELD_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

struct AppState {
    http: Http,
    cache_dir: PathBuf,
}

impl AppState {
    fn cache_path(&self, name: &str) -> PathBuf {
        self.cache_dir.join(name)
    }
}

/// Store a day gzipped.
///
/// A digest is ~4.4 MB of JSON but only ~1.5 MB on the wire, because Pages
/// serves it compressed. Keeping a month of history is the difference between
/// 132 MB and 45 MB of the phone's storage, so we re-compress before writing.
fn write_cached(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(body.as_bytes())?;
    std::fs::write(path, enc.finish()?)
}

fn read_cached(path: &std::path::Path) -> std::io::Result<String> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let raw = std::fs::read(path)?;
    // Caches written before compression was introduced are plain JSON, and a
    // reinstall is not worth forcing on anyone.
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::new();
        GzDecoder::new(&raw[..]).read_to_string(&mut s)?;
        Ok(s)
    } else {
        String::from_utf8(raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Fetch a JSON document and keep a copy for offline use.
async fn fetch_and_cache(state: &AppState, name: &str) -> Result<String, String> {
    let url = format!("{DATA_BASE}/{name}");
    let body = state.http.get_text(&url).await.map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&state.cache_dir).ok();
    write_cached(&state.cache_path(name), &body).ok();
    Ok(body)
}

/// Fetch a JSON document, falling back to the on-disk copy when offline.
async fn cached_json<T>(state: &AppState, name: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let url = format!("{DATA_BASE}/{name}");
    match state.http.get_text(&url).await {
        Ok(body) => {
            // Only cache once the payload actually parses, so a broken deploy
            // cannot destroy a good offline copy.
            match serde_json::from_str::<T>(&body) {
                Ok(parsed) => {
                    std::fs::create_dir_all(&state.cache_dir).ok();
                    write_cached(&state.cache_path(name), &body).ok();
                    Ok(parsed)
                }
                Err(e) => Err(format!("{name} from the server is malformed: {e}")),
            }
        }
        Err(net) => {
            let body = read_cached(&state.cache_path(name))
                .map_err(|_| format!("offline and nothing saved for {name} ({net})"))?;
            serde_json::from_str::<T>(&body)
                .map_err(|e| format!("saved {name} is unreadable: {e}"))
        }
    }
}

#[tauri::command]
async fn load_index(state: tauri::State<'_, AppState>) -> Result<Vec<IndexEntry>, String> {
    cached_json::<Vec<IndexEntry>>(&state, "index.json").await
}

#[tauri::command]
async fn load_digest(
    state: tauri::State<'_, AppState>,
    date: String,
) -> Result<Digest, String> {
    // The date lands in a path, so refuse anything that is not one.
    if date.len() != 10 || !date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Err(format!("not a date: {date}"));
    }
    cached_json::<Digest>(&state, &format!("{date}.json")).await
}

/// Pull one paper's introduction and emails from arXiv right now.
#[tauri::command]
async fn fetch_paper(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Paper, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '/') {
        return Err(format!("not an arXiv id: {id}"));
    }
    let mut p = Paper {
        id: id.clone(),
        title: String::new(),
        abstract_text: String::new(),
        categories: vec![],
        announced: String::new(),
        authors: vec![],
        affiliations: vec![],
        contact_emails: vec![],
        distance_km: None,
        geo_source: GeoSource::None,
        intro: None,
        enriched: false,
    };
    match paper::enrich(&state.http, &mut p).await {
        Ok(true) => Ok(p),
        Ok(false) => Err(format!(
            "arXiv has no HTML rendering of {id}, so there is no introduction to extract"
        )),
        Err(e) => Err(e.to_string()),
    }
}

// --- offline storage ----------------------------------------------------

/// A date the phone holds, and what it costs in storage.
#[derive(serde::Serialize)]
struct SavedDay {
    date: String,
    bytes: u64,
}

fn is_date(s: &str) -> bool {
    s.len() == 10 && s.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// Which days are readable with no signal.
#[tauri::command]
fn saved_days(state: tauri::State<'_, AppState>) -> Vec<SavedDay> {
    let Ok(entries) = std::fs::read_dir(&state.cache_dir) else {
        return Vec::new();
    };
    let mut out: Vec<SavedDay> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let date = name.strip_suffix(".json")?;
            if !is_date(date) {
                return None;
            }
            Some(SavedDay {
                date: date.to_string(),
                bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

#[derive(serde::Serialize)]
struct DownloadReport {
    saved: Vec<String>,
    failed: Vec<String>,
    bytes: u64,
}

/// Download days for later, without having to open each one first.
///
/// This is the before-a-flight button: the digest already carries the
/// introductions and emails for the nearest papers, so a saved day is fully
/// readable offline. Papers outside that nightly prefetch still need the
/// network for their introduction, which no amount of caching can change.
#[tauri::command]
async fn download_days(
    state: tauri::State<'_, AppState>,
    dates: Vec<String>,
) -> Result<DownloadReport, String> {
    let mut report = DownloadReport { saved: vec![], failed: vec![], bytes: 0 };
    for date in dates {
        if !is_date(&date) {
            report.failed.push(date);
            continue;
        }
        let name = format!("{date}.json");
        match fetch_and_cache(&state, &name).await {
            Ok(_) => {
                report.bytes += std::fs::metadata(state.cache_path(&name))
                    .map(|m| m.len())
                    .unwrap_or(0);
                report.saved.push(date);
            }
            Err(e) => {
                tracing::warn!(%date, error = %e, "could not save day");
                report.failed.push(date);
            }
        }
    }
    Ok(report)
}

/// Free the storage a day is using.
#[tauri::command]
fn remove_day(state: tauri::State<'_, AppState>, date: String) -> Result<(), String> {
    if !is_date(&date) {
        return Err(format!("not a date: {date}"));
    }
    std::fs::remove_file(state.cache_path(&format!("{date}.json")))
        .map_err(|e| format!("could not remove {date}: {e}"))
}

#[derive(serde::Serialize)]
struct AppInfo {
    version: &'static str,
    channel: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo { version: VERSION, channel: CHANNEL }
}

/// One channel's published build, as written by the release workflow.
#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    /// Direct link to the APK asset on the GitHub release.
    url: String,
    #[serde(default)]
    pub_date: String,
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    version: String,
    notes: String,
    url: String,
    pub_date: String,
}

/// Is there a newer build on this channel?
///
/// `Ok(None)` means "up to date", which is the common case and not an error.
/// The official Tauri updater is desktop-only (its dependencies are gated to
/// `cfg(not(target_os = "android"))`), so this is a deliberate hand-rolled
/// check. Installing is still Android's decision: we hand the APK URL to the
/// system, which always asks the user to confirm.
#[tauri::command]
async fn check_update(
    state: tauri::State<'_, AppState>,
    channel: Option<String>,
) -> Result<Option<UpdateInfo>, String> {
    // The channel lands in a URL path, and it can come from a stored
    // preference, so only the two known names are allowed through.
    let channel = match channel.as_deref() {
        None => CHANNEL,
        Some("stable") => "stable",
        Some("nightly") => "nightly",
        Some(other) => return Err(format!("unknown channel: {other}")),
    };
    let url = format!("{UPDATE_BASE}/{channel}.json");
    let body = state
        .http
        .get_text(&url)
        .await
        .map_err(|e| format!("could not reach the update channel: {e}"))?;
    let m: Manifest =
        serde_json::from_str(&body).map_err(|e| format!("bad update manifest: {e}"))?;

    // Switching channels should offer the other channel's build even when it
    // is not strictly newer — moving from nightly back to stable is a
    // downgrade by version but is what the user asked for.
    let switching = channel != CHANNEL;
    if switching || nearfield_core::version::is_newer(&m.version, VERSION) {
        if m.version == VERSION {
            return Ok(None);
        }
        Ok(Some(UpdateInfo {
            version: m.version,
            notes: m.notes,
            url: m.url,
            pub_date: m.pub_date,
        }))
    } else {
        Ok(None)
    }
}

/// Look up a place name for the origin picker.
///
/// Lives in Rust because Nominatim sends no cross-origin headers, and because
/// their usage policy wants an identifying User-Agent and one request a
/// second — both of which the shared client already honours.
#[tauri::command]
async fn geocode_place(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<Place>, String> {
    geocode::search(&state.http, &query, 6)
        .await
        .map_err(|e| format!("place lookup failed: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_geolocation::init())
        .setup(|app| {
            let cache_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("digests");
            std::fs::create_dir_all(&cache_dir).ok();

            let http = Http::new(CONTACT)?;
            // Same courtesy rate limit the pipeline uses.
            let h = http.clone();
            tauri::async_runtime::block_on(async move {
                h.set_rate("arxiv.org", Duration::from_secs(3)).await;
                // Nominatim asks for at most one request a second.
                h.set_rate("nominatim.openstreetmap.org", Duration::from_secs(1)).await;
            });

            app.manage(AppState { http, cache_dir });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_index,
            load_digest,
            fetch_paper,
            app_info,
            check_update,
            geocode_place,
            saved_days,
            download_days,
            remove_day
        ])
        .run(tauri::generate_context!())
        .expect("error while running nearfield");
}
