//! VON (project) CRUD and conversation history (PRD Page 3, Process A).
//!
//! A "VON" is the user-facing name for one isolated sandbox plus its agent
//! conversation. Internally it stays `project` — renaming the storage layer
//! would churn the schema for a branding decision, and every provider API this
//! talks to speaks in neutral terms anyway.

use crate::api::types::{MessageDto, ProjectDto, VonException};
use crate::error::VonError;
use crate::{db, llm, server, state};

/// Creates a VON and its isolated sandbox directory.
pub fn create_project(
    name: String,
    description: String,
    kind: String,
    provider: String,
    model_id: String,
) -> Result<ProjectDto, VonException> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(VonError::InvalidArgument("a VON needs a name".into()).into());
    }
    if name.chars().count() > 80 {
        return Err(VonError::InvalidArgument("that name is too long".into()).into());
    }
    // Validate the provider up front: a VON pinned to a provider that doesn't
    // exist would only fail later, mid-run.
    llm::Provider::parse(&provider).map_err(VonException::from)?;

    let kind = match kind.as_str() {
        "web" | "data" | "docs" => kind,
        _ => "general".to_string(),
    };

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    // Create the sandbox first — if the filesystem refuses, we don't want a
    // database row pointing at a directory that was never made.
    state::sandbox_for(&id).map_err(VonException::from)?;

    let row = db::ProjectRow {
        id: id.clone(),
        ordinal: db::claim_next_ordinal().map_err(VonException::from)?,
        name,
        description,
        kind,
        provider,
        model_id,
        created_at: now,
        updated_at: now,
        size_bytes: 0,
    };
    db::insert_project(&row).map_err(VonException::from)?;
    Ok(ProjectDto::from_row(row, 0))
}

pub fn list_projects() -> Result<Vec<ProjectDto>, VonException> {
    let rows = db::list_projects().map_err(VonException::from)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            // Count from disk rather than trusting a cached number: the agent
            // may have written files since the last update.
            let count = state::sandbox_for(&row.id)
                .and_then(|sb| sb.usage())
                .map(|u| u.file_count as i32)
                .unwrap_or(0);
            ProjectDto::from_row(row, count)
        })
        .collect())
}

pub fn get_project(project_id: String) -> Result<ProjectDto, VonException> {
    let row = db::get_project(&project_id).map_err(VonException::from)?;
    let count = state::sandbox_for(&project_id)
        .and_then(|sb| sb.usage())
        .map(|u| u.file_count as i32)
        .unwrap_or(0);
    Ok(ProjectDto::from_row(row, count))
}

pub fn rename_project(project_id: String, name: String) -> Result<(), VonException> {
    let name = name.trim();
    if name.is_empty() {
        return Err(VonError::InvalidArgument("a VON needs a name".into()).into());
    }
    db::rename_project(&project_id, name).map_err(VonException::from)
}

/// Switches a project between models or providers mid-conversation.
///
/// This is the "start offline on the train, finish on GPT-4o at home" path: the
/// transcript and sandbox are untouched, so the next turn resumes with full
/// context on the new model.
pub fn set_project_model(
    project_id: String,
    provider: String,
    model_id: String,
) -> Result<(), VonException> {
    llm::Provider::parse(&provider).map_err(VonException::from)?;
    db::update_project_model(&project_id, &provider, &model_id).map_err(VonException::from)
}

/// Deletes the project, its conversation, and its entire sandbox.
pub fn delete_project(project_id: String) -> Result<(), VonException> {
    // Confirm it exists before deleting anything on disk.
    db::get_project(&project_id).map_err(VonException::from)?;

    server::stop(&project_id);
    state::request_cancel(&project_id);

    let sandbox = state::sandbox_for(&project_id).map_err(VonException::from)?;
    if sandbox.root().exists() {
        std::fs::remove_dir_all(sandbox.root())
            .map_err(|e| VonException::from(VonError::from(e)))?;
    }
    // Messages and memory cascade with the row.
    db::delete_project(&project_id).map_err(VonException::from)
}

pub fn list_messages(project_id: String) -> Result<Vec<MessageDto>, VonException> {
    let rows = db::list_messages(&project_id).map_err(VonException::from)?;
    Ok(rows.into_iter().map(MessageDto::from).collect())
}

/// Clears the conversation while keeping the files the agent already produced.
pub fn clear_conversation(project_id: String) -> Result<(), VonException> {
    db::clear_messages(&project_id).map_err(VonException::from)
}
