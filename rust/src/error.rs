//! Error type shared by the whole core.
//!
//! Two audiences read these errors: the Flutter UI (which shows them to the
//! user) and the agent itself (which reads tool failures as observations and is
//! expected to recover from them). `agent_hint` is what gets fed back into the
//! model — it is deliberately instructional rather than diagnostic.

use std::fmt;

pub type Result<T, E = VonError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum VonError {
    /// The agent tried to touch something outside its sandbox.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A sandbox quota (file size, total size, file count) was exceeded.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    #[error("execution failed: {0}")]
    Execution(String),

    #[error("execution timed out after {0}ms")]
    Timeout(u64),

    #[error("model error: {0}")]
    Model(String),

    /// The governor halted work to protect the device.
    #[error("device throttled: {0}")]
    Throttled(String),

    #[error("no API key configured for provider '{0}'")]
    MissingApiKey(String),

    #[error("provider '{provider}' returned {status}: {body}")]
    Provider {
        provider: String,
        status: u16,
        body: String,
    },

    #[error("database error: {0}")]
    Db(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("{0}")]
    Other(String),
}

impl VonError {
    /// Short machine-readable kind, surfaced to Dart so the UI can branch on it
    /// without string matching on the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PermissionDenied(_) => "permission_denied",
            Self::NotFound(_) => "not_found",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::QuotaExceeded(_) => "quota_exceeded",
            Self::Execution(_) => "execution",
            Self::Timeout(_) => "timeout",
            Self::Model(_) => "model",
            Self::Throttled(_) => "throttled",
            Self::MissingApiKey(_) => "missing_api_key",
            Self::Provider { .. } => "provider",
            Self::Db(_) => "db",
            Self::Io(_) => "io",
            Self::Network(_) => "network",
            Self::Other(_) => "other",
        }
    }

    /// Whether retrying the exact same call could plausibly succeed. The agent
    /// loop uses this to decide between "try again" and "change approach".
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::Throttled(_) | Self::Timeout(_)
        ) || matches!(self, Self::Provider { status, .. } if *status == 429 || *status >= 500)
    }

    /// Text handed back to the model as a tool observation. Tells the agent what
    /// to do differently instead of just what broke.
    pub fn agent_hint(&self) -> String {
        match self {
            Self::PermissionDenied(m) => format!(
                "PermissionDenied: {m}. You may only touch paths inside the project \
                 sandbox, written relative to the project root (e.g. `src/main.py`). \
                 Absolute paths and `..` are rejected."
            ),
            Self::NotFound(m) => format!(
                "NotFound: {m}. Call list_files first to see what actually exists."
            ),
            Self::QuotaExceeded(m) => format!(
                "QuotaExceeded: {m}. Write smaller files or delete something you no \
                 longer need before retrying."
            ),
            Self::Timeout(ms) => format!(
                "Timeout: the program ran longer than {ms}ms and was killed. Reduce \
                 the work per run, or check for an infinite loop."
            ),
            Self::Throttled(m) => format!(
                "Throttled: {m}. The device is protecting itself. Prefer cheaper \
                 steps, or ask the user to switch this project to a cloud model."
            ),
            other => other.to_string(),
        }
    }
}

impl From<std::io::Error> for VonError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(e.to_string()),
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(e.to_string()),
            _ => Self::Io(e.to_string()),
        }
    }
}

impl From<rusqlite::Error> for VonError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<serde_json::Error> for VonError {
    fn from(e: serde_json::Error) -> Self {
        Self::InvalidArgument(format!("json: {e}"))
    }
}

impl From<reqwest::Error> for VonError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout(0)
        } else {
            Self::Network(e.to_string())
        }
    }
}

impl From<zip::result::ZipError> for VonError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Io(format!("zip: {e}"))
    }
}

impl From<anyhow::Error> for VonError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e.to_string())
    }
}

/// Flat, Dart-friendly view of an error. `flutter_rust_bridge` mirrors this as a
/// Dart class so the UI can switch on `kind` and show `message`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorInfo {
    pub kind: String,
    pub message: String,
    pub transient: bool,
}

impl From<&VonError> for ErrorInfo {
    fn from(e: &VonError) -> Self {
        Self {
            kind: e.kind().to_string(),
            message: e.to_string(),
            transient: e.is_transient(),
        }
    }
}

impl fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)
    }
}
