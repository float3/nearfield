//! One shared, deliberately polite HTTP client.
//!
//! arXiv, OpenAlex and ROR all ask bulk users to identify themselves and to
//! throttle. We send a contact address in the User-Agent and serialise each
//! host behind its own minimum delay.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    /// Per-host: minimum gap, and when we last spoke to it.
    gates: Arc<Mutex<HashMap<String, (Duration, Instant)>>>,
}

impl Http {
    pub fn new(contact: &str) -> Result<Self> {
        let ua = format!(
            "nearfield/{} (daily preprint digest; mailto:{})",
            env!("CARGO_PKG_VERSION"),
            contact
        );
        let client = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(60))
            .build()
            .context("building http client")?;
        Ok(Self { client, gates: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// Declare the minimum delay between requests to a host.
    pub async fn set_rate(&self, host: &str, gap: Duration) {
        let mut g = self.gates.lock().await;
        g.insert(host.to_string(), (gap, Instant::now() - gap));
    }

    async fn wait_turn(&self, url: &str) {
        let host = match reqwest::Url::parse(url) {
            Ok(u) => u.host_str().unwrap_or_default().to_string(),
            Err(_) => return,
        };
        // Hold the lock across the sleep so requests to one host queue up
        // rather than all waking at once.
        let mut g = self.gates.lock().await;
        if let Some((gap, last)) = g.get(&host).copied() {
            let elapsed = last.elapsed();
            if elapsed < gap {
                sleep(gap - elapsed).await;
            }
            g.insert(host, (gap, Instant::now()));
        }
    }

    /// GET with rate limiting and retry on 429/5xx, honouring Retry-After.
    pub async fn get_text(&self, url: &str) -> Result<String> {
        const ATTEMPTS: u32 = 4;
        let mut backoff = Duration::from_secs(2);

        for attempt in 1..=ATTEMPTS {
            self.wait_turn(url).await;
            let resp = self.client.get(url).send().await;

            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        return r.text().await.context("reading body");
                    }
                    // 404 is a real answer (no HTML version of this paper) and
                    // 401/403 means the credential is wrong, not that the
                    // server is busy. Retrying either just wastes time — and
                    // an unactivated OpenAlex key 403s on every call, so the
                    // caller needs to hear about it on the first attempt.
                    if matches!(
                        status,
                        reqwest::StatusCode::NOT_FOUND
                            | reqwest::StatusCode::UNAUTHORIZED
                            | reqwest::StatusCode::FORBIDDEN
                    ) {
                        anyhow::bail!("{} {url}", status.as_u16());
                    }
                    let retry_after = r
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);

                    if attempt == ATTEMPTS {
                        anyhow::bail!("{status} after {ATTEMPTS} attempts: {url}");
                    }
                    let wait = retry_after.unwrap_or(backoff);
                    tracing::warn!(%status, ?wait, url, "retrying");
                    sleep(wait).await;
                    backoff *= 2;
                }
                Err(e) => {
                    if attempt == ATTEMPTS {
                        return Err(e).context(format!("GET {url}"));
                    }
                    tracing::warn!(error = %e, url, "transport error, retrying");
                    sleep(backoff).await;
                    backoff *= 2;
                }
            }
        }
        unreachable!("loop returns or bails")
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let body = self.get_text(url).await?;
        serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))
    }
}
