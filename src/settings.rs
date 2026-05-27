//! Persistent application settings.
//!
//! Stored as JSON at `$XDG_CONFIG_HOME/finupdate/settings.json`.
//! Uses `gtk::glib::user_config_dir()` for correct XDG path resolution.

use gtk::glib;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How often the automatic update timer should fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateInterval {
    Hourly,
    Daily,
    Weekly,
    Custom,
}

impl UpdateInterval {
    /// Index into the UI combo-row model (Hourly=0, Daily=1, Weekly=2, Custom=3).
    pub fn to_index(&self) -> u32 {
        match self {
            Self::Hourly => 0,
            Self::Daily => 1,
            Self::Weekly => 2,
            Self::Custom => 3,
        }
    }

    pub fn from_index(i: u32) -> Self {
        match i {
            0 => Self::Hourly,
            1 => Self::Daily,
            2 => Self::Weekly,
            _ => Self::Custom,
        }
    }
}

/// All persistent user preferences for finupdate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whether the uupd systemd timer is allowed to run automatically.
    pub auto_updates: bool,
    /// How often the timer fires (informational — actual timer is managed by systemd).
    pub update_interval: UpdateInterval,
    /// Suppress automatic updates when on a metered/limited connection.
    pub pause_on_metered: bool,
    /// Custom interval in hours (only used when `update_interval == Custom`).
    pub custom_interval_hours: u32,
    /// Run a simulated update instead of the real uupd process.
    /// Useful for UI development and demos without root or a live system.
    pub dev_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_updates: true,
            update_interval: UpdateInterval::Daily,
            pause_on_metered: true,
            custom_interval_hours: 6,
            dev_mode: false,
        }
    }
}

impl Settings {
    fn config_path() -> PathBuf {
        glib::user_config_dir()
            .join("finupdate")
            .join("settings.json")
    }

    /// Load settings from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse settings (using defaults): {}", e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to disk, logging errors but never panicking.
    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!("Failed to create config directory: {}", e);
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&path, data) {
                    tracing::error!("Failed to write settings: {}", e);
                }
            }
            Err(e) => tracing::error!("Failed to serialize settings: {}", e),
        }
    }
}
