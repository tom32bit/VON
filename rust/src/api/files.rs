//! Sandbox Explorer and project export (PRD Page 5).

use crate::api::types::{
    flatten_tree,
    FileEntryDto,
    SandboxUsageDto,
    VonException,
};
use crate::sandbox;
use crate::state;

/// The file tree, flattened in display order with a depth for indenting.
pub fn file_tree(project_id: String) -> Result<Vec<FileEntryDto>, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    let tree = sb.tree().map_err(VonException::from)?;
    let mut out = Vec::new();
    flatten_tree(&tree, 0, &mut out);
    Ok(out)
}

/// Reads a text file for the editor pane.
pub fn read_project_file(project_id: String, path: String) -> Result<String, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    sb.read_file(&path).map_err(VonException::from)
}

/// Saves a user edit. Goes through the same validation and quota checks the
/// agent's own writes do.
pub fn write_project_file(
    project_id: String,
    path: String,
    content: String,
) -> Result<i64, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    let bytes = sb.write_file(&path, &content).map_err(VonException::from)?;
    let usage = sb.usage().map_err(VonException::from)?;
    let _ = crate::db::touch_project(&project_id, usage.total_bytes as i64);
    Ok(bytes as i64)
}

pub fn delete_project_file(project_id: String, path: String) -> Result<(), VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    sb.delete(&path).map_err(VonException::from)
}

pub fn sandbox_usage(project_id: String) -> Result<SandboxUsageDto, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    let usage = sb.usage().map_err(VonException::from)?;
    Ok(SandboxUsageDto {
        total_bytes: usage.total_bytes as i64,
        file_count: usage.file_count as i32,
        max_bytes: sandbox::MAX_SANDBOX_BYTES as i64,
        max_files: sandbox::MAX_SANDBOX_FILES as i32,
    })
}

/// Zips the sandbox for the "Download Project" button and returns the archive's
/// path, which Dart hands to the platform share sheet.
pub fn export_project(project_id: String) -> Result<String, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    let project = crate::db::get_project(&project_id).map_err(VonException::from)?;

    // Build a filename that is safe on every filesystem the share sheet might
    // hand it to, and readable when it lands in the user's Downloads.
    let slug: String = project
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "project".to_string()
    } else {
        slug.chars().take(48).collect()
    };

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dest = state::paths()
        .map_err(VonException::from)?
        .exports
        .join(format!("{slug}-{stamp}.zip"));

    sb.export_zip(&dest).map_err(VonException::from)?;
    Ok(dest.to_string_lossy().to_string())
}

/// Removes previously exported archives — they are duplicates of data the app
/// already holds and add up quickly.
pub fn clear_exports() -> Result<i32, VonException> {
    let dir = &state::paths().map_err(VonException::from)?.exports;
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("zip")
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    Ok(removed)
}
