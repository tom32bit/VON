//! Process-wide state: directories, the async runtime, the shared HTTP client,
//! and per-project cancellation flags.
//!
//! Android can recreate the Flutter activity without restarting the process, so
//! everything here is initialised once and survives across UI lifecycles.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use once_cell::sync::{Lazy, OnceCell};
use parking_lot::RwLock;

use crate::error::{Result, VonError};
use crate::sandbox::Sandbox;

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// App-private root (`getApplicationSupportDirectory()` on Android).
    pub root: PathBuf,
    /// `<root>/projects/<project_id>/`
    pub projects: PathBuf,
    /// Downloaded GGUF weights.
    pub models: PathBuf,
    /// Exported `.zip` archives, written where the user's file picker can see them.
    pub exports: PathBuf,
    pub db_file: PathBuf,
}

static PATHS: OnceCell<AppPaths> = OnceCell::new();

/// The single multi-threaded runtime. Cloud requests and downloads are async;
/// inference and code execution are blocking and run on dedicated threads.
static RUNTIME: OnceCell<tokio::runtime::Runtime> = OnceCell::new();

/// One HTTP client for the whole app: connection pooling matters a lot on a
/// phone radio, where each new TLS handshake costs real battery.
static HTTP: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("VON/", env!("CARGO_PKG_VERSION")))
        // No global request timeout: streaming completions legitimately stay
        // open for minutes. Per-request timeouts are applied where they apply.
        .connect_timeout(std::time::Duration::from_secs(20))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("failed to build HTTP client")
});

/// Cancellation flags, keyed by project id. The UI's "Stop" button flips one of
/// these; the agent loop checks it between steps and mid-token.
static CANCEL: Lazy<RwLock<HashMap<String, Arc<AtomicBool>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub fn init(app_root: &Path) -> Result<()> {
    let paths = AppPaths {
        root: app_root.to_path_buf(),
        projects: app_root.join("projects"),
        models: app_root.join("models"),
        exports: app_root.join("exports"),
        db_file: app_root.join("von.db"),
    };
    for dir in [&paths.projects, &paths.models, &paths.exports] {
        std::fs::create_dir_all(dir)?;
    }
    let _ = PATHS.set(paths);

    if RUNTIME.get().is_none() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Two worker threads is plenty for HTTP and downloads, and leaves
            // the cores that matter to inference and to Flutter's raster thread.
            .worker_threads(2)
            .thread_name("von-tokio")
            .enable_all()
            .build()
            .map_err(|e| VonError::Other(format!("tokio runtime: {e}")))?;
        let _ = RUNTIME.set(rt);
    }
    Ok(())
}

pub fn paths() -> Result<&'static AppPaths> {
    PATHS
        .get()
        .ok_or_else(|| VonError::Other("app not initialised; call init_app first".into()))
}

pub fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    RUNTIME
        .get()
        .ok_or_else(|| VonError::Other("runtime not initialised; call init_app first".into()))
}

/// Runs a future to completion on the shared runtime, blocking the caller.
/// Used by the FFI surface, which is already on a background thread.
pub fn block_on<F: std::future::Future>(fut: F) -> Result<F::Output> {
    Ok(runtime()?.block_on(fut))
}

pub fn http() -> &'static reqwest::Client {
    &HTTP
}

pub fn sandbox_for(project_id: &str) -> Result<Sandbox> {
    // Project ids are UUIDs we generate, but they arrive back from Dart, so
    // treat them as untrusted and validate before using one as a path segment.
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(VonError::InvalidArgument(format!(
            "'{project_id}' is not a valid project id"
        )));
    }
    Sandbox::open(paths()?.projects.join(project_id))
}

// ---- cancellation -------------------------------------------------------

/// How many agent runs are in flight, across all projects.
///
/// Android backgrounding releases the resident model and the preview servers,
/// which is right when the app is idle and wrong when it is mid-run: dropping a
/// 1.7GB mmap only to fault it straight back in is slow, spikes memory, and
/// makes the process a *better* target for the low-memory killer, not a worse
/// one.
static ACTIVE_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Marks a run as in flight until the returned guard is dropped.
///
/// A guard rather than a pair of calls because the loop has several early
/// returns — a refusal, a transient error, a cancellation — and any one of them
/// forgetting to decrement would leave the app permanently unable to shed
/// memory on pause.
pub struct RunGuard(());

pub fn hold_active_run() -> RunGuard {
    ACTIVE_RUNS.fetch_add(1, Ordering::SeqCst);
    RunGuard(())
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        ACTIVE_RUNS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn runs_in_flight() -> bool {
    ACTIVE_RUNS.load(Ordering::SeqCst) > 0
}

pub fn cancel_flag(project_id: &str) -> Arc<AtomicBool> {
    let mut map = CANCEL.write();
    map.entry(project_id.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// Arms a fresh run: clears any stale cancellation from a previous one.
pub fn begin_run(project_id: &str) -> Arc<AtomicBool> {
    let flag = cancel_flag(project_id);
    flag.store(false, Ordering::SeqCst);
    flag
}

pub fn request_cancel(project_id: &str) {
    cancel_flag(project_id).store(true, Ordering::SeqCst);
}

pub fn is_cancelled(project_id: &str) -> bool {
    cancel_flag(project_id).load(Ordering::SeqCst)
}
