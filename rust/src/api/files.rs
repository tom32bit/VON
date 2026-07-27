//! Sandbox Explorer and project export (PRD Page 5).

use crate::api::types::{
    flatten_tree,
    FileEntryDto,
    SandboxUsageDto,
    VonException,
};
use crate::documents;
use crate::error::VonError;
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

/// Reads a file for the viewer pane.
///
/// A PDF or Word document comes back as extracted text so it can be read on
/// screen. Dart must treat those as read-only — saving the extraction back
/// would replace the document with a flattened copy of itself — which is what
/// [`is_document`] is for.
pub fn read_project_file(project_id: String, path: String) -> Result<String, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;
    if documents::needs_extraction(&path) {
        let bytes = sb.read_bytes(&path).map_err(VonException::from)?;
        return documents::extract(&path, &bytes).map_err(VonException::from);
    }
    sb.read_file(&path).map_err(VonException::from)
}

/// Whether this path is a document VON converts rather than reads directly.
///
/// Lives in Rust rather than being re-derived in Dart so there is one list of
/// extensions, and the viewer can never disagree with the reader about what is
/// editable.
pub fn is_document(path: String) -> Result<bool, VonException> {
    Ok(documents::needs_extraction(&path))
}

/// Copies a file from the phone into the sandbox.
///
/// The bytes arrive from Dart's file picker, which can hand back anything the
/// user can reach. `file_name` is treated as untrusted: only the last path
/// segment survives, and it still goes through `Sandbox::resolve` — a picker
/// returning `../../etc/passwd` is a path traversal attempt whether or not it
/// meant to be.
pub fn import_file(
    project_id: String,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<String, VonException> {
    let sb = state::sandbox_for(&project_id).map_err(VonException::from)?;

    let base = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('.')
        .to_string();
    if base.is_empty() {
        return Err(VonError::InvalidArgument("that file has no usable name".into()).into());
    }

    // Imports land together rather than scattered through the project, so the
    // agent can be told "your attachments are in uploads/" and the user can see
    // at a glance what came from outside.
    let mut target = format!("uploads/{base}");
    if sb.exists(&target) {
        target = unique_name(&sb, &base);
    }

    sb.write_bytes(&target, &bytes).map_err(VonException::from)?;
    let usage = sb.usage().map_err(VonException::from)?;
    let _ = crate::db::touch_project(&project_id, usage.total_bytes as i64);
    Ok(target)
}

/// Adds `-2`, `-3`… before the extension rather than overwriting.
///
/// Overwriting silently is the wrong default for an import: the user picked a
/// file, they did not ask to replace one, and the collision is usually two
/// different documents that happen to share a name.
fn unique_name(sb: &sandbox::Sandbox, base: &str) -> String {
    let (stem, ext) = match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (base, String::new()),
    };
    for n in 2..1000 {
        let candidate = format!("uploads/{stem}-{n}{ext}");
        if !sb.exists(&candidate) {
            return candidate;
        }
    }
    format!("uploads/{stem}-{}{ext}", uuid::Uuid::new_v4())
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
