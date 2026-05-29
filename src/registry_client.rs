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

    pub fn registry(&self) -> &str { &self.registry }
    pub fn org(&self) -> &str { &self.org }
    pub fn image(&self) -> &str { &self.image }

    /// Detect the current image stream from the running system.
    ///
    /// Precedence:
    /// 1. `Settings::mock_identity` (test override — no subprocess, no network).
    /// 2. `bootc status --json` (most reliable on a real host).
    /// 3. `/etc/os-release` fallback (Flatpak-friendly via flatpak-spawn).
    pub async fn detect() -> Option<Self> {
        Self::detect_with_settings(&crate::settings::Settings::load()).await
    }

    /// Like [`Self::detect`], but reads the mock-identity override from the
    /// caller-supplied `Settings` instead of loading from disk. Lets tests
    /// (and any future preferences UI) drive detection without round-tripping
    /// through settings.json.
    pub async fn detect_with_settings(settings: &crate::settings::Settings) -> Option<Self> {
        println!("[debug] RegistryClient::detect_with_settings()");

        if let Some(mock) = settings.mock_identity.as_ref() {
            let stream = strip_date_suffix(&mock.tag).unwrap_or_else(|| mock.tag.clone());
            println!(
                "[debug] RegistryClient::detect_with_settings() mock_identity = {}/{}/{} stream={}",
                mock.registry, mock.org, mock.image, stream
            );
            return Some(Self::new(&mock.registry, &mock.org, &mock.image, &stream));
        }

        // Try bootc status --json for the most reliable answer.
        if let Some(client) = Self::detect_from_bootc().await {
            return Some(client);
        }
        // Fallback: parse os-release
        let fallback = Self::detect_from_os_release();
        println!("[debug] RegistryClient::detect() fallback os-release = {:?}", fallback.as_ref().map(|c| c.stream.clone()));
        fallback
    }

    async fn detect_from_bootc() -> Option<Self> {
        let cmd_name = if crate::update_worker::is_flatpak() {
            "flatpak-spawn --host bootc status --json"
        } else {
            "bootc status --json"
        };
        println!("[debug] RegistryClient::detect_from_bootc() running {}", cmd_name);
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

        println!("[debug] RegistryClient::detect_from_bootc() exit = {:?}", output.status);
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

    fn read_os_release_content() -> Option<String> {
        if crate::update_worker::is_flatpak() {
            let output = std::process::Command::new("flatpak-spawn")
                .args(["--host", "cat", "/etc/os-release"])
                .output()
                .ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        } else {
            std::fs::read_to_string("/etc/os-release").ok()
        }
    }

    pub fn detect_from_os_release() -> Option<Self> {
        if let Some(content) = Self::read_os_release_content() {
            let mut image_ref = None;
            let mut image_tag = None;
            let mut image_id = None;
            let mut version_id = None;
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("IMAGE_REF=") {
                    image_ref = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("IMAGE_TAG=") {
                    image_tag = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("IMAGE_ID=") {
                    image_id = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                    version_id = Some(v.trim_matches('"').to_string());
                }
            }

            if let Some(ref_str) = image_ref {
                let clean_ref = if let Some(pos) = ref_str.find("docker://") {
                    &ref_str[pos + 9..]
                } else {
                    &ref_str
                };
                let parts: Vec<&str> = clean_ref.split('/').collect();
                if parts.len() >= 3 {
                    let registry = parts[0];
                    let org = parts[1];
                    let image = parts[2..].join("/");
                    let tag = image_tag.unwrap_or_else(|| "latest".to_string());
                    let stream = strip_date_suffix(&tag).unwrap_or(tag);
                    return Some(Self::new(registry, org, &image, &stream));
                }
            }

            if let (Some(img), Some(ver)) = (image_id, version_id) {
                let org = if img.contains("dakota") || img.contains("bluefin") || img.contains("aurora") {
                    "projectbluefin"
                } else {
                    "ublue-os"
                };
                let stream = if ver == "latest" {
                    "latest".to_string()
                } else {
                    format!("stable-daily-{}", ver)
                };
                return Some(Self::new("ghcr.io", org, &img, &stream));
            }
        }
        None
    }

    /// Fetch all available versions for this stream in the last `days` days.
    ///
    /// - Round trip 1: tag list
    /// - Round trip 2…N: manifest HEADs, up to 12 concurrent
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
            // Fallback: no dated tags found — try fetching the latest tag directly.
            // This handles images like Dakota that only publish a :latest tag.
            let latest_tag = "latest";
            if tag_resp.tags.contains(&latest_tag.to_string()) {
                let today = Utc::now().date_naive();
                let url = format!(
                    "https://{}/v2/{}/{}/manifests/{}",
                    self.registry, self.org, self.image, latest_tag
                );
                let full_ref = format!(
                    "{}/{}/{}:{}",
                    self.registry, self.org, self.image, latest_tag
                );
                if let Some(version) = fetch_version(&client, &url, &token, today, full_ref).await {
                    return Ok(vec![version]);
                }
            }
            return Err(RegistryError::NoTags(self.stream.clone()));
        }

        // Fetch manifests concurrently with a limit of 12 — significantly
        // faster than sequential chunking because slow manifests don't block
        // the entire batch.
        let registry = self.registry.clone();
        let org = self.org.clone();
        let image = self.image.clone();
        let concurrency = 12;
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));

        let futs: Vec<_> = candidate_tags
            .into_iter()
            .map(|(date, tag)| {
                let url = format!(
                    "https://{}/v2/{}/{}/manifests/{}",
                    registry, org, image, tag
                );
                let full_ref = format!("{}/{}/{}:{}", registry, org, image, tag);
                let client = client.clone();
                let token = token.clone();
                let permit = semaphore.clone();
                async move {
                    let _permit = permit.acquire().await.ok();
                    fetch_version(&client, &url, &token, date, full_ref).await
                }
            })
            .collect();

        let mut versions: Vec<ImageVersion> = futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect();

        versions.sort_by_key(|v| v.date);
        Ok(versions)
    }

    /// Return the tags available for this image, organised for the tag selector:
    /// - non-dated "stream/channel" tags first (e.g. `latest`, `gts`)
    /// - then dated tags for this stream, newest-first (e.g. `latest-20260527`)
    pub async fn fetch_available_tags(&self) -> Result<Vec<String>, RegistryError> {
        let token = self.get_token().await?;
        let tags_url = format!(
            "https://{}/v2/{}/{}/tags/list",
            self.registry, self.org, self.image
        );
        let tag_resp: TagListResponse = self
            .client
            .get(&tags_url)
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;

        let mut stream_tags: Vec<String> = Vec::new();
        let mut dated: Vec<(NaiveDate, String)> = Vec::new();

        for tag in &tag_resp.tags {
            // Skip OCI digest references and suspiciously long tokens.
            if tag.starts_with("sha256:") || tag.len() > 80 {
                continue;
            }
            if let Some(date) = parse_dated_tag(tag, &self.stream) {
                dated.push((date, tag.clone()));
            } else if strip_date_suffix(tag).is_none() {
                // No date suffix → it's a stream / channel tag.
                stream_tags.push(tag.clone());
            }
        }

        stream_tags.sort();
        dated.sort_by(|a, b| b.0.cmp(&a.0));

        let mut result = stream_tags;
        result.extend(dated.into_iter().take(30).map(|(_, t)| t));
        Ok(result)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_date_suffix ────────────────────────────────────────────────

    #[test]
    fn strip_date_suffix_dot_form() {
        assert_eq!(
            strip_date_suffix("stable-daily-43.20260222"),
            Some("stable-daily-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_dash_form() {
        assert_eq!(
            strip_date_suffix("stable-daily-43-20260222"),
            Some("stable-daily-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_rejects_non_date_suffix() {
        assert_eq!(strip_date_suffix("latest"), None);
        assert_eq!(strip_date_suffix("stable-daily"), None);
        assert_eq!(strip_date_suffix("stable.notadate"), None);
    }

    #[test]
    fn strip_date_suffix_rejects_wrong_length() {
        assert_eq!(strip_date_suffix("stream-1234567"), None); // 7 digits
        assert_eq!(strip_date_suffix("stream-123456789"), None); // 9 digits
    }

    #[test]
    fn strip_date_suffix_rejects_non_digit_chars() {
        assert_eq!(strip_date_suffix("stream-2026022x"), None);
    }

    // ── parse_dated_tag ──────────────────────────────────────────────────

    #[test]
    fn parse_dated_tag_dot_separator() {
        let d = parse_dated_tag("stable-daily-43.20260222", "stable-daily-43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    #[test]
    fn parse_dated_tag_dash_separator() {
        let d = parse_dated_tag("stable-daily-43-20260222", "stable-daily-43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    #[test]
    fn parse_dated_tag_rejects_unrelated_tag() {
        assert!(parse_dated_tag("latest", "stable-daily-43").is_none());
        assert!(parse_dated_tag("dev-daily-20260222", "stable-daily").is_none());
    }

    #[test]
    fn parse_dated_tag_rejects_invalid_calendar_date() {
        // 2026-02-30 isn't a real date.
        assert!(parse_dated_tag("stable.20260230", "stable").is_none());
    }

    // ── parse_image_ref ──────────────────────────────────────────────────

    #[test]
    fn parse_image_ref_full_ghcr_with_dot_date() {
        let c = parse_image_ref("ghcr.io/ublue-os/bluefin:stable-daily-43.20260222").unwrap();
        assert_eq!(c.registry(), "ghcr.io");
        assert_eq!(c.org(), "ublue-os");
        assert_eq!(c.image(), "bluefin");
        assert_eq!(c.stream, "stable-daily-43");
    }

    #[test]
    fn parse_image_ref_full_ghcr_with_dash_date() {
        let c = parse_image_ref("ghcr.io/projectbluefin/dakota:latest-20260527").unwrap();
        assert_eq!(c.stream, "latest");
    }

    #[test]
    fn parse_image_ref_rejects_missing_org_or_image() {
        assert!(parse_image_ref("ghcr.io:tag").is_none()); // no slashes
        assert!(parse_image_ref("ghcr.io/org:tag").is_none()); // only 2 parts
    }

    #[test]
    fn parse_image_ref_rejects_tag_without_date() {
        assert!(parse_image_ref("ghcr.io/ublue-os/bluefin:latest").is_none());
    }

    #[test]
    fn parse_image_ref_handles_nested_image_path() {
        // Some registries use multi-segment image paths.
        let c = parse_image_ref(
            "ghcr.io/ublue-os/bluefin-dx/extras:stable-daily.20260222",
        ).unwrap();
        assert_eq!(c.image(), "bluefin-dx/extras");
    }
}
