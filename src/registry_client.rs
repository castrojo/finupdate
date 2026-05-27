//! GHCR registry client for querying historical image versions.
//!
//! Fetches the list of dated image tags from `ghcr.io`, filters to the last
//! `days` days, then retrieves OCI manifest annotations for each tag in
//! parallel to collect version metadata (build time, kernel, git revision).
//!
//! All network I/O is async (tokio). Callers run this on a background thread.
//!
//! ## Tag format
//!
//! Universal Blue images use the pattern:
//! ```text
//! {stream}-{YYYYMMDD}    e.g.  stable-daily-43-20260222
//! {stream}.{YYYYMMDD}    e.g.  stable-daily-43.20260222   (dot variant)
//! ```
//! Both separators are supported; the dot form is preferred.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use std::collections::HashMap;

// ── Public data types ─────────────────────────────────────────────────────────

/// Metadata for a single dated image build available for rebasing.
#[derive(Debug, Clone)]
pub struct ImageVersion {
    /// Calendar date the image was built (UTC, YYYYMMDD from the tag).
    pub date: NaiveDate,
    /// Full OCI image reference — pass this to `bootc switch`.
    pub full_ref: String,
    /// Human-readable version string from `org.opencontainers.image.version`.
    pub version: String,
    /// Kernel version from `ostree.linux` annotation.
    pub kernel: String,
    /// Short git commit hash (first 8 chars of `org.opencontainers.image.revision`).
    pub revision: String,
    /// Build timestamp from `org.opencontainers.image.created`.
    pub created: DateTime<Utc>,
}

/// Error type for registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("No dated tags found for stream '{0}'")]
    NoTags(String),
    #[error("Unable to detect current image — is bootc installed?")]
    NoCurrentImage,
}

// ── Internal GHCR API types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Deserialize)]
struct TagListResponse {
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestResponse {
    annotations: Option<HashMap<String, String>>,
}

// ── RegistryClient ────────────────────────────────────────────────────────────

/// Client for fetching dated image versions from GHCR.
pub struct RegistryClient {
    registry: String,
    org: String,
    image: String,
    /// Tag prefix for dated builds, e.g. `"stable-daily-43"`.
    stream: String,
    client: reqwest::Client,
}

impl RegistryClient {
    /// Create a client targeting the given image stream.
    ///
    /// `stream` is everything in the tag before the date, e.g. `"stable-daily-43"`.
    pub fn new(registry: &str, org: &str, image: &str, stream: &str) -> Self {
        Self {
            registry: registry.to_string(),
            org: org.to_string(),
            image: image.to_string(),
            stream: stream.to_string(),
            client: build_http_client(),
        }
    }

    /// Detect the current image stream from the running system.
    ///
    /// Tries `bootc status --json` first, then falls back to parsing
    /// `/run/host/etc/os-release` (Flatpak-friendly path).
    pub async fn detect() -> Option<Self> {
        // Try bootc status --json for the most reliable answer.
        if let Some(client) = Self::detect_from_bootc().await {
            return Some(client);
        }
        // Fallback: parse os-release
        Self::detect_from_os_release()
    }

    async fn detect_from_bootc() -> Option<Self> {
        let output = if crate::update_worker::is_flatpak() {
            tokio::process::Command::new("flatpak-spawn")
                .args(["--host", "bootc", "status", "--json"])
                .output()
                .await
                .ok()?
        } else {
            tokio::process::Command::new("bootc")
                .args(["status", "--json"])
                .output()
                .await
                .ok()?
        };

        if !output.status.success() {
            return None;
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

        // Navigate: .status.booted.image.image.image  → full ref string
        let image_ref = json
            .pointer("/status/booted/image/image/image")
            .or_else(|| json.pointer("/status/booted/image/image"))
            .and_then(|v| v.as_str())?;

        // image_ref = "ghcr.io/ublue-os/bluefin:stable-daily-43.20260222"
        parse_image_ref(image_ref)
    }

    fn detect_from_os_release() -> Option<Self> {
        let paths = ["/run/host/etc/os-release", "/etc/os-release"];
        for path in &paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                let mut image_id = None;
                let mut version_id = None;
                for line in content.lines() {
                    if let Some(v) = line.strip_prefix("IMAGE_ID=") {
                        image_id = Some(v.trim_matches('"').to_string());
                    } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                        version_id = Some(v.trim_matches('"').to_string());
                    }
                }
                if let (Some(img), Some(ver)) = (image_id, version_id) {
                    // Best-guess stream: "stable-daily-{version_id}"
                    let stream = format!("stable-daily-{}", ver);
                    return Some(Self::new("ghcr.io", "ublue-os", &img, &stream));
                }
            }
        }
        None
    }

    /// Fetch all available versions for this stream in the last `days` days.
    ///
    /// - Round trip 1: tag list
    /// - Round trip 2…N: manifest HEADs, up to 8 concurrent
    pub async fn fetch_versions(&self, days: u32) -> Result<Vec<ImageVersion>, RegistryError> {
        let token = self.get_token().await?;
        let client = self.client.clone();

        // Fetch the full tag list.
        let tags_url = format!(
            "https://{}/v2/{}/{}/tags/list",
            self.registry, self.org, self.image
        );
        let tag_resp: TagListResponse = client
            .get(&tags_url)
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;

        // Filter to dated tags for this stream within the window.
        let cutoff = Utc::now().date_naive() - chrono::Duration::days(days as i64);
        let candidate_tags: Vec<(NaiveDate, String)> = tag_resp
            .tags
            .iter()
            .filter_map(|tag| {
                let date = parse_dated_tag(tag, &self.stream)?;
                if date >= cutoff {
                    Some((date, tag.clone()))
                } else {
                    None
                }
            })
            .collect();

        if candidate_tags.is_empty() {
            return Err(RegistryError::NoTags(self.stream.clone()));
        }

        // Fetch manifests in parallel, capped at 8 concurrent requests.
        let registry = self.registry.clone();
        let org = self.org.clone();
        let image = self.image.clone();

        let chunk_size = 8;
        let mut versions = Vec::new();

        for chunk in candidate_tags.chunks(chunk_size) {
            let futures: Vec<_> = chunk
                .iter()
                .map(|(date, tag)| {
                    let url = format!(
                        "https://{}/v2/{}/{}/manifests/{}",
                        registry, org, image, tag
                    );
                    let full_ref = format!("{}/{}/{}:{}", registry, org, image, tag);
                    let client = client.clone();
                    let token = token.clone();
                    let date = *date;
                    async move { fetch_version(&client, &url, &token, date, full_ref).await }
                })
                .collect();

            let results = futures::future::join_all(futures).await;
            for result in results.into_iter().flatten() {
                versions.push(result);
            }
        }

        versions.sort_by_key(|v| v.date);
        Ok(versions)
    }

    async fn get_token(&self) -> Result<String, RegistryError> {
        let url = format!(
            "https://{}/token?scope=repository:{}/{}:pull&service={}",
            self.registry, self.org, self.image, self.registry
        );
        let resp: TokenResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.token)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Fetch one manifest and extract `ImageVersion` from OCI annotations.
async fn fetch_version(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    date: NaiveDate,
    full_ref: String,
) -> Option<ImageVersion> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .ok()?;

    let manifest: ManifestResponse = resp.json().await.ok()?;
    let ann = manifest.annotations?;

    let version = ann
        .get("org.opencontainers.image.version")
        .cloned()
        .unwrap_or_else(|| date.format("%Y%m%d").to_string());

    let kernel = ann.get("ostree.linux").cloned().unwrap_or_default();

    let revision = ann
        .get("org.opencontainers.image.revision")
        .map(|r| r.chars().take(8).collect())
        .unwrap_or_default();

    let created = ann
        .get("org.opencontainers.image.created")
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).unwrap().and_utc());

    Some(ImageVersion {
        date,
        full_ref,
        version,
        kernel,
        revision,
        created,
    })
}

/// Build a shared reqwest client with a reasonable timeout.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .unwrap_or_default()
}

/// Parse a full OCI ref like `ghcr.io/ublue-os/bluefin:stable-daily-43.20260222`
/// into a `RegistryClient` for that stream.
fn parse_image_ref(image_ref: &str) -> Option<RegistryClient> {
    // Format: registry/org/image:stream.date  OR  registry/org/image:stream-date
    let (without_tag, tag) = image_ref.rsplit_once(':')?;
    let parts: Vec<&str> = without_tag.splitn(3, '/').collect();
    if parts.len() < 3 {
        return None;
    }
    let (registry, org, image) = (parts[0], parts[1], parts[2]);

    // Strip the date suffix from the tag to get the stream prefix.
    let stream = strip_date_suffix(tag)?;

    Some(RegistryClient::new(registry, org, image, &stream))
}

/// Extract a `NaiveDate` from a tag like `stable-daily-43.20260222` or
/// `stable-daily-43-20260222`, given the expected stream prefix.
fn parse_dated_tag(tag: &str, stream: &str) -> Option<NaiveDate> {
    // Try dot separator: "stream.YYYYMMDD"
    let date_str = if let Some(rest) = tag.strip_prefix(&format!("{}.", stream)) {
        rest
    } else if let Some(rest) = tag.strip_prefix(&format!("{}-", stream)) {
        rest
    } else {
        return None;
    };

    // Validate it looks like YYYYMMDD (8 digits)
    if date_str.len() == 8 && date_str.chars().all(|c| c.is_ascii_digit()) {
        NaiveDate::parse_from_str(date_str, "%Y%m%d").ok()
    } else {
        None
    }
}

/// Remove the trailing `.YYYYMMDD` or `-YYYYMMDD` from a tag to get the stream.
fn strip_date_suffix(tag: &str) -> Option<String> {
    // Walk backward to find an 8-digit date suffix.
    let separators = ['.', '-'];
    for sep in &separators {
        if let Some(pos) = tag.rfind(*sep) {
            let suffix = &tag[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(tag[..pos].to_string());
            }
        }
    }
    None
}
