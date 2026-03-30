use std::{thread, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{
    StatusCode,
    blocking::{Client, RequestBuilder, Response},
    header::{HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT},
};
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CratesIoClient {
    base_url: String,
    client: Client,
    max_retries: u32,
    retry_delay: Duration,
    max_retry_delay: Duration,
}

#[derive(Debug, Clone)]
pub struct CratesIoClientOptions {
    pub base_url: String,
    pub timeout: Duration,
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub max_retry_delay: Duration,
}

impl Default for CratesIoClientOptions {
    fn default() -> Self {
        Self {
            base_url: String::from("https://crates.io/api/v1"),
            timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CrateMetadataResponse {
    #[serde(rename = "crate")]
    metadata: CrateMetadata,
}

#[derive(Debug, Deserialize)]
struct CrateMetadata {
    #[serde(rename = "max_version")]
    latest: Option<String>,
    #[serde(rename = "max_stable_version")]
    latest_stable: Option<String>,
    #[serde(rename = "newest_version")]
    newest: Option<String>,
}

impl CratesIoClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::with_options(&CratesIoClientOptions {
            base_url: base_url.into(),
            ..CratesIoClientOptions::default()
        })
    }

    pub fn with_options(options: &CratesIoClientOptions) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("github-actions-maintainer"));

        let client = Client::builder()
            .default_headers(headers)
            .timeout(options.timeout)
            .build()
            .context("failed to build crates.io HTTP client")?;

        Ok(Self {
            base_url: options.base_url.trim_end_matches('/').to_owned(),
            client,
            max_retries: options.max_retries,
            retry_delay: options.retry_delay,
            max_retry_delay: options.max_retry_delay,
        })
    }

    pub fn latest_stable_version(&self, crate_name: &str) -> Result<String> {
        let encoded = urlencoding::encode(crate_name);
        let response = self.send_with_retry(
            || self.client.get(format!("{}/crates/{encoded}", self.base_url)),
            || format!("fetch crates.io metadata for {crate_name}"),
        )?;
        let metadata = response
            .json::<CrateMetadataResponse>()
            .with_context(|| format!("failed to decode crates.io metadata for {crate_name}"))?;

        select_stable_version(&metadata.metadata)
            .ok_or_else(|| anyhow!("no stable crates.io release found for {crate_name}"))
    }

    fn send_with_retry<F, D>(&self, mut build_request: F, describe: D) -> Result<Response>
    where
        F: FnMut() -> RequestBuilder,
        D: Fn() -> String,
    {
        let mut attempt = 0u32;

        loop {
            match build_request().send() {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    if should_retry_response(&response) && attempt < self.max_retries {
                        sleep_for_retry(
                            response.headers(),
                            attempt,
                            self.retry_delay,
                            self.max_retry_delay,
                        );
                        attempt += 1;
                        continue;
                    }
                    return error_from_response(response, &describe());
                }
                Err(error) => {
                    if (error.is_timeout() || error.is_connect()) && attempt < self.max_retries {
                        thread::sleep(calculate_backoff(
                            self.retry_delay,
                            self.max_retry_delay,
                            attempt,
                        ));
                        attempt += 1;
                        continue;
                    }
                    return Err(error).with_context(describe);
                }
            }
        }
    }
}

fn select_stable_version(metadata: &CrateMetadata) -> Option<String> {
    [metadata.latest_stable.as_deref(), metadata.latest.as_deref(), metadata.newest.as_deref()]
        .into_iter()
        .flatten()
        .find(|candidate| is_stable_version(candidate))
        .map(ToOwned::to_owned)
}

fn is_stable_version(candidate: &str) -> bool {
    Version::parse(candidate).is_ok_and(|version| version.pre.is_empty())
}

fn should_retry_response(response: &Response) -> bool {
    response.status() == StatusCode::TOO_MANY_REQUESTS || response.status().is_server_error()
}

fn sleep_for_retry(
    headers: &HeaderMap,
    attempt: u32,
    retry_delay: Duration,
    max_retry_delay: Duration,
) {
    let delay = retry_delay_from_headers(headers)
        .filter(|delay| *delay > Duration::ZERO && *delay <= max_retry_delay * 10)
        .unwrap_or_else(|| calculate_backoff(retry_delay, max_retry_delay, attempt));
    thread::sleep(delay);
}

fn calculate_backoff(retry_delay: Duration, max_retry_delay: Duration, attempt: u32) -> Duration {
    let shift = attempt.min(10);
    let candidate = retry_delay.saturating_mul(1u32 << shift);
    candidate.min(max_retry_delay)
}

fn retry_delay_from_headers(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn error_from_response(response: Response, context: &str) -> Result<Response> {
    let status = response.status();
    let body = response.text().unwrap_or_else(|_| String::from("<response body unavailable>"));

    if status == StatusCode::NOT_FOUND {
        bail!("{context}: crate not found ({body})");
    }

    bail!("{context}: crates.io API returned {status} ({body})")
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::time::Duration;

    use mockito::Server;

    use super::{CratesIoClient, CratesIoClientOptions};

    #[test]
    fn latest_stable_version_prefers_max_stable_version() {
        let mut server = Server::new();
        let _crate = server
            .mock("GET", "/crates/reqwest")
            .match_header("user-agent", "github-actions-maintainer")
            .with_status(200)
            .with_body(
                r#"{
                    "crate": {
                        "id": "reqwest",
                        "name": "reqwest",
                        "max_version": "0.14.0-beta.1",
                        "max_stable_version": "0.13.2",
                        "newest_version": "0.14.0-beta.1"
                    }
                }"#,
            )
            .create();

        let client = CratesIoClient::new(server.url()).expect("crates.io client");
        let latest = client.latest_stable_version("reqwest").expect("latest stable version");

        assert_eq!(latest, "0.13.2");
    }

    #[test]
    fn latest_stable_version_retries_after_rate_limit() {
        let mut server = Server::new();
        let _rate_limited = server
            .mock("GET", "/crates/serde")
            .expect(1)
            .match_header("user-agent", "github-actions-maintainer")
            .with_status(429)
            .with_header("retry-after", "0")
            .with_body(r#"{"errors":[{"detail":"slow down"}]}"#)
            .create();
        let _success = server
            .mock("GET", "/crates/serde")
            .expect(1)
            .match_header("user-agent", "github-actions-maintainer")
            .with_status(200)
            .with_body(
                r#"{
                    "crate": {
                        "id": "serde",
                        "name": "serde",
                        "max_version": "1.0.219",
                        "max_stable_version": "1.0.219",
                        "newest_version": "1.0.219"
                    }
                }"#,
            )
            .create();

        let client = CratesIoClient::with_options(&CratesIoClientOptions {
            base_url: server.url(),
            timeout: Duration::from_secs(5),
            max_retries: 1,
            retry_delay: Duration::from_millis(1),
            max_retry_delay: Duration::from_millis(5),
        })
        .expect("crates.io client");
        let latest = client.latest_stable_version("serde").expect("latest stable version");

        assert_eq!(latest, "1.0.219");
    }
}
