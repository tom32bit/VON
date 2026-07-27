//! Localhost preview control (PRD Page 5, Tab 3).

use flutter_rust_bridge::frb;

use crate::api::types::VonException;
use crate::{server, state};

/// Starts (or returns the existing) preview server and gives Dart the URL to
/// point its `WebView` at.
pub fn start_preview(project_id: String) -> Result<String, VonException> {
    let sandbox = state::sandbox_for(&project_id).map_err(VonException::from)?;
    server::start(&project_id, sandbox).map_err(VonException::from)
}

/// Stops the server and frees its port. Called when the workspace closes.
pub fn stop_preview(project_id: String) -> Result<(), VonException> {
    server::stop(&project_id);
    Ok(())
}

/// The live URL, if a server is already running for this project.
#[frb(sync)]
pub fn preview_url(project_id: String) -> Option<String> {
    server::url_for(&project_id)
}

/// Shuts every preview down — used when the app is backgrounded.
pub fn stop_all_previews() -> Result<(), VonException> {
    server::stop_all();
    Ok(())
}
