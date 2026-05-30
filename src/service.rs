//! Backend trait for finupdate operations — the seam between the GTK frontend
//! and the underlying bootc/registry/uupd machinery.
//!
//! Goal: give alternative frontends (CLI, TUI, web/D-Bus daemon, integration
//! tests) a single point of contact that has no GTK or relm4 dependencies. The
//! trait is intentionally narrow — it covers what the existing UI already does
//! today, no speculative additions. Things like "search for an arbitrary image"
//! belong in a follow-up once a real consumer needs them.
//!
//! ## Design choices
//!
//! - **Async fns under #[async_trait]** for the one-shot operations. Native
//!   async fn in trait would be nicer but `Box<dyn UpdaterService>` (which the
//!   frontends will want) requires `#[async_trait]` until Rust stabilises the
//!   async-fn-in-dyn-trait path. Cost: one boxed Future per call, acceptable
//!   for an interactive app.
//!
//! - **Streaming operations expose `tokio::sync::mpsc::Receiver`** rather than
//!   `impl Stream`. The orchestrator already uses mpsc, so a wrapper that goes
//!   `impl Stream` is just bookkeeping. The receiver pattern lets a GTK
//!   frontend poll via `glib::timeout_add_local` and a CLI frontend iterate
//!   with `while let Some(ev) = rx.recv().await`, both without pulling tokio
//!   runtime into the trait surface.
//!
//! - **Plain data types in/out**. `ImageRef`, `ImageVersion`, `FamilyInfo`,
//!   `Changelog`, `*Progress` are all owned, serde-friendly structs. No GTK
//!   widgets, no `Rc<RefCell<>>`, no GTK signals. A web frontend could
//!   JSON-serialise the entire trait surface.
//!
//! ## Migration plan
//!
//! Today this module ships the trait, types, and a `BootcUpdaterService` that
//! re-exports the existing free functions in `registry_client`, `orchestrator`,
//! `update_worker`, etc. The GTK UI keeps calling those free functions
//! directly for now. As each UI site gets refactored to take a
//! `&dyn UpdaterService`, the equivalent free function gets retired.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

// Re-export the existing concrete types so frontends only depend on this
// module. When/if registry_client gets split into a separate crate, these
// re-exports change to use the new path — single point of update.
pub use crate::registry_client::ImageVersion;

/// A reference to an OCI image as `registry/org/image:tag`.
///
/// Mirrors the canonical bootc spec format. Frontends construct one of these
/// from user input or from a previous `current_image()` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ImageRef {
    pub registry: String,
    pub org: String,
    pub image: String,
    pub tag: String,
}

impl ImageRef {
    /// Build from a `registry/org/image:tag` string. Returns None on malformed
    /// input. Symmetric with `to_string()`.
    pub fn parse(s: &str) -> Option<Self> {
        let (before_tag, tag) = s.rsplit_once(':')?;
        let mut parts = before_tag.splitn(3, '/');
        let registry = parts.next()?.to_string();
        let org = parts.next()?.to_string();
        let image = parts.next()?.to_string();
        Some(Self {
            registry,
            org,
            image,
            tag: tag.to_string(),
        })
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}/{}:{}", self.registry, self.org, self.image, self.tag)
    }
}

impl std::fmt::Display for ImageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_string())
    }
}

/// Information about a Family that's relevant to picking a target image.
///
/// Frontends use this to render the feature switches (one switch per entry in
/// `features`) and to display the family name. The base image and feature
/// list are enough for `resolve_target()` to compute the concrete target ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FamilyInfo {
    pub name: String,
    pub base_image: String,
    pub features: Vec<Feature>,
}

/// A switchable feature for the booted family — e.g. NVIDIA, DX, Steam Deck.
///
/// `id` is the atomic suffix used by `resolve_target` (e.g. `"nvidia"`).
/// `display_name` and `subtitle` are human-readable strings for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Feature {
    pub id: String,
    pub display_name: String,
    pub subtitle: String,
}

/// Errors a Service operation can return.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("registry: {0}")]
    Registry(String),
    #[error("io: {0}")]
    Io(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// Progress events streamed during a long-running `switch_image` operation.
///
/// The terminal events are `Done` and `Failed`. Receivers should treat the
/// channel as closed after either of those — `recv()` returning None after
/// `Done` is normal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwitchProgress {
    /// Initial state — the operation has been accepted and is pulling.
    Started { target: String },
    /// Optional per-layer progress (when we can parse it from bootc stdout).
    Layer {
        index: u32,
        total: u32,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// A raw log line from the underlying tool, when no structured signal
    /// could be extracted. Frontends can show this in a "details" pane.
    Log { line: String },
    /// Operation finished successfully — the new image is staged for next boot.
    Done,
    /// Operation failed.
    Failed { message: String },
}

/// The minimal backend surface every finupdate frontend needs.
///
/// Implementations must be `Send + Sync` so they can be wrapped in `Arc<dyn>`
/// and shared between threads. The default `BootcUpdaterService` is a thin
/// stateless adapter over the existing registry/orchestrator helpers.
#[async_trait::async_trait]
pub trait UpdaterService: Send + Sync {
    /// Return the currently-booted image (respecting `mock_identity` for tests).
    async fn current_image(&self) -> Result<ImageRef, ServiceError>;

    /// Return the Family the current image belongs to, with its switchable
    /// features. None when the booted image isn't in the KNOWN_FAMILIES table.
    async fn current_family(&self) -> Result<Option<FamilyInfo>, ServiceError>;

    /// Return the most recent N versions of the given image, newest-first.
    /// Surfaces config-blob `created` timestamps for sha-only-tagged images.
    async fn list_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError>;

    /// Compute the target image ref for a family + selected features. Returns
    /// None if the combination doesn't match a published image.
    fn resolve_target(&self, family: &FamilyInfo, features: &[String]) -> Option<ImageRef>;

    /// Switch the booted system to a new image. Progress events stream on the
    /// returned receiver; the operation is complete when `Done` or `Failed`
    /// arrives and the channel closes.
    async fn switch_image(
        &self,
        target: &ImageRef,
    ) -> Result<mpsc::Receiver<SwitchProgress>, ServiceError>;
}

/// Default in-process implementation backed by the existing registry_client
/// and orchestrator modules. Constructed once at app startup and passed to
/// UI components as `Arc<dyn UpdaterService>`.
pub struct BootcUpdaterService;

impl BootcUpdaterService {
    pub fn new() -> Arc<dyn UpdaterService> {
        Arc::new(Self)
    }
}

impl Default for BootcUpdaterService {
    fn default() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl UpdaterService for BootcUpdaterService {
    async fn current_image(&self) -> Result<ImageRef, ServiceError> {
        let client = crate::registry_client::RegistryClient::detect()
            .await
            .ok_or_else(|| ServiceError::NotFound("no booted image detected".into()))?;
        Ok(ImageRef {
            registry: "ghcr.io".to_string(),
            org: client.org().to_string(),
            image: client.image().to_string(),
            tag: client.stream().to_string(),
        })
    }

    async fn current_family(&self) -> Result<Option<FamilyInfo>, ServiceError> {
        let Some(client) = crate::registry_client::RegistryClient::detect().await else {
            return Ok(None);
        };
        let Some(fam) = crate::registry_client::Family::best_match(
            client.org(),
            client.image(),
            client.stream(),
        ) else {
            return Ok(None);
        };
        let features = fam
            .available_features()
            .into_iter()
            .map(|feat| Feature {
                id: feat.to_string(),
                display_name: feature_display_name(feat).to_string(),
                subtitle: feature_subtitle(feat).to_string(),
            })
            .collect();
        Ok(Some(FamilyInfo {
            name: fam.name.to_string(),
            base_image: fam.base_image().to_string(),
            features,
        }))
    }

    async fn list_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError> {
        let client = crate::registry_client::RegistryClient::new(
            &image.registry,
            &image.org,
            &image.image,
            &image.tag,
        );
        let mut versions = client
            .fetch_versions(0)
            .await
            .map_err(|e| ServiceError::Registry(e.to_string()))?;
        versions.sort_by(|a, b| b.date.cmp(&a.date));
        versions.truncate(max);
        Ok(versions)
    }

    fn resolve_target(&self, family: &FamilyInfo, features: &[String]) -> Option<ImageRef> {
        // Find the matching static Family for the resolution helper. We do
        // not carry &'static Family across the trait boundary so callers (and
        // alternative frontends) only see plain data.
        let fam = crate::registry_client::KNOWN_FAMILIES
            .iter()
            .find(|f| f.name == family.name)?;
        let want: Vec<&str> = features.iter().map(|s| s.as_str()).collect();
        let target_image = fam.select_image_for_features(&want)?;
        Some(ImageRef {
            registry: "ghcr.io".to_string(),
            org: fam.org.to_string(),
            image: target_image.to_string(),
            tag: "stable".to_string(),
        })
    }

    async fn switch_image(
        &self,
        _target: &ImageRef,
    ) -> Result<mpsc::Receiver<SwitchProgress>, ServiceError> {
        // The orchestrator currently invokes pkexec bootc switch via
        // tokio::process and only surfaces an exit code. Wiring real
        // streaming progress here belongs in task #24 phase 2 (parse the
        // bootc layer-by-layer stdout). Until that lands we return
        // Unsupported so callers fall back to the existing direct path.
        Err(ServiceError::Unsupported(
            "switch_image streaming not yet implemented; use run_bootc_switch directly".into(),
        ))
    }
}

// Friendly-name maps duplicated from rebase_dialog.rs so the trait
// implementation doesn't depend on the UI module. When rebase_dialog
// migrates to consume FamilyInfo, the originals can be deleted.
fn feature_display_name(feat: &str) -> &'static str {
    match feat {
        "nvidia" => "NVIDIA drivers (proprietary)",
        "open" => "NVIDIA open kernel modules",
        "dx" => "Developer extras (DX)",
        "hwe" => "Hardware-enablement kernel (HWE)",
        "gdx" => "GNOME Developer extras (GDX)",
        "deck" => "Steam Deck profile",
        "asus" => "ASUS ROG tuning",
        "surface" => "Microsoft Surface kernel",
        "framework" => "Framework laptop tuning",
        _ => "Variant feature",
    }
}

fn feature_subtitle(feat: &str) -> &'static str {
    match feat {
        "nvidia" => "Use the closed-source NVIDIA driver",
        "open" => "Use Mesa's NVK / NVIDIA open-source kernel driver",
        "dx" => "Includes container tools, IDEs, and language SDKs",
        "hwe" => "Newer kernel + drivers backported for fresh hardware",
        "gdx" => "GNOME-focused developer toolchain",
        "deck" => "Tuned for Steam Deck hardware (gamescope, Steam shell)",
        "asus" => "Kernel patches for ASUS ROG Ally / Strix laptops",
        "surface" => "Linux-surface kernel + camera fix-ups",
        "framework" => "Power profiles + fingerprint reader support",
        _ => "Additional ublue extras",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_ref_parse_roundtrip() {
        let r = ImageRef::parse("ghcr.io/ublue-os/bluefin:stable-daily-43.20260527").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.org, "ublue-os");
        assert_eq!(r.image, "bluefin");
        assert_eq!(r.tag, "stable-daily-43.20260527");
        assert_eq!(
            r.as_string(),
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527"
        );
    }

    #[test]
    fn image_ref_parse_rejects_missing_tag() {
        assert!(ImageRef::parse("ghcr.io/ublue-os/bluefin").is_none());
    }

    #[test]
    fn image_ref_parse_rejects_missing_registry() {
        assert!(ImageRef::parse("ublue-os/bluefin:stable").is_none());
    }

    #[tokio::test]
    async fn resolve_target_routes_to_known_family() {
        let svc = BootcUpdaterService;
        let family = FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![],
        };
        let r = svc.resolve_target(&family, &["nvidia".to_string()]);
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.image, "bluefin-nvidia");
    }

    #[tokio::test]
    async fn resolve_target_returns_none_for_unpublished_combo() {
        let svc = BootcUpdaterService;
        let family = FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![],
        };
        // "open" alone (without nvidia) isn't a published Bluefin image.
        let r = svc.resolve_target(&family, &["open".to_string()]);
        assert!(r.is_none());
    }
}
