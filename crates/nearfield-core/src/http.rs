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

/// Longest we will ever wait between attempts.
///
/// A throttled OpenAlex can answer `Retry-After: 3600`. Sleeping on that
/// stalls the whole nightly run for an hour and achieves nothing — the daily
/// budget is not coming back within this job. Waiting a bounded moment and
/// then letting the caller's circuit breaker trip is strictly better: the
/// cached locations still apply and the next run continues.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// A 429 is only worth waiting out if the server wants us back soon.
pub const SHORT_RETRY: Duration = Duration::from_secs(5);

/// How long to wait before the next attempt, honouring `Retry-After` but
/// never beyond [`MAX_RETRY_WAIT`].
pub fn retry_wait(retry_after: Option<Duration>, backoff: Duration) -> Duration {
    retry_after.unwrap_or(backoff).min(MAX_RETRY_WAIT)
}

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
    ///
    /// The nightly job has a finite budget, so a server that asks us to wait
    /// an hour gets told no: see [`retry_wait`].
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

                    // A 429 from a *daily* budget will not clear inside this
                    // run, and retrying it cost 4 attempts x 30s per lookup —
                    // enough to stall a night. Give it one short retry in case
                    // it is a per-second limit, then give up and let the
                    // caller degrade.
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        && (attempt > 1 || retry_after.is_some_and(|w| w > SHORT_RETRY))
                    {
                        anyhow::bail!("429 rate limited: {url}");
                    }

                    if attempt == ATTEMPTS {
                        anyhow::bail!("{status} after {ATTEMPTS} attempts: {url}");
                    }
                    let wait = retry_wait(retry_after, backoff);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_backoff_when_no_header() {
        assert_eq!(retry_wait(None, Duration::from_secs(4)), Duration::from_secs(4));
    }

    #[test]
    fn honours_a_reasonable_retry_after() {
        assert_eq!(
            retry_wait(Some(Duration::from_secs(10)), Duration::from_secs(2)),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn refuses_to_sleep_for_an_hour() {
        // Regression: a throttled OpenAlex answers Retry-After: 3600, which
        // stalled an entire nightly run.
        assert_eq!(
            retry_wait(Some(Duration::from_secs(3600)), Duration::from_secs(2)),
            MAX_RETRY_WAIT
        );
    }

    #[test]
    fn caps_runaway_backoff_too() {
        assert_eq!(retry_wait(None, Duration::from_secs(600)), MAX_RETRY_WAIT);
    }
}
