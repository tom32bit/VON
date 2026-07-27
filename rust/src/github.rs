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
use std::collections::{BTreeMap, BTreeSet, HashSet};

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
    /// Files that came *from* GitHub during the merge and are now in the
    /// sandbox. A sync changes the VON as well as the repo, and saying so is
    /// the difference between a merge and a surprise.
    pub pulled: Vec<String>,
    /// Files deleted on GitHub since the last sync, and so deleted here too.
    pub removed_locally: Vec<String>,
    /// The branch already held exactly this tree, so no commit was made.
    pub up_to_date: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullOutcome {
    pub repo_full_name: String,
    pub branch: String,
    pub commit_sha: String,
    pub pulled: Vec<String>,
    pub removed_locally: Vec<String>,
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

/// Synchronises the sandbox with a branch and leaves both holding the merged
/// result.
///
/// This is a three-way merge, not an overwrite. `base` — the tree VON last
/// pushed for this project — is fetched from the link table, and every path is
/// resolved against it, so a file added on GitHub since the last push survives
/// and a file the agent deleted really is deleted. Changes that arrived
/// remotely are written back into the sandbox before the push, because a
/// commit containing content the Files tab has never seen would be a lie.
///
/// Refuses on conflict rather than picking a side. There is no local history to
/// fall back on and no way to show a diff on a phone worth acting on, so the
/// honest move is to name the files and stop.
pub fn sync_sandbox(
    project_id: &str,
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

    on_progress("Reading the VON");
    let Payload { files, skipped } = collect(sandbox)?;
    let ours: Snapshot = files
        .iter()
        .map(|f| (f.path.clone(), blob_sha(&f.bytes)))
        .collect();
    let local_bytes: BTreeMap<&str, &Vec<u8>> =
        files.iter().map(|f| (f.path.as_str(), &f.bytes)).collect();

    on_progress("Resolving repository");
    let (full_name, created_repo) = ensure_repo(&token, repo, private)?;
    let (owner, name) = split_repo(&full_name)?;

    // A repository created a moment ago has no commits, so there is no ref to
    // read and the commit we build is a root commit.
    let parent = head_commit(&token, &owner, &name, branch)?;

    on_progress("Comparing with GitHub");
    let (parent_tree, theirs) = match &parent {
        Some(commit) => {
            let tree_sha = commit_tree(&token, &owner, &name, commit)?;
            let listing = snapshot(&token, &owner, &name, &tree_sha)?;
            (Some(tree_sha), listing)
        }
        None => (None, Snapshot::new()),
    };

    // The merge base. A link recorded against a different repo or branch tells
    // us nothing about *this* target, so it is ignored rather than trusted —
    // an empty base degrades to "treat everything remote as new", which is
    // conservative in the right direction: it never deletes.
    let link = crate::db::get_github_link(project_id)?;
    let base = match &link {
        Some(l) if l.repo == full_name && l.branch == branch => {
            snapshot(&token, &owner, &name, &l.base_tree)?
        }
        _ => Snapshot::new(),
    };

    let plan = plan_merge(&base, &ours, &theirs);
    if !plan.conflicts.is_empty() {
        return Err(VonError::Conflict(format!(
            "{} changed both here and on {full_name}. Nothing was pushed and nothing in this VON \
             was touched. Push to a different branch to keep both versions, or delete your copy \
             of the file to take theirs.",
            plan.conflicts.join(", ")
        )));
    }

    // Sandbox first. If a quota rejects an incoming file, that should happen
    // before a commit exists, not after.
    let pulled = plan.paths_with(Action::Pull);
    let removed_locally = plan.paths_with(Action::RemoveBoth);
    for (index, path) in pulled.iter().enumerate() {
        on_progress(&format!(
            "Bringing in {} of {}",
            index + 1,
            pulled.len()
        ));
        let sha = &plan.entries[path].0;
        let bytes = fetch_blob(&token, &owner, &name, sha)?;
        sandbox.write_bytes(path, &bytes)?;
    }
    for path in &removed_locally {
        // Already gone is the desired state, so a missing file is not an error.
        let _ = sandbox.delete(path);
    }

    let uploads = plan.paths_with(Action::Upload);
    let mut tree_entries = Vec::with_capacity(plan.entries.len());
    for (index, path) in uploads.iter().enumerate() {
        on_progress(&format!("Uploading {} of {}", index + 1, uploads.len()));
        let bytes = local_bytes.get(path.as_str()).ok_or_else(|| {
            VonError::Other(format!("'{path}' was planned for upload but has no content"))
        })?;
        let blob = request(
            &token,
            reqwest::Method::POST,
            &format!("/repos/{owner}/{name}/git/blobs"),
            Some(json!({
                "content": base64::engine::general_purpose::STANDARD.encode(bytes),
                "encoding": "base64",
            })),
        )?;
        let returned = blob
            .get("sha")
            .and_then(Value::as_str)
            .ok_or_else(|| VonError::Other(format!("GitHub stored '{path}' but returned no sha")))?;
        debug_assert_eq!(returned, plan.entries[path].0, "blob id disagreement");
    }

    for (path, (sha, action)) in &plan.entries {
        if matches!(action, Action::RemoveBoth | Action::Drop) {
            continue;
        }
        tree_entries.push(json!({
            "path": path,
            // 100644 for everything: a sandbox has no executables to speak of
            // and no way to record a mode, so claiming one would be a lie.
            "mode": "100644",
            "type": "blob",
            "sha": sha,
        }));
    }
    if tree_entries.is_empty() {
        return Err(VonError::InvalidArgument(
            "this VON has no files to publish yet".into(),
        ));
    }

    // Nothing to say: the merged tree is byte-for-byte what the branch already
    // holds. An empty commit would just be noise in the history.
    let unchanged = parent.is_some()
        && theirs.len() == tree_entries.len()
        && plan
            .entries
            .iter()
            .all(|(p, (sha, _))| theirs.get(p) == Some(sha));
    if let (true, Some(commit), Some(tree)) = (unchanged, &parent, &parent_tree) {
        // Still worth recording: the base may have been stale or missing, and
        // this is the cheapest moment to learn that the two sides agree.
        remember(project_id, &full_name, branch, commit, tree)?;
        let commit = commit.clone();
        return Ok(PushOutcome {
            commit_url: format!("https://github.com/{full_name}/commit/{commit}"),
            repo_full_name: full_name,
            branch: branch.to_string(),
            commit_sha: commit,
            files_pushed: 0,
            created_repo,
            skipped,
            pulled,
            removed_locally,
            up_to_date: true,
        });
    }

    on_progress("Building the commit");
    // No `base_tree`: `tree_entries` is already the complete merged tree, so
    // layering it on the previous one would resurrect the paths the merge
    // decided to delete.
    let tree = request(
        &token,
        reqwest::Method::POST,
        &format!("/repos/{owner}/{name}/git/trees"),
        Some(json!({ "tree": tree_entries })),
    )?;
    let tree_sha = tree
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| VonError::Other("GitHub returned a tree with no sha".into()))?
        .to_string();

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
        // Not forced. The commit's parent is the head read at the start of this
        // sync, so the update is a fast-forward by construction — force would
        // change nothing in the ordinary case. The one case it does change is
        // the one that matters: somebody pushing between our read and our
        // write, whose commit force would silently discard.
        request(
            &token,
            reqwest::Method::PATCH,
            &format!("/repos/{owner}/{name}/git/refs/heads/{branch}"),
            Some(json!({ "sha": commit_sha })),
        )
        .map_err(|e| match e {
            VonError::InvalidArgument(detail) if detail.to_lowercase().contains("fast forward") => {
                VonError::InvalidArgument(format!(
                    "'{branch}' moved on GitHub while this sync was in flight, so it was not \
                     updated. Nothing was lost — sync again to merge against what landed."
                ))
            }
            other => other,
        })?;
    } else {
        request(
            &token,
            reqwest::Method::POST,
            &format!("/repos/{owner}/{name}/git/refs"),
            Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": commit_sha })),
        )?;
    }

    remember(project_id, &full_name, branch, &commit_sha, &tree_sha)?;

    Ok(PushOutcome {
        commit_url: format!("https://github.com/{full_name}/commit/{commit_sha}"),
        repo_full_name: full_name,
        branch: branch.to_string(),
        commit_sha,
        files_pushed: (uploads.len() + pulled.len()) as u32,
        created_repo,
        skipped,
        pulled,
        removed_locally,
        up_to_date: false,
    })
}

/// Records what this project is now in step with, which is the merge base for
/// the next sync.
fn remember(project_id: &str, repo: &str, branch: &str, commit: &str, tree: &str) -> Result<()> {
    crate::db::set_github_link(
        project_id,
        &crate::db::GitHubLink {
            repo: repo.to_string(),
            branch: branch.to_string(),
            base_commit: commit.to_string(),
            base_tree: tree.to_string(),
        },
    )
}

/// Brings a branch into the sandbox without pushing anything back.
///
/// The entry point for starting from an existing repository. It is deliberately
/// one-directional and refuses to clobber: a file that differs locally is a
/// conflict, exactly as it would be during a sync.
pub fn pull_into_sandbox(
    project_id: &str,
    sandbox: &Sandbox,
    repo: &str,
    branch: &str,
    mut on_progress: impl FnMut(&str),
) -> Result<PullOutcome> {
    let token = token()?;
    let branch = if branch.trim().is_empty() {
        "main"
    } else {
        branch.trim()
    };

    on_progress("Resolving repository");
    let repo = repo.trim().trim_matches('/');
    let (owner, name) = match repo.split_once('/') {
        Some((owner, name)) => (owner.to_string(), name.to_string()),
        None => (fetch_account(&token)?.login, repo.to_string()),
    };
    validate_repo_name(&name)?;
    let full_name = format!("{owner}/{name}");

    let head = head_commit(&token, &owner, &name, branch)?.ok_or_else(|| {
        VonError::NotFound(format!("'{full_name}' has no branch '{branch}' to pull from"))
    })?;
    let tree_sha = commit_tree(&token, &owner, &name, &head)?;

    on_progress("Comparing with GitHub");
    let theirs = snapshot(&token, &owner, &name, &tree_sha)?;
    let Payload { files, .. } = collect(sandbox)?;
    let ours: Snapshot = files
        .iter()
        .map(|f| (f.path.clone(), blob_sha(&f.bytes)))
        .collect();

    let link = crate::db::get_github_link(project_id)?;
    let base = match &link {
        Some(l) if l.repo == full_name && l.branch == branch => {
            snapshot(&token, &owner, &name, &l.base_tree)?
        }
        _ => Snapshot::new(),
    };

    let plan = plan_merge(&base, &ours, &theirs);
    if !plan.conflicts.is_empty() {
        return Err(VonError::Conflict(format!(
            "{} changed both here and on {full_name}. Nothing in this VON was touched. Move your \
             version aside, or sync to a different branch.",
            plan.conflicts.join(", ")
        )));
    }

    let pulled = plan.paths_with(Action::Pull);
    let removed_locally = plan.paths_with(Action::RemoveBoth);
    for (index, path) in pulled.iter().enumerate() {
        on_progress(&format!("Writing {} of {}", index + 1, pulled.len()));
        let bytes = fetch_blob(&token, &owner, &name, &plan.entries[path].0)?;
        sandbox.write_bytes(path, &bytes)?;
    }
    for path in &removed_locally {
        let _ = sandbox.delete(path);
    }

    // The branch this VON is now in step with. Recording it is what lets the
    // next sync tell a genuine deletion from a file that was simply never
    // there. Local-only work is safe: it is absent from both the base and the
    // remote, which the merge reads as "ours, keep it".
    remember(project_id, &full_name, branch, &head, &tree_sha)?;

    Ok(PullOutcome {
        repo_full_name: full_name,
        branch: branch.to_string(),
        commit_sha: head,
        pulled,
        removed_locally,
    })
}

/// The tree a commit points at.
fn commit_tree(token: &str, owner: &str, name: &str, commit: &str) -> Result<String> {
    let body = request(
        token,
        reqwest::Method::GET,
        &format!("/repos/{owner}/{name}/git/commits/{commit}"),
        None,
    )?;
    body.get("tree")
        .and_then(|t| t.get("sha"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VonError::Other(format!("commit {commit} has no tree")))
}

/// Every file in a tree, flattened to path -> blob id.
///
/// GitHub truncates the recursive listing for very large trees. A truncated
/// answer would read as "those paths do not exist", which the merge would
/// interpret as deletions, so it is refused instead.
fn snapshot(token: &str, owner: &str, name: &str, tree_sha: &str) -> Result<Snapshot> {
    let body = request(
        token,
        reqwest::Method::GET,
        &format!("/repos/{owner}/{name}/git/trees/{tree_sha}?recursive=1"),
        None,
    )?;

    if body.get("truncated").and_then(Value::as_bool) == Some(true) {
        return Err(VonError::Other(format!(
            "'{owner}/{name}' is too large for VON to compare safely — GitHub truncated its file \
             listing. Syncing it would risk deleting files that were simply not listed."
        )));
    }

    let mut out = Snapshot::new();
    for entry in body
        .get("tree")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if entry.get("type").and_then(Value::as_str) != Some("blob") {
            continue;
        }
        let (Some(path), Some(sha)) = (
            entry.get("path").and_then(Value::as_str),
            entry.get("sha").and_then(Value::as_str),
        ) else {
            continue;
        };
        out.insert(path.to_string(), sha.to_string());
    }
    Ok(out)
}

/// A blob's bytes. GitHub answers base64 regardless of whether it is text.
fn fetch_blob(token: &str, owner: &str, name: &str, sha: &str) -> Result<Vec<u8>> {
    let body = request(
        token,
        reqwest::Method::GET,
        &format!("/repos/{owner}/{name}/git/blobs/{sha}"),
        None,
    )?;
    let content = body
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| VonError::Other(format!("blob {sha} came back with no content")))?;

    // The JSON form is wrapped at 60 characters, which the decoder will not
    // accept as-is.
    let stripped: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(stripped)
        .map_err(|e| VonError::Other(format!("blob {sha} was not valid base64: {e}")))
}

// ---- three-way merge ----------------------------------------------------

/// Git's blob object id: sha1 over the header `blob <len>\0` and then the
/// bytes. Computing it locally is what lets a push skip uploading a file the
/// repository already has — the id *is* the content, so a match is proof.
fn blob_sha(bytes: &[u8]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// A snapshot of a tree: path to blob id.
type Snapshot = BTreeMap<String, String>;

/// What one path's three-way comparison decided.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Goes into the new tree at this blob id, and the bytes must be uploaded
    /// first because the repository does not have that object yet.
    Upload,
    /// Goes into the new tree at this blob id, which the repository already
    /// has — no upload.
    Reuse,
    /// Goes into the new tree, and the sandbox must be updated to match.
    Pull,
    /// Leaves the tree, and leaves the sandbox.
    RemoveBoth,
    /// Leaves the tree; the sandbox already does not have it.
    Drop,
}

#[derive(Debug, Default)]
struct Plan {
    /// path -> (blob id, what to do about it)
    entries: BTreeMap<String, (String, Action)>,
    /// Paths changed on both sides since the base. Non-empty means refuse.
    conflicts: Vec<String>,
}

impl Plan {
    fn paths_with(&self, action: Action) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, (_, a))| *a == action)
            .map(|(p, _)| p.clone())
            .collect()
    }
}

/// Decides, for every path either side knows about, what the push should do.
///
/// This is an ordinary three-way merge at file granularity. `base` is the tree
/// VON last pushed for this project, which is the only thing that makes the
/// interesting cases decidable: without it, a file present on the remote and
/// absent locally is equally consistent with "the user deleted it" and "someone
/// else added it", and guessing wrong destroys work either way.
///
/// Granularity is the file, not the line. Line-level merging needs a real diff3
/// and would turn every awkward case into a half-merged file on a phone, which
/// is worse than being told two people edited the same thing.
fn plan_merge(base: &Snapshot, ours: &Snapshot, theirs: &Snapshot) -> Plan {
    // A blob id anywhere on the remote means the object exists in the
    // repository, so any path can reference it without an upload.
    let remote_objects: HashSet<&str> = theirs.values().map(String::as_str).collect();

    let mut plan = Plan::default();
    let paths: BTreeSet<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();

    for path in paths {
        let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));

        // Filtered paths are outside the merge entirely. They are never in
        // `ours` (collect drops them), so treating them normally would read as
        // a local deletion on the second push and quietly delete a `.env`
        // somebody deliberately committed. Pass them through untouched, and
        // never write them into the sandbox.
        if is_filtered(path) {
            if let Some(sha) = t {
                plan.entries
                    .insert(path.clone(), (sha.clone(), Action::Reuse));
            }
            continue;
        }

        if o == t {
            // Same on both sides — including both absent, and including both
            // having made the identical change.
            if let Some(sha) = t {
                plan.entries
                    .insert(path.clone(), (sha.clone(), Action::Reuse));
            }
        } else if b == o {
            // We did not touch it; whatever happened remotely is the truth.
            match t {
                Some(sha) => {
                    plan.entries
                        .insert(path.clone(), (sha.clone(), Action::Pull));
                }
                None => {
                    plan.entries
                        .insert(path.clone(), (String::new(), Action::RemoveBoth));
                }
            }
        } else if b == t {
            // They did not touch it; ours is the truth.
            match o {
                Some(sha) => {
                    let action = if remote_objects.contains(sha.as_str()) {
                        Action::Reuse
                    } else {
                        Action::Upload
                    };
                    plan.entries.insert(path.clone(), (sha.clone(), action));
                }
                None => {
                    plan.entries
                        .insert(path.clone(), (String::new(), Action::Drop));
                }
            }
        } else {
            plan.conflicts.push(path.clone());
        }
    }
    plan
}

fn is_filtered(path: &str) -> bool {
    path.split('/')
        .any(|segment| NEVER_PUBLISH.contains(&segment))
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
        debug_assert!(!is_filtered(&path));
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

    // ---- three-way merge -------------------------------------------------
    //
    // The whole point of the merge base is that these cases are decidable.
    // Without one, "on the remote but not here" is equally consistent with a
    // local deletion and a remote addition, and guessing destroys work.

    fn snap(entries: &[(&str, &str)]) -> Snapshot {
        entries
            .iter()
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .collect()
    }

    fn action(plan: &Plan, path: &str) -> Option<Action> {
        plan.entries.get(path).map(|(_, a)| a.clone())
    }

    #[test]
    fn a_file_added_on_github_survives_the_next_push() {
        // The bug this whole mechanism exists to prevent: someone adds a README
        // on the web, and the next push from the phone deletes it.
        let base = snap(&[("main.py", "aaa")]);
        let ours = snap(&[("main.py", "aaa")]);
        let theirs = snap(&[("main.py", "aaa"), ("README.md", "bbb")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(action(&plan, "README.md"), Some(Action::Pull));
        assert_eq!(
            action(&plan, "main.py"),
            Some(Action::Reuse),
            "an untouched file should cost nothing"
        );
    }

    #[test]
    fn a_file_the_agent_deleted_is_deleted_on_github() {
        let base = snap(&[("main.py", "aaa"), ("old.py", "ccc")]);
        let ours = snap(&[("main.py", "aaa")]);
        let theirs = snap(&[("main.py", "aaa"), ("old.py", "ccc")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(action(&plan, "old.py"), Some(Action::Drop));
    }

    #[test]
    fn a_file_deleted_on_github_is_deleted_here() {
        let base = snap(&[("main.py", "aaa"), ("gone.py", "ccc")]);
        let ours = snap(&[("main.py", "aaa"), ("gone.py", "ccc")]);
        let theirs = snap(&[("main.py", "aaa")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(action(&plan, "gone.py"), Some(Action::RemoveBoth));
    }

    #[test]
    fn a_locally_edited_file_is_uploaded() {
        let base = snap(&[("main.py", "aaa")]);
        let ours = snap(&[("main.py", "zzz")]);
        let theirs = snap(&[("main.py", "aaa")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert_eq!(action(&plan, "main.py"), Some(Action::Upload));
        assert_eq!(plan.entries["main.py"].0, "zzz");
    }

    #[test]
    fn both_sides_editing_the_same_file_is_a_conflict() {
        let base = snap(&[("main.py", "aaa")]);
        let ours = snap(&[("main.py", "zzz")]);
        let theirs = snap(&[("main.py", "yyy")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert_eq!(plan.conflicts, vec!["main.py"]);
        assert!(
            !plan.entries.contains_key("main.py"),
            "a conflicted path must not silently pick a side"
        );
    }

    #[test]
    fn both_sides_making_the_identical_change_is_not_a_conflict() {
        // Same content, so there is nothing to disagree about.
        let base = snap(&[("main.py", "aaa")]);
        let ours = snap(&[("main.py", "zzz")]);
        let theirs = snap(&[("main.py", "zzz")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(action(&plan, "main.py"), Some(Action::Reuse));
    }

    #[test]
    fn a_file_github_already_has_is_never_re_uploaded() {
        // Blob ids are content-addressed, so a matching id anywhere on the
        // remote proves the object exists. On mobile data this is the
        // difference between sending one file and sending the project.
        let base = snap(&[("a.py", "aaa")]);
        let ours = snap(&[("a.py", "aaa"), ("copy.py", "aaa")]);
        let theirs = snap(&[("a.py", "aaa")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert_eq!(
            action(&plan, "copy.py"),
            Some(Action::Reuse),
            "the blob is already in the repository under another path"
        );
    }

    #[test]
    fn a_first_sync_against_an_existing_repo_imports_it() {
        // No base: nothing was ever pushed from here, so every remote file is
        // new to this VON rather than something it deleted.
        let base = Snapshot::new();
        let ours = snap(&[("notes.md", "aaa")]);
        let theirs = snap(&[("src/lib.rs", "bbb"), ("Cargo.toml", "ccc")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(action(&plan, "src/lib.rs"), Some(Action::Pull));
        assert_eq!(action(&plan, "Cargo.toml"), Some(Action::Pull));
        assert_eq!(action(&plan, "notes.md"), Some(Action::Upload));
    }

    #[test]
    fn a_committed_secret_is_left_alone_rather_than_deleted() {
        // `.env` never appears in `ours` because collect filters it. Treating
        // that as a local deletion would erase a file somebody deliberately
        // committed — and would do it on the *second* sync, once the base knew
        // about it, which is exactly the kind of delayed bug that gets shipped.
        let base = snap(&[("main.py", "aaa"), (".env", "sss")]);
        let ours = snap(&[("main.py", "aaa")]);
        let theirs = snap(&[("main.py", "aaa"), (".env", "sss")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(
            action(&plan, ".env"),
            Some(Action::Reuse),
            "it stays in the tree"
        );
        assert!(
            !plan.paths_with(Action::Pull).contains(&".env".to_string()),
            "but it is never written into the sandbox"
        );
    }

    #[test]
    fn a_filtered_path_is_never_a_conflict() {
        let base = snap(&[("node_modules/x/index.js", "aaa")]);
        let ours = Snapshot::new();
        let theirs = snap(&[("node_modules/x/index.js", "bbb")]);

        let plan = plan_merge(&base, &ours, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(
            action(&plan, "node_modules/x/index.js"),
            Some(Action::Reuse)
        );
    }

    #[test]
    fn blob_ids_match_git() {
        // `git hash-object` on an empty file, and on "hello\n" — the two values
        // every git implementation agrees on, which is what makes the skip-
        // upload optimisation safe to trust.
        assert_eq!(blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
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
