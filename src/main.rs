//! Finpilot — system update frontend for uupd.
//!
//! Entry point pattern for Bluefin utility apps:
//! 1. Initialize tracing (structured logging)
//! 2. Create an `adw::Application` via relm4 with a proper app ID
//! 3. Hand control to the relm4 component tree
//!
//! This pattern ensures:
//! - D-Bus activation works (app ID matches .desktop file)
//! - Single-instance behavior is enforced by GApplication
//! - libadwaita styles are loaded before any widgets are created

mod app;
mod config;
mod update_worker;
mod ui;

use app::App;

fn main() {
    // Initialize structured logging — respects RUST_LOG env var.
    // Default to "info" for release, "debug" for dev builds.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting Finpilot ({}) v{}", config::APP_ID, config::VERSION);

    // relm4::RelmApp handles:
    // - Creating the adw::Application (because we enabled the "libadwaita" feature)
    // - Calling adw::init() which loads Adwaita CSS and enables color scheme support
    // - Running the GLib main loop
    let app = relm4::RelmApp::new(config::APP_ID);
    app.run::<App>(());
}
