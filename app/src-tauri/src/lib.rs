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

/// Fetch a JSON document, falling back to the on-disk copy when offline.
async fn cached_json<T>(state: &AppState, name: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let url = format!("{DATA_BASE}/{name}");
    match state.http.get_text(&url).await {
        Ok(body) => {
            // Only overwrite the cache once the payload actually parses, so a
            // broken deploy cannot destroy a good offline copy.
            match serde_json::from_str::<T>(&body) {
                Ok(parsed) => {
                    std::fs::create_dir_all(&state.cache_dir).ok();
                    std::fs::write(state.cache_path(name), &body).ok();
                    Ok(parsed)
                }
                Err(e) => Err(format!("{name} from the server is malformed: {e}")),
            }
        }
        Err(net) => {
            let path = state.cache_path(name);
            let body = std::fs::read_to_string(&path)
                .map_err(|_| format!("offline and nothing cached for {name} ({net})"))?;
            serde_json::from_str::<T>(&body)
                .map_err(|e| format!("cached {name} is unreadable: {e}"))
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
) -> Result<Option<UpdateInfo>, String> {
    let url = format!("{UPDATE_BASE}/{CHANNEL}.json");
    let body = state
        .http
        .get_text(&url)
        .await
        .map_err(|e| format!("could not reach the update channel: {e}"))?;
    let m: Manifest =
        serde_json::from_str(&body).map_err(|e| format!("bad update manifest: {e}"))?;

    if nearfield_core::version::is_newer(&m.version, VERSION) {
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
            geocode_place
        ])
        .run(tauri::generate_context!())
        .expect("error while running nearfield");
}
