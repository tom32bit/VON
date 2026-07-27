//! Data transfer objects for the Dart bridge.
//!
//! These are deliberately flat: `flutter_rust_bridge` mirrors them into Dart
//! classes, and flat structs with primitive fields cross the boundary cheaply
//! and generate readable Dart. In particular the file tree is flattened to a
//! depth-tagged list rather than a recursive type — the UI renders it as an
//! indented list anyway, and recursion across FFI buys nothing here.

use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::VonError;
use crate::governor;
use crate::llm::catalog;
use crate::sandbox::FileNode;

/// Error surfaced to Dart. `kind` is stable and machine-readable so the UI can
/// branch (e.g. show the API-key sheet on `missing_api_key`) without matching
/// on message text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VonException {
    pub kind: String,
    pub message: String,
    /// Whether retrying the same action could plausibly succeed.
    pub transient: bool,
}

impl From<VonError> for VonException {
    fn from(e: VonError) -> Self {
        Self {
            kind: e.kind().to_string(),
            message: e.to_string(),
            transient: e.is_transient(),
        }
    }
}

impl std::fmt::Display for VonException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)
    }
}

impl std::error::Error for VonException {}

pub type ApiResult<T> = std::result::Result<T, VonException>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: String,
    /// The user-facing VON number, rendered as "VON 3". Stable for the life of
    /// the project — see the `projects.ordinal` schema comment.
    pub ordinal: i64,
    pub name: String,
    pub description: String,
    /// `web` | `data` | `docs` | `general`
    pub kind: String,
    pub provider: String,
    pub model_id: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub size_bytes: i64,
    pub file_count: i32,
}

impl ProjectDto {
    pub fn from_row(row: db::ProjectRow, file_count: i32) -> Self {
        Self {
            id: row.id,
            ordinal: row.ordinal,
            name: row.name,
            description: row.description,
            kind: row.kind,
            provider: row.provider,
            model_id: row.model_id,
            created_at_ms: row.created_at,
            updated_at_ms: row.updated_at,
            size_bytes: row.size_bytes,
            file_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDto {
    pub seq: i64,
    /// `user` | `assistant` | `tool` | `system`
    pub role: String,
    pub content: String,
    /// JSON: tool calls for an assistant turn, tool results for a tool turn.
    pub steps_json: String,
    pub created_at_ms: i64,
}

impl From<db::MessageRow> for MessageDto {
    fn from(row: db::MessageRow) -> Self {
        Self {
            seq: row.seq,
            role: row.role,
            content: row.content,
            steps_json: row.steps_json,
            created_at_ms: row.created_at,
        }
    }
}

/// One row of the Sandbox Explorer. `depth` drives the indent; children always
/// follow their parent immediately, so a single pass renders the tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntryDto {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: i64,
    pub modified_ms: i64,
    pub depth: i32,
}

/// Flattens the nested tree in display order.
pub fn flatten_tree(nodes: &[FileNode], depth: i32, out: &mut Vec<FileEntryDto>) {
    for node in nodes {
        out.push(FileEntryDto {
            path: node.path.clone(),
            name: node.name.clone(),
            is_dir: node.is_dir,
            size_bytes: node.size_bytes as i64,
            modified_ms: node.modified_ms,
            depth,
        });
        if node.is_dir {
            flatten_tree(&node.children, depth + 1, out);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineModelDto {
    pub id: String,
    pub name: String,
    pub quant: String,
    pub size_bytes: i64,
    pub ram_required_mb: i64,
    pub context_len: i64,
    pub params_b: f32,
    pub blurb: String,
    pub downloaded: bool,
    pub is_default: bool,
    /// `green` | `yellow` | `red`
    pub compatibility: String,
    pub compatibility_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudModelDto {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_tokens: i64,
    pub blurb: String,
    pub input_price: f32,
    pub output_price: f32,
    /// Whether an API key for this provider is configured.
    pub key_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgressDto {
    pub model_id: String,
    pub received_bytes: i64,
    pub total_bytes: i64,
    pub done: bool,
    pub error: Option<String>,
}

impl From<catalog::DownloadProgress> for DownloadProgressDto {
    fn from(p: catalog::DownloadProgress) -> Self {
        Self {
            model_id: p.model_id,
            received_bytes: p.received_bytes,
            total_bytes: p.total_bytes,
            done: p.done,
            error: p.error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfileDto {
    /// 1 = low-end, 2 = mid-range, 3 = flagship.
    pub tier: i32,
    pub tier_label: String,
    pub cores: i32,
    pub total_ram_mb: i64,
    pub available_ram_mb: i64,
    pub score: f64,
    pub max_local_params_b: f32,
    pub notes: Vec<String>,
}

impl From<governor::DeviceProfile> for DeviceProfileDto {
    fn from(p: governor::DeviceProfile) -> Self {
        Self {
            tier: p.tier.as_i32(),
            tier_label: p.tier.label().to_string(),
            cores: p.cores as i32,
            total_ram_mb: p.total_ram_mb as i64,
            available_ram_mb: p.available_ram_mb as i64,
            score: p.score,
            max_local_params_b: p.max_local_params_b,
            notes: p.notes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatsDto {
    /// `normal` | `light` | `severe` | `critical`
    pub thermal: String,
    pub temperature_c: f32,
    pub cpu_cores: i32,
    pub cpu_usage_pct: f32,
    pub total_ram_mb: i64,
    pub available_ram_mb: i64,
    /// `-1` when the device does not expose it.
    pub battery_pct: i32,
    pub inference_threads: i32,
    pub token_delay_ms: i64,
    pub paused: bool,
}

impl From<governor::DeviceStats> for DeviceStatsDto {
    fn from(s: governor::DeviceStats) -> Self {
        Self {
            thermal: s.thermal.as_str().to_string(),
            temperature_c: s.temperature_c,
            cpu_cores: s.cpu_cores as i32,
            cpu_usage_pct: s.cpu_usage_pct,
            total_ram_mb: s.total_ram_mb as i64,
            available_ram_mb: s.available_ram_mb as i64,
            battery_pct: s.battery_pct,
            inference_threads: s.plan.threads as i32,
            token_delay_ms: s.plan.delay_ms as i64,
            paused: s.plan.paused,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxUsageDto {
    pub total_bytes: i64,
    pub file_count: i32,
    pub max_bytes: i64,
    pub max_files: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, is_dir: bool, children: Vec<FileNode>) -> FileNode {
        FileNode {
            path: name.to_string(),
            name: name.to_string(),
            is_dir,
            size_bytes: 10,
            modified_ms: 0,
            children,
        }
    }

    #[test]
    fn flattening_preserves_display_order_and_depth() {
        let tree = vec![
            node(
                "src",
                true,
                vec![node("main.py", false, vec![]), node("util.py", false, vec![])],
            ),
            node("README.md", false, vec![]),
        ];
        let mut out = Vec::new();
        flatten_tree(&tree, 0, &mut out);

        let shape: Vec<(&str, i32)> = out.iter().map(|e| (e.name.as_str(), e.depth)).collect();
        assert_eq!(
            shape,
            vec![("src", 0), ("main.py", 1), ("util.py", 1), ("README.md", 0)]
        );
    }

    #[test]
    fn exceptions_carry_a_stable_kind() {
        let e: VonException = VonError::MissingApiKey("anthropic".into()).into();
        assert_eq!(e.kind, "missing_api_key");
        assert!(!e.transient);

        let net: VonException = VonError::Network("offline".into()).into();
        assert!(net.transient, "network errors should invite a retry");
    }
}
