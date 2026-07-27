//! Publishing a VON to GitHub, from Dart.
//!
//! The push is exposed as a stream rather than a plain future: uploading a
//! sandbox is several round trips and a phone's connection is not always quick,
//! so the UI shows what is happening instead of a spinner that could mean
//! anything.

use crate::api::types::VonException;
use crate::frb_generated::StreamSink;
use crate::github;
use crate::state;

/// The connected account, or empty strings when no token is stored.
///
/// Returns a DTO rather than an `Option` because frb turns `Option<Struct>`
/// into a nullable Dart type that every call site then has to unwrap; a
/// `connected` flag reads better in the widget.
#[derive(Debug, Clone)]
pub struct GitHubAccountDto {
    pub connected: bool,
    pub login: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct GitHubPushEventDto {
    /// `progress` | `done` | `failed`
    pub kind: String,
    /// Human-readable stage, for `progress`.
    pub detail: String,
    pub repo_full_name: String,
    pub branch: String,
    pub commit_sha: String,
    pub commit_url: String,
    pub files_pushed: i32,
    pub created_repo: bool,
    /// Files deliberately left out, each with its reason.
    pub skipped: Vec<String>,
    pub error_kind: String,
}

impl Default for GitHubPushEventDto {
    fn default() -> Self {
        Self {
            kind: String::new(),
            detail: String::new(),
            repo_full_name: String::new(),
            branch: String::new(),
            commit_sha: String::new(),
            commit_url: String::new(),
            files_pushed: 0,
            created_repo: false,
            skipped: Vec::new(),
            error_kind: String::new(),
        }
    }
}

/// Stores a personal access token after checking it works. An empty string
/// disconnects the account and forgets the token.
pub fn connect_github(token: String) -> Result<GitHubAccountDto, VonException> {
    match github::connect(&token).map_err(VonException::from)? {
        Some(account) => Ok(GitHubAccountDto {
            connected: true,
            login: account.login,
            name: account.name,
        }),
        None => Ok(GitHubAccountDto {
            connected: false,
            login: String::new(),
            name: String::new(),
        }),
    }
}

/// Whether a token is stored. Cheap and offline — no request is made, so the
/// settings screen can render before the network answers.
pub fn github_connected() -> Result<bool, VonException> {
    Ok(github::is_connected())
}

/// Resolves the stored token to an account. Hits the network, so the UI should
/// treat a failure here as "the token stopped working", not "not connected".
pub fn github_account() -> Result<GitHubAccountDto, VonException> {
    match github::account().map_err(VonException::from)? {
        Some(account) => Ok(GitHubAccountDto {
            connected: true,
            login: account.login,
            name: account.name,
        }),
        None => Ok(GitHubAccountDto {
            connected: false,
            login: String::new(),
            name: String::new(),
        }),
    }
}

/// Publishes a VON's sandbox as one commit, streaming progress.
///
/// `repo` is either `name` — created under the connected account if it does not
/// exist — or `owner/name`, which must already exist.
pub fn push_project_to_github(
    sink: StreamSink<GitHubPushEventDto>,
    project_id: String,
    repo: String,
    branch: String,
    message: String,
    private: bool,
) -> Result<(), VonException> {
    let sandbox = state::sandbox_for(&project_id).map_err(VonException::from)?;

    let outcome = github::push_sandbox(&sandbox, &repo, &branch, &message, private, |stage| {
        // A closed sink means the user navigated away; the push finishes
        // regardless, because a half-uploaded tree helps nobody.
        let _ = sink.add(GitHubPushEventDto {
            kind: "progress".into(),
            detail: stage.to_string(),
            ..Default::default()
        });
    });

    match outcome {
        Ok(result) => {
            let _ = sink.add(GitHubPushEventDto {
                kind: "done".into(),
                detail: format!(
                    "Pushed {} file{} to {}",
                    result.files_pushed,
                    if result.files_pushed == 1 { "" } else { "s" },
                    result.repo_full_name
                ),
                repo_full_name: result.repo_full_name,
                branch: result.branch,
                commit_sha: result.commit_sha,
                commit_url: result.commit_url,
                files_pushed: result.files_pushed as i32,
                created_repo: result.created_repo,
                skipped: result.skipped,
                ..Default::default()
            });
            Ok(())
        }
        Err(e) => {
            let exception = VonException::from(e);
            let _ = sink.add(GitHubPushEventDto {
                kind: "failed".into(),
                detail: exception.message.clone(),
                error_kind: exception.kind.clone(),
                ..Default::default()
            });
            Err(exception)
        }
    }
}
