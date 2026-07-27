//! Publishing a VON's sandbox to GitHub.
//!
//! This talks to GitHub's Git Data API rather than speaking the git wire
//! protocol. The reason is not laziness: the alternative is linking libgit2
//! (a C dependency that has to be cross-compiled for four Android ABIs and
//! brings its own TLS stack) or gitoxide (whose push support is not there
//! yet). The Git Data API is plain HTTPS over the client we already have, and
//! it produces exactly the same object graph — real blobs, a real tree, a real
//! commit, a real ref update.
//!
//! A push is one commit containing the sandbox as it stands. The tree is built
//! from scratch rather than layered on the previous one, so a file the agent
//! deleted disappears from the repo instead of lingering forever. There is no
//! local `.git`, no index, and no merge: a VON is a working directory that
//! happens to be publishable, not a clone.
//!
//! ## Token handling
//!
//! The personal access token lives in the same private SQLite table as the
//! model provider keys, under the pseudo-provider `github`. It is never logged,
//! never returned to Dart, and never sent anywhere except `api.github.com`.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Result, VonError};
use crate::sandbox::Sandbox;
use crate::state;

const API: &str = "https://api.github.com";

/// GitHub rejects requests without one, and the docs ask that it identify the
/// application rather than a browser.
const USER_AGENT: &str = concat!("VON/", env!("CARGO_PKG_VERSION"));

/// The pseudo-provider under which the token is stored. Not an `llm::Provider`,
/// because GitHub is not a model provider — it just shares the table.
pub const KEY_SLOT: &str = "github";

/// Files above this go up as-is but are worth naming, because GitHub rejects
/// blobs over 100 MB outright and starts warning at 50.
const LARGE_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Paths that should never leave the device, matched against any path segment.
///
/// A VON is a scratch directory and the agent writes into it freely; publishing
/// is the one moment that content becomes public, so the filter belongs here
/// rather than in the sandbox.
const NEVER_PUBLISH: &[&str] = &[
    ".git",
    ".env",
    ".env.local",
    "node_modules",
    "__pycache__",
    ".DS_Store",
    "id_rsa",
    "id_ed25519",
    ".netrc",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushOutcome {
    pub repo_full_name: String,
    pub branch: String,
    pub commit_sha: String,
    pub commit_url: String,
    pub files_pushed: u32,
    pub created_repo: bool,
    /// Files left behind and why — surfaced rather than swallowed, so a missing
    /// file in the repo is never a silent surprise.
    pub skipped: Vec<String>,
}

/// The stored token, or a `MissingApiKey` error naming what to do about it.
pub fn token() -> Result<String> {
    crate::db::get_api_key(KEY_SLOT)?
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| VonError::MissingApiKey(KEY_SLOT.into()))
}

pub fn is_connected() -> bool {
    token().is_ok()
}

/// Stores a token after proving it works. An empty string disconnects.
///
/// Validating before storing matters more here than for a model key: a bad
/// model key fails on the next message, but a bad GitHub token would fail
/// halfway through a push, after blobs had already been uploaded.
pub fn connect(raw: &str) -> Result<Option<Account>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        crate::db::set_api_key(KEY_SLOT, "")?;
        return Ok(None);
    }
    let account = fetch_account(trimmed)?;
    crate::db::set_api_key(KEY_SLOT, trimmed)?;
    Ok(Some(account))
}

/// The connected account, or `None` when no token is stored.
pub fn account() -> Result<Option<Account>> {
    match token() {
        Ok(t) => fetch_account(&t).map(Some),
        Err(VonError::MissingApiKey(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn fetch_account(token: &str) -> Result<Account> {
    let body = request(token, reqwest::Method::GET, "/user", None)?;
    let login = body
        .get("login")
        .and_then(Value::as_str)
        .ok_or_else(|| VonError::Other("GitHub did not return an account for this token".into()))?;
    Ok(Account {
        login: login.to_string(),
        name: body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(login)
            .to_string(),
    })
}

/// Publishes the sandbox as a single commit.
///
/// `repo` may be `name` (created under the token's own account) or
/// `owner/name` (which must already exist — VON will not create repositories
/// inside an organisation on your behalf).
pub fn push_sandbox(
    sandbox: &Sandbox,
    repo: &str,
    branch: &str,
    message: &str,
    private: bool,
    mut on_progress: impl FnMut(&str),
) -> Result<PushOutcome> {
    let token = token()?;
    let branch = if branch.trim().is_empty() {
        "main"
    } else {
        branch.trim()
    };
    let message = if message.trim().is_empty() {
        "Update from VON"
    } else {
        message.trim()
    };

    on_progress("Collecting files");
    let Payload { files, skipped } = collect(sandbox)?;
    if files.is_empty() {
        return Err(VonError::InvalidArgument(
            "this VON has no files to publish yet".into(),
        ));
    }

    on_progress("Resolving repository");
    let (full_name, created_repo) = ensure_repo(&token, repo, private)?;
    let (owner, name) = split_repo(&full_name)?;

    // A repository created a moment ago has no commits, so there is no ref to
    // read and the commit we build is a root commit.
    let parent = head_commit(&token, &owner, &name, branch)?;

    let mut tree_entries = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        on_progress(&format!("Uploading {} of {}", index + 1, files.len()));
        let blob = request(
            &token,
            reqwest::Method::POST,
            &format!("/repos/{owner}/{name}/git/blobs"),
            Some(json!({
                "content": base64::engine::general_purpose::STANDARD.encode(&file.bytes),
                "encoding": "base64",
            })),
        )?;
        let sha = blob
            .get("sha")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                VonError::Other(format!("GitHub stored '{}' but returned no sha", file.path))
            })?;
        tree_entries.push(json!({
            "path": file.path,
            // 100644 for everything: a sandbox has no executables to speak of
            // and no way to record a mode, so claiming one would be a lie.
            "mode": "100644",
            "type": "blob",
            "sha": sha,
        }));
    }

    on_progress("Building the commit");
    // Deliberately no `base_tree`: this tree is the whole VON, so a file the
    // agent deleted is absent here and therefore deleted in the repo too.
    let tree = request(
        &token,
        reqwest::Method::POST,
        &format!("/repos/{owner}/{name}/git/trees"),
        Some(json!({ "tree": tree_entries })),
    )?;
    let tree_sha = tree
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| VonError::Other("GitHub returned a tree with no sha".into()))?;

    let mut commit_body = json!({ "message": message, "tree": tree_sha });
    if let Some(parent) = &parent {
        commit_body["parents"] = json!([parent]);
    }
    let commit = request(
        &token,
        reqwest::Method::POST,
        &format!("/repos/{owner}/{name}/git/commits"),
        Some(commit_body),
    )?;
    let commit_sha = commit
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| VonError::Other("GitHub returned a commit with no sha".into()))?
        .to_string();

    on_progress("Updating the branch");
    if parent.is_some() {
        // `force` because the tree is authoritative: VON has no local history
        // to fast-forward from, and refusing here would leave the user with a
        // commit object that no branch points at.
        request(
            &token,
            reqwest::Method::PATCH,
            &format!("/repos/{owner}/{name}/git/refs/heads/{branch}"),
            Some(json!({ "sha": commit_sha, "force": true })),
        )?;
    } else {
        request(
            &token,
            reqwest::Method::POST,
            &format!("/repos/{owner}/{name}/git/refs"),
            Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": commit_sha })),
        )?;
    }

    Ok(PushOutcome {
        commit_url: format!("https://github.com/{full_name}/commit/{commit_sha}"),
        repo_full_name: full_name,
        branch: branch.to_string(),
        commit_sha,
        files_pushed: files.len() as u32,
        created_repo,
        skipped,
    })
}

/// One file on its way up.
struct PublishFile {
    path: String,
    bytes: Vec<u8>,
}

/// What a push will contain, and what it deliberately will not.
struct Payload {
    files: Vec<PublishFile>,
    skipped: Vec<String>,
}

/// Reads the sandbox into memory, dropping what must not be published.
///
/// Everything is held at once because the tree has to be posted as a unit;
/// sandbox quotas keep that bounded to something a phone can hold.
fn collect(sandbox: &Sandbox) -> Result<Payload> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();

    let mut paths = sandbox.list_flat()?;
    paths.sort();

    for path in paths {
        if let Some(segment) = path
            .split('/')
            .find(|segment| NEVER_PUBLISH.contains(segment))
        {
            skipped.push(format!("{path} (contains '{segment}')"));
            continue;
        }
        let bytes = sandbox.read_bytes(&path)?;
        if bytes.len() as u64 > LARGE_FILE_BYTES {
            skipped.push(format!(
                "{path} ({} MB — GitHub rejects blobs this large)",
                bytes.len() / (1024 * 1024)
            ));
            continue;
        }
        files.push(PublishFile { path, bytes });
    }
    Ok(Payload { files, skipped })
}

/// Returns `(owner/name, created)`, creating the repository if it is the user's
/// own and does not exist yet.
fn ensure_repo(token: &str, repo: &str, private: bool) -> Result<(String, bool)> {
    let repo = repo.trim().trim_matches('/');
    if repo.is_empty() {
        return Err(VonError::InvalidArgument(
            "a repository name is required".into(),
        ));
    }

    let (owner, name) = match repo.split_once('/') {
        Some((owner, name)) => (owner.to_string(), name.to_string()),
        None => (fetch_account(token)?.login, repo.to_string()),
    };
    validate_repo_name(&name)?;

    match request(
        token,
        reqwest::Method::GET,
        &format!("/repos/{owner}/{name}"),
        None,
    ) {
        Ok(body) => {
            let full = body
                .get("full_name")
                .and_then(Value::as_str)
                .unwrap_or(&format!("{owner}/{name}"))
                .to_string();
            Ok((full, false))
        }
        Err(VonError::NotFound(_)) => {
            // Only ever under the token's own account. Creating repositories
            // inside someone's organisation is not a decision to make on their
            // behalf from a phone.
            let me = fetch_account(token)?.login;
            if !owner.eq_ignore_ascii_case(&me) {
                return Err(VonError::NotFound(format!(
                    "'{owner}/{name}' does not exist. VON only creates repositories under your \
                     own account ({me}); create this one on GitHub first."
                )));
            }
            let body = request(
                token,
                reqwest::Method::POST,
                "/user/repos",
                Some(json!({
                    "name": name,
                    "private": private,
                    "description": "Built with VON",
                    // No auto-init: an initial commit here would be a parent we
                    // would immediately have to force past.
                    "auto_init": false,
                })),
            )?;
            let full = body
                .get("full_name")
                .and_then(Value::as_str)
                .unwrap_or(&format!("{me}/{name}"))
                .to_string();
            Ok((full, true))
        }
        Err(e) => Err(e),
    }
}

/// GitHub's own rule: letters, digits, `.`, `-`, `_`. Checked here so a bad
/// name fails before any blob is uploaded.
fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(VonError::InvalidArgument(
            "a repository name is required".into(),
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        return Err(VonError::InvalidArgument(format!(
            "'{name}' is not a valid repository name: '{bad}' is not allowed. Use letters, \
             digits, dots, hyphens and underscores."
        )));
    }
    Ok(())
}

fn split_repo(full_name: &str) -> Result<(String, String)> {
    full_name
        .split_once('/')
        .map(|(o, n)| (o.to_string(), n.to_string()))
        .ok_or_else(|| VonError::Other(format!("GitHub returned an odd repository name: {full_name}")))
}

/// The sha the branch currently points at, or `None` when the branch (or the
/// repository's history) does not exist yet.
fn head_commit(token: &str, owner: &str, name: &str, branch: &str) -> Result<Option<String>> {
    match request(
        token,
        reqwest::Method::GET,
        &format!("/repos/{owner}/{name}/git/ref/heads/{branch}"),
        None,
    ) {
        Ok(body) => Ok(body
            .get("object")
            .and_then(|o| o.get("sha"))
            .and_then(Value::as_str)
            .map(str::to_string)),
        // An empty repository answers 409 rather than 404, which arrives as a
        // Provider error; both mean the same thing to us.
        Err(VonError::NotFound(_)) => Ok(None),
        Err(VonError::Provider { status: 409, .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// One authenticated call, with GitHub's error bodies turned into ours.
fn request(
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let url = format!("{API}{path}");
    state::block_on(async move {
        let mut req = state::http()
            .request(method, &url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT);
        if let Some(body) = body {
            req = req.json(&body);
        }

        let response = req
            .send()
            .await
            .map_err(|e| VonError::Network(format!("could not reach GitHub: {e}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if status.is_success() {
            // 204 No Content is a success with nothing to parse.
            return Ok(if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::Null)
            });
        }

        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(300).collect());

        Err(match status.as_u16() {
            401 => VonError::MissingApiKey(format!(
                "{KEY_SLOT}: the token was rejected ({detail}). Reconnect with a fresh one."
            )),
            403 if detail.contains("rate limit") => {
                VonError::Provider { provider: "github".into(), status: 403, body: detail }
            }
            403 => VonError::PermissionDenied(format!(
                "GitHub refused this ({detail}). The token needs the `repo` scope — or, for a \
                 fine-grained token, Contents: Read and write on this repository."
            )),
            404 => VonError::NotFound(detail),
            422 => VonError::InvalidArgument(format!("GitHub rejected this: {detail}")),
            status => VonError::Provider {
                provider: "github".into(),
                status,
                body: detail,
            },
        })
    })
    .map_err(|e| VonError::Other(format!("GitHub request failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_are_checked_before_anything_is_uploaded() {
        assert!(validate_repo_name("von-report").is_ok());
        assert!(validate_repo_name("VON_2.tar").is_ok());
        assert!(validate_repo_name("my repo").is_err());
        assert!(validate_repo_name("owner/name").is_err());
        assert!(validate_repo_name("").is_err());
    }

    #[test]
    fn full_names_split_into_owner_and_repo() {
        let (owner, name) = split_repo("tom32bit/VON").unwrap();
        assert_eq!(owner, "tom32bit");
        assert_eq!(name, "VON");
        assert!(split_repo("VON").is_err());
    }

    #[test]
    fn secrets_and_junk_never_reach_a_public_repo() {
        let (_dir, sandbox) = sandbox();
        sandbox.write_file("main.py", "print(1)").unwrap();
        sandbox.write_file(".env", "TOKEN=hunter2").unwrap();
        sandbox.write_file("src/app.js", "run()").unwrap();
        sandbox
            .write_file("node_modules/left-pad/index.js", "//")
            .unwrap();
        sandbox.write_file("__pycache__/main.pyc", "junk").unwrap();

        let payload = collect(&sandbox).unwrap();
        let paths: Vec<&str> = payload.files.iter().map(|f| f.path.as_str()).collect();
        let skipped = &payload.skipped;

        assert_eq!(paths, vec!["main.py", "src/app.js"]);
        assert_eq!(skipped.len(), 3, "skipped: {skipped:?}");
        assert!(
            skipped.iter().any(|s| s.starts_with(".env")),
            "the reason has to name the file: {skipped:?}"
        );
    }

    #[test]
    fn a_secret_nested_deep_is_still_caught() {
        let (_dir, sandbox) = sandbox();
        sandbox.write_file("a/b/.env", "TOKEN=x").unwrap();
        sandbox.write_file("a/b/keep.txt", "fine").unwrap();

        let payload = collect(&sandbox).unwrap();
        assert_eq!(payload.files.len(), 1);
        assert_eq!(payload.files[0].path, "a/b/keep.txt");
    }

    #[test]
    fn a_file_merely_named_like_a_secret_is_kept() {
        let (_dir, sandbox) = sandbox();
        // `.env.example` is a convention meant to be committed; only an exact
        // segment match should be filtered.
        sandbox.write_file(".env.example", "TOKEN=").unwrap();
        sandbox.write_file("environment.md", "notes").unwrap();

        let payload = collect(&sandbox).unwrap();
        assert_eq!(
            payload.files.len(),
            2,
            "wrongly skipped: {:?}",
            payload.skipped
        );
    }

    /// Matches sandbox.rs's own helper: the TempDir has to stay alive for the
    /// duration of the test, so it is returned alongside the sandbox.
    fn sandbox() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::open(dir.path().join("proj")).unwrap();
        (dir, sb)
    }
}
