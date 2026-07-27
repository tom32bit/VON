//! Startup and app-wide settings.

use flutter_rust_bridge::frb;

use crate::api::types::VonException;
use crate::{db, llm, server, state};

/// Boots the core. Called once from Dart with the app's private directory
/// (`getApplicationSupportDirectory()`), before any other API call.
///
/// Idempotent: Flutter recreates its engine on some Android lifecycle
/// transitions while the Rust process keeps running, so this must tolerate
/// being called again.
pub fn init_app(app_dir: String) -> Result<(), VonException> {
    install_logging();

    state::init(std::path::Path::new(&app_dir)).map_err(VonException::from)?;
    let paths = state::paths().map_err(VonException::from)?;
    db::init(&paths.db_file).map_err(VonException::from)?;
    llm::catalog::seed_catalog().map_err(VonException::from)?;

    // The governor starts polling immediately: the Model Hub shows live device
    // stats before any project is open.
    crate::governor::start();

    tracing::info!("VON core {} ready at {app_dir}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

#[cfg(target_os = "android")]
fn install_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("VON"),
        );
    });
}

#[cfg(not(target_os = "android"))]
fn install_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "von=info".into()),
            )
            .try_init();
    });
}

#[frb(sync)]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Which optional engines this binary was built with, so the UI can hide
/// features a cloud-only build cannot provide.
#[frb(sync)]
pub fn build_features() -> Vec<String> {
    let mut features = Vec::new();
    if cfg!(feature = "local-llm") {
        features.push("local-llm".to_string());
    }
    if cfg!(feature = "wasm-exec") {
        features.push("wasm-exec".to_string());
    }
    if cfg!(feature = "python-exec") {
        features.push("python-exec".to_string());
    }
    features
}

/// Called when Android backgrounds the app.
///
/// Releases the two things that make the process an attractive target for the
/// low-memory killer: the resident model weights and any preview servers.
pub fn on_app_paused() -> Result<(), VonException> {
    // Not while the agent is working. Dropping a 1.7GB mmap only to fault it
    // straight back in is slow, spikes memory at exactly the wrong moment, and
    // makes the process a better low-memory-killer target rather than a worse
    // one — which is how switching apps mid-run ended up losing the run.
    if state::runs_in_flight() {
        return Ok(());
    }
    llm::local::release_model();
    server::stop_all();
    Ok(())
}

pub fn set_setting(key: String, value: String) -> Result<(), VonException> {
    db::set_setting(&key, &value).map_err(VonException::from)
}

pub fn get_setting(key: String) -> Result<Option<String>, VonException> {
    db::get_setting(&key).map_err(VonException::from)
}
