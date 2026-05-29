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

/// One sibling variant within an image family — e.g. `bluefin-nvidia` next
/// to `bluefin`. Yielded by [`RegistryClient::discover_variants`].
#[derive(Debug, Clone, PartialEq)]
pub struct VariantRef {
    /// Just the image name (no registry / org / tag), e.g. `"bluefin-nvidia"`.
    pub image: String,
    /// Human-readable label for chip rendering. Usually equal to `image`,
    /// but with title-casing applied (`"Bluefin-Nvidia"`).
    pub display_name: String,
    /// Full OCI ref for this variant at the current stream:
    /// `ghcr.io/ublue-os/bluefin-nvidia:stable`. Pass directly to `bootc switch`.
    pub full_ref: String,
}

/// One coherent product family — a user-facing concept that groups a set of
/// sibling image *names* (the GPU/hardware variants like `-nvidia`, `-dx`,
/// `-deck`) and the tag streams (channels) under which they're published.
///
/// A given GHCR image can belong to multiple families — Bluefin Stable and
/// Bluefin LTS, for instance, share the `ublue-os/bluefin` image but use
/// disjoint stream sets (`stable*` vs `lts*`).
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    /// Display name for menus / dropdowns: "Bluefin Stable", "Bluefin LTS".
    pub name: &'static str,
    /// Registry org owning every image in this family.
    pub org: &'static str,
    /// Sibling image names — what the rebase dialog's variant chips render.
    /// First entry is the canonical/default for chip rendering. Each entry
    /// resolves to `ghcr.io/{org}/{name}:{stream}` at rebase time.
    pub images: &'static [&'static str],
    /// Tag streams this family publishes under. The rebase / changelog UI
    /// can offer a stream picker. First entry is the canonical default.
    pub streams: &'static [&'static str],
}

/// Catalogue of Universal Blue + Project Bluefin product families.
///
/// Used by [`RegistryClient::discover_variants`] as the candidate set for HEAD
/// probes — GHCR's `/v2/_catalog` endpoint isn't available for anonymous reads,
/// so enumeration falls back to "try every well-known name and keep the hits".
///
/// **Add new families / variants here as Universal Blue ships them.** Source
/// of truth for the user-visible "family" concept across the app.
pub const KNOWN_FAMILIES: &[Family] = &[
    Family {
        name: "Bluefin Stable",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-nvidia-open",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-dx-nvidia-open",
            "bluefin-asus",
            "bluefin-asus-nvidia",
            "bluefin-surface",
            "bluefin-framework",
        ],
        streams: &["latest", "stable", "stable-daily", "beta", "gts"],
    },
    Family {
        name: "Bluefin LTS",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-gdx",
        ],
        streams: &["lts", "lts-hwe", "lts-amd64", "lts-arm64", "gdx"],
    },
    Family {
        name: "Aurora",
        org: "ublue-os",
        images: &[
            "aurora",
            "aurora-nvidia",
            "aurora-nvidia-open",
            "aurora-dx",
            "aurora-dx-nvidia",
            "aurora-dx-nvidia-open",
        ],
        streams: &["latest", "stable", "stable-daily", "beta"],
    },
    Family {
        name: "Bazzite KDE",
        org: "ublue-os",
        images: &[
            "bazzite",
            "bazzite-nvidia",
            "bazzite-nvidia-open",
            "bazzite-deck",
            "bazzite-deck-nvidia",
            "bazzite-asus",
            "bazzite-framework",
        ],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    Family {
        name: "Bazzite GNOME",
        org: "ublue-os",
        images: &["bazzite-gnome", "bazzite-gnome-nvidia"],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    Family {
        name: "ucore",
        org: "ublue-os",
        images: &["ucore", "ucore-hci", "ucore-zfs"],
        streams: &["stable", "testing", "latest"],
    },
    Family {
        name: "Bluefin Dakota",
        org: "projectbluefin",
        images: &["dakota", "dakota-nvidia", "dakota-dx", "dakota-dx-nvidia"],
        streams: &["latest"],
    },
];

impl Family {
    /// Find every family that contains `image` under `org`. An image can
    /// belong to more than one family (Bluefin's image is shared between
    /// Bluefin Stable and Bluefin LTS; the stream tells them apart).
    pub fn all_for_image(org: &str, image: &str) -> Vec<&'static Family> {
        KNOWN_FAMILIES
            .iter()
            .filter(|f| f.org == org && f.images.iter().any(|i| *i == image))
            .collect()
    }

    /// Pick the family that best matches an `(org, image, stream)` triple by
    /// preferring families whose streams contain `stream` exactly. Falls back
    /// to any family containing the image, then `None`.
    pub fn best_match(org: &str, image: &str, stream: &str) -> Option<&'static Family> {
        let candidates = Self::all_for_image(org, image);
        candidates
            .iter()
            .find(|f| f.streams.iter().any(|s| *s == stream))
            .copied()
            .or_else(|| candidates.first().copied())
    }

    /// The first image name is treated as the family's *base* — every other
    /// image in `images` is derived from it by adding feature suffixes.
    /// E.g. Bluefin Stable's base is "bluefin"; "bluefin-nvidia" is "bluefin"
    /// plus the {nvidia} feature; "bluefin-dx-nvidia" is base + {dx, nvidia}.
    pub fn base_image(&self) -> &'static str {
        self.images.first().copied().unwrap_or("")
    }

    /// Atomic feature suffixes available in this family — derived from the
    /// image names by splitting each non-base image's suffix on '-'. Powers
    /// the SwitchRow list in the rebase dialog: e.g. Bluefin Stable yields
    /// `["asus", "dx", "framework", "nvidia", "open", "surface"]`.
    ///
    /// The order is alphabetical for stable UI rendering. Not every
    /// combination is valid — call [`Family::select_image_for_features`] to
    /// resolve a switch state to a concrete image (returns `None` if no
    /// image in the family has that exact combination).
    pub fn available_features(&self) -> Vec<&'static str> {
        let base = self.base_image();
        let mut set: std::collections::BTreeSet<&'static str> = Default::default();
        for img in self.images {
            if *img == base {
                continue;
            }
            if let Some(suffix) = img.strip_prefix(&format!("{}-", base)) {
                for atom in suffix.split('-') {
                    set.insert(atom);
                }
            }
        }
        set.into_iter().collect()
    }

    /// Given a set of selected atomic features (`features`), find the image
    /// name in this family whose suffix is exactly that set.
    ///
    /// Returns `Some(image_name)` when the combination matches a published
    /// image (`"bluefin"` for `[]`, `"bluefin-nvidia"` for `["nvidia"]`,
    /// `"bluefin-dx-nvidia"` for `["dx", "nvidia"]`), or `None` if no image
    /// matches (e.g. `["open"]` alone — open driver requires nvidia).
    pub fn select_image_for_features(&self, features: &[&str]) -> Option<&'static str> {
        let base = self.base_image();
        if features.is_empty() {
            return self.images.iter().copied().find(|i| *i == base);
        }
        for img in self.images {
            if *img == base {
                continue;
            }
            let suffix = match img.strip_prefix(&format!("{}-", base)) {
                Some(s) => s,
                None => continue,
            };
            let mut have: Vec<&str> = suffix.split('-').collect();
            have.sort();
            let mut want: Vec<&str> = features.iter().copied().collect();
            want.sort();
            if have == want {
                return Some(img);
            }
        }
        None
    }
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
    pub fn stream(&self) -> &str { &self.stream }

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
    /// Discover sibling image variants in the same family by probing GHCR
    /// with concurrent HEAD requests against each candidate.
    ///
    /// Returns the variants that respond with HTTP 200 for
    /// `/v2/{org}/{image}/manifests/{stream}` — i.e. images that actually
    /// publish a manifest under the same channel we're currently on.
    /// Always includes the current image as the first result (so the chip
    /// list never looks broken if the network is flaky).
    ///
    /// GHCR's `/v2/_catalog` is not readable anonymously, so we lean on a
    /// static [`KNOWN_FAMILIES`] table for the candidate set.
    pub async fn discover_variants(&self) -> Vec<VariantRef> {
        let make_ref = |image: &str| VariantRef {
            image: image.to_string(),
            display_name: image
                .split('-')
                .map(|part| {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join("-"),
            full_ref: format!("{}/{}/{}:{}", self.registry, self.org, image, self.stream),
        };

        // Find the family group whose org matches AND which contains self.image,
        // preferring the one whose streams include self.stream so Bluefin Stable
        // and Bluefin LTS are correctly disambiguated even though they share
        // the `ublue-os/bluefin` image.
        let family = Family::best_match(&self.org, &self.image, &self.stream);

        let candidates: Vec<&str> = match family {
            Some(f) => f.images.iter().copied().collect(),
            None => return vec![make_ref(&self.image)], // unknown family
        };

        // Token first — anonymous HEAD probes still need the bearer.
        let token = match self.get_token().await {
            Ok(t) => t,
            Err(_) => return vec![make_ref(&self.image)],
        };

        let concurrency = 12;
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
        let registry = self.registry.clone();
        let org = self.org.clone();
        let stream = self.stream.clone();
        let client = self.client.clone();

        let futs = candidates.into_iter().map(|img| {
            let url = format!("https://{}/v2/{}/{}/manifests/{}", registry, org, img, stream);
            let client = client.clone();
            let token = token.clone();
            let permit = semaphore.clone();
            let img_owned = img.to_string();
            async move {
                let _p = permit.acquire().await.ok()?;
                let resp = client
                    .head(&url)
                    .bearer_auth(&token)
                    .header("Accept", "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json")
                    .send()
                    .await
                    .ok()?;
                if resp.status().is_success() {
                    Some(img_owned)
                } else {
                    None
                }
            }
        });

        let hits: Vec<String> = futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect();

        if hits.is_empty() {
            vec![make_ref(&self.image)]
        } else {
            hits.iter().map(|i| make_ref(i)).collect()
        }
    }

    pub async fn fetch_versions(&self, days: u32) -> Result<Vec<ImageVersion>, RegistryError> {
        let token = self.get_token().await?;
        let client = self.client.clone();

        // Fetch the full tag list.
        let tags_url = format!(
            "https://{}/v2/{}/{}/tags/list?n=1000",
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
        let mut candidate_tags: Vec<(NaiveDate, String)> = tag_resp
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

        // Sort by date DESC and cap at HISTORY_MAX, since the home page never
        // displays more. Measured against live GHCR: 16 parallel manifest
        // HEADs took ~8s for ublue-os/bluefin; 8 cuts that roughly in half
        // and removes a real freeze on every launch. If we ever start showing
        // more than 8 entries this needs to grow with it.
        const CANDIDATE_CAP: usize = 8;
        candidate_tags.sort_by(|a, b| b.0.cmp(&a.0));
        candidate_tags.truncate(CANDIDATE_CAP);

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
            "https://{}/v2/{}/{}/tags/list?n=1000",
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

/// Extract a `NaiveDate` from a dated image tag, accepting the four conventions
/// observed across the bootc image families we support:
///
/// 1. **Stream-suffixed** (Bluefin, Aurora):
///    `stable-daily-43.20260222`, `lts-hwe-20260224`, `latest.20260527`
///    → accepted for stream `"stable"`, `"lts"`, `"latest"` respectively (via
///      prefix match — see the prefix rule below).
///
/// 2. **Sub-revisioned** (Bazzite):
///    `testing-43.20260308.1`, `stable-43.20260301.2`
///    → trailing `.N` (1–4 digits) is treated as a build sub-revision and
///      stripped before the date extraction.
///
/// 3. **Stream-prefix match** (Bluefin, Aurora, Bazzite):
///    A tag like `stable-daily-43.20260527` is accepted when the caller asks
///    for stream `"stable"` — the prefix begins with `"stable-"`. This lets
///    callers ask for the broad channel ("stable") and get back any tagged
///    build in that family, regardless of the fully-qualified stream
///    (e.g. `stable-daily-43`, `stable-gts-42`).
///
/// 4. **Bare date** (Dakota):
///    `20260114` — 8 digits, no prefix. Accepted only when the caller asks
///    for stream `"latest"` or `""` (the implicit / pointer-tag streams).
///
/// Returns the parsed calendar date, or `None` if the tag doesn't match any
/// of these patterns or fails calendar validation (e.g. month 13).
fn parse_dated_tag(tag: &str, stream: &str) -> Option<NaiveDate> {
    // (4) Bare YYYYMMDD with no separator — accepted only for the implicit
    //     streams that don't qualify their dates.
    if (stream == "latest" || stream.is_empty())
        && tag.len() == 8
        && tag.chars().all(|c| c.is_ascii_digit())
    {
        return NaiveDate::parse_from_str(tag, "%Y%m%d").ok();
    }

    // (2) Strip an optional trailing build sub-revision `.N` (1-4 digits) so
    //     `testing-43.20260308.1` reduces to `testing-43.20260308`.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len()) && suffix.chars().all(|c| c.is_ascii_digit()) {
            // Only strip if doing so leaves a date-shaped tail. Otherwise
            // we'd corrupt something like `stable.20260527` (where the `.`
            // is the date separator, not a sub-revision).
            let candidate = &tag[..idx];
            if candidate.len() >= 8
                && candidate[candidate.len() - 8..]
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                candidate
            } else {
                tag
            }
        } else {
            tag
        }
    } else {
        tag
    };

    // (1)/(3) Find a trailing `-YYYYMMDD` or `.YYYYMMDD` on `base`, then
    //         check the prefix matches the requested stream.
    for sep in ['.', '-'] {
        if let Some(idx) = base.rfind(sep) {
            let date_str = &base[idx + 1..];
            if date_str.len() != 8 || !date_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let prefix = &base[..idx];

            // Stream match rule: prefix is exactly `stream`, or begins with
            // `stream.` / `stream-` (qualified channel: stable-daily-43 etc.).
            let stream_matches = prefix == stream
                || prefix.starts_with(&format!("{}.", stream))
                || prefix.starts_with(&format!("{}-", stream));
            if !stream_matches {
                continue;
            }

            if let Some(date) = NaiveDate::parse_from_str(date_str, "%Y%m%d").ok() {
                return Some(date);
            }
        }
    }

    None
}

/// Remove the trailing `.YYYYMMDD[.N]` or `-YYYYMMDD[.N]` from a tag to get the
/// fully-qualified stream prefix.
///
/// Examples:
///   `stable-daily-43.20260527`     → `Some("stable-daily-43")`
///   `testing-43.20260308.1`        → `Some("testing-43")`   (sub-revision stripped)
///   `lts-hwe-20260224`             → `Some("lts-hwe")`
///   `latest`                       → `None`                  (no date)
///   `20260114`                     → `None`                  (no stream embedded)
fn strip_date_suffix(tag: &str) -> Option<String> {
    // Strip optional trailing sub-revision `.N` (1-4 digits) before looking
    // for the date — matches the Bazzite convention.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len())
            && suffix.chars().all(|c| c.is_ascii_digit())
            // Only strip when what's left ends in 8 digits — otherwise we'd
            // turn `stable.20260527` into `stable.20260527` again incorrectly.
            && idx >= 8
            && tag[..idx].as_bytes()[idx - 8..idx].iter().all(|b| b.is_ascii_digit())
        {
            &tag[..idx]
        } else {
            tag
        }
    } else {
        tag
    };

    for sep in ['.', '-'] {
        if let Some(pos) = base.rfind(sep) {
            let suffix = &base[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(base[..pos].to_string());
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

    // ── parse_dated_tag: real-world per-family tag formats ─────────────────
    // Samples below are real tags pulled from GHCR on 2026-05-29 — see the
    // queries in the bring-up plan. Update if the upstream conventions change.

    /// Bluefin: `stable-daily-43.20260222` for stream `"stable"` (prefix match).
    #[test]
    fn parse_dated_tag_bluefin_stable_daily_dot() {
        let d = parse_dated_tag("stable-daily-43.20260222", "stable").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    /// Bluefin: `43-43.20260222` for stream `"43"` (exact prefix match).
    #[test]
    fn parse_dated_tag_bluefin_version_qualified_dot() {
        let d = parse_dated_tag("43-43.20260222", "43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    /// Bluefin LTS: `lts-hwe.20260224` for stream `"lts"` (prefix match).
    #[test]
    fn parse_dated_tag_bluefin_lts_hwe_dot() {
        let d = parse_dated_tag("lts-hwe.20260224", "lts").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 24).unwrap());
    }

    /// Bluefin LTS, dash variant: `lts-hwe-20260224` for stream `"lts"`.
    #[test]
    fn parse_dated_tag_bluefin_lts_hwe_dash() {
        let d = parse_dated_tag("lts-hwe-20260224", "lts").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 24).unwrap());
    }

    /// Bazzite: `testing-43.20260308.1` — sub-revision is stripped before
    /// extracting the date.
    #[test]
    fn parse_dated_tag_bazzite_sub_revision() {
        let d = parse_dated_tag("testing-43.20260308.1", "testing").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 3, 8).unwrap());
    }

    /// Bazzite: `testing-43.20260301` without sub-revision still works.
    #[test]
    fn parse_dated_tag_bazzite_no_sub_revision() {
        let d = parse_dated_tag("testing-43.20260301", "testing").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
    }

    /// Dakota: `latest.20260114` for stream `"latest"` (exact prefix).
    #[test]
    fn parse_dated_tag_dakota_latest_dot() {
        let d = parse_dated_tag("latest.20260114", "latest").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Dakota: bare `20260114` accepted when stream is "latest" (implicit).
    #[test]
    fn parse_dated_tag_dakota_bare_date() {
        let d = parse_dated_tag("20260114", "latest").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Bare date is also accepted when stream is empty (no qualifier).
    #[test]
    fn parse_dated_tag_bare_date_empty_stream() {
        let d = parse_dated_tag("20260114", "").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Bare date is REJECTED when stream is anything else: a tag like
    /// `20260114` doesn't belong in stream `"stable"` results.
    #[test]
    fn parse_dated_tag_bare_date_rejected_for_qualified_stream() {
        assert!(parse_dated_tag("20260114", "stable").is_none());
    }

    /// Cross-family contamination: a `gts-*` tag must not appear in `stable`
    /// results even if the date is valid.
    #[test]
    fn parse_dated_tag_rejects_other_family() {
        assert!(parse_dated_tag("gts-daily-42.20260527", "stable").is_none());
    }

    /// Sub-revision must be 1–4 digits; `testing-43.20260308.55555` would be
    /// a malformed tag.
    #[test]
    fn parse_dated_tag_rejects_long_sub_revision() {
        assert!(parse_dated_tag("testing-43.20260308.55555", "testing").is_none());
    }

    /// `stable.20260527` — the `.20260527` is the date separator, not a
    /// sub-revision. The sub-revision stripper must not over-fire here.
    #[test]
    fn parse_dated_tag_does_not_strip_date_as_sub_revision() {
        let d = parse_dated_tag("stable.20260527", "stable").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 5, 27).unwrap());
    }

    // ── strip_date_suffix: sub-revisions and bare dates ─────────────────

    #[test]
    fn strip_date_suffix_strips_sub_revision() {
        assert_eq!(
            strip_date_suffix("testing-43.20260308.1"),
            Some("testing-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_bare_date_returns_none() {
        // Bare date has no stream prefix to return.
        assert_eq!(strip_date_suffix("20260114"), None);
    }

    // ── Family taxonomy disambiguation ──────────────────────────────────

    #[test]
    fn family_best_match_disambiguates_bluefin_stable_vs_lts_by_stream() {
        // The image `ublue-os/bluefin` belongs to both Bluefin Stable and
        // Bluefin LTS. The stream picks which family the user is on.
        let stable = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        assert_eq!(stable.name, "Bluefin Stable");

        let lts = Family::best_match("ublue-os", "bluefin", "lts").unwrap();
        assert_eq!(lts.name, "Bluefin LTS");

        let lts_hwe = Family::best_match("ublue-os", "bluefin", "lts-hwe").unwrap();
        assert_eq!(lts_hwe.name, "Bluefin LTS");
    }

    #[test]
    fn family_best_match_falls_back_to_first_when_stream_unknown() {
        // Unknown stream → first family containing the image wins.
        let f = Family::best_match("ublue-os", "bluefin", "moonshot-fictional").unwrap();
        // Bluefin Stable is declared first in KNOWN_FAMILIES.
        assert_eq!(f.name, "Bluefin Stable");
    }

    #[test]
    fn family_best_match_finds_aurora_by_image_alone() {
        let f = Family::best_match("ublue-os", "aurora", "stable").unwrap();
        assert_eq!(f.name, "Aurora");
        assert!(f.images.contains(&"aurora-nvidia"));
    }

    #[test]
    fn family_best_match_finds_bazzite_gnome_separately_from_kde() {
        let kde = Family::best_match("ublue-os", "bazzite", "stable").unwrap();
        assert_eq!(kde.name, "Bazzite KDE");

        let gnome = Family::best_match("ublue-os", "bazzite-gnome", "stable").unwrap();
        assert_eq!(gnome.name, "Bazzite GNOME");
    }

    #[test]
    fn family_best_match_returns_none_for_unknown_image() {
        assert!(Family::best_match("ublue-os", "totally-fake-image", "stable").is_none());
    }

    // ── Family feature switches ─────────────────────────────────────────

    #[test]
    fn family_base_image_is_first_in_list() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        assert_eq!(bluefin.base_image(), "bluefin");
        let dakota = Family::best_match("projectbluefin", "dakota", "latest").unwrap();
        assert_eq!(dakota.base_image(), "dakota");
    }

    #[test]
    fn family_available_features_lists_atomic_suffixes() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        let feats = bluefin.available_features();
        // From images like bluefin-nvidia / bluefin-nvidia-open / bluefin-dx /
        // bluefin-dx-nvidia / bluefin-dx-nvidia-open / bluefin-asus / etc.
        assert!(feats.contains(&"nvidia"));
        assert!(feats.contains(&"open"));
        assert!(feats.contains(&"dx"));
        assert!(feats.contains(&"asus"));
        assert!(feats.contains(&"surface"));
        assert!(feats.contains(&"framework"));
        // Alphabetical for stable UI rendering.
        let mut sorted = feats.clone();
        sorted.sort();
        assert_eq!(feats, sorted);
    }

    #[test]
    fn family_select_image_for_features_resolves_combinations() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();

        // Empty features → base.
        assert_eq!(bluefin.select_image_for_features(&[]), Some("bluefin"));
        // Single feature.
        assert_eq!(
            bluefin.select_image_for_features(&["nvidia"]),
            Some("bluefin-nvidia")
        );
        assert_eq!(bluefin.select_image_for_features(&["dx"]), Some("bluefin-dx"));
        // Two features, order-independent.
        assert_eq!(
            bluefin.select_image_for_features(&["dx", "nvidia"]),
            Some("bluefin-dx-nvidia")
        );
        assert_eq!(
            bluefin.select_image_for_features(&["nvidia", "dx"]),
            Some("bluefin-dx-nvidia")
        );
        // Three features — Bluefin Stable ships bluefin-dx-nvidia-open.
        assert_eq!(
            bluefin.select_image_for_features(&["dx", "nvidia", "open"]),
            Some("bluefin-dx-nvidia-open")
        );
    }

    #[test]
    fn family_select_image_for_features_returns_none_for_invalid_combo() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        // "open" alone (without nvidia) doesn't map to a published image.
        assert!(bluefin.select_image_for_features(&["open"]).is_none());
        // "dx" + "framework" isn't a real combination.
        assert!(bluefin
            .select_image_for_features(&["dx", "framework"])
            .is_none());
    }

    #[test]
    fn family_select_image_for_dakota_features() {
        let dakota = Family::best_match("projectbluefin", "dakota", "latest").unwrap();
        assert_eq!(dakota.select_image_for_features(&[]), Some("dakota"));
        assert_eq!(
            dakota.select_image_for_features(&["nvidia"]),
            Some("dakota-nvidia")
        );
    }

    #[test]
    fn family_all_for_image_returns_both_bluefin_families() {
        let families = Family::all_for_image("ublue-os", "bluefin");
        let names: Vec<&str> = families.iter().map(|f| f.name).collect();
        assert!(names.contains(&"Bluefin Stable"));
        assert!(names.contains(&"Bluefin LTS"));
    }

    #[test]
    fn strip_date_suffix_does_not_strip_non_date_as_sub_revision() {
        // `stable.20260527` is `stream.date`, not `stream.sub-revision`.
        assert_eq!(
            strip_date_suffix("stable.20260527"),
            Some("stable".to_string())
        );
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
