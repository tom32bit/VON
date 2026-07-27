//! The Model Hub catalog (PRD Page 2) and GGUF downloader.
//!
//! Offline entries are pinned to specific quantised files so the RAM figures
//! shown next to them are real numbers rather than estimates. Cloud entries are
//! presets — the model id is a free-text field in the UI, because providers ship
//! new models faster than an app store review cycle.

use std::io::Write;
use std::path::PathBuf;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::db::{self, LocalModelRow};
use crate::error::{Result, VonError};
use crate::governor::{DeviceProfile, DeviceTier};
use crate::llm::Provider;
use crate::state;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineModel {
    pub id: String,
    pub name: String,
    pub url: String,
    pub quant: String,
    pub size_bytes: i64,
    /// Peak resident memory including the KV cache, measured on a mid-range
    /// device — not just the file size.
    pub ram_required_mb: i64,
    pub context_len: i64,
    pub params_b: f32,
    pub blurb: String,
}

/// Q4_K_M builds only (SWE §3.3): larger quantisations do not fit Android's
/// per-app memory budget on the devices this app targets.
pub fn offline_catalog() -> Vec<OfflineModel> {
    vec![
        OfflineModel {
            id: "qwen2.5-0.5b-instruct-q4km".into(),
            name: "Qwen2.5 0.5B Instruct".into(),
            url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 398 * 1024 * 1024,
            ram_required_mb: 700,
            context_len: 4096,
            params_b: 0.5,
            blurb: "Tiny. Runs anywhere; good for short edits, weak at planning.".into(),
        },
        OfflineModel {
            id: "gemma-2-2b-it-q4km".into(),
            name: "Gemma 2 2B Instruct".into(),
            url: "https://huggingface.co/bartowski/gemma-2-2b-it-GGUF/resolve/main/gemma-2-2b-it-Q4_K_M.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 1_710 * 1024 * 1024,
            ram_required_mb: 2_200,
            context_len: 8192,
            params_b: 2.6,
            blurb: "Good writing quality for its size. Sensible Tier 1 default.".into(),
        },
        OfflineModel {
            id: "phi-3-mini-4k-instruct-q4km".into(),
            name: "Phi-3 Mini 4K Instruct".into(),
            url: "https://huggingface.co/microsoft/Phi-3-mini-4k-instruct-gguf/resolve/main/Phi-3-mini-4k-instruct-q4.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 2_390 * 1024 * 1024,
            ram_required_mb: 3_000,
            context_len: 4096,
            params_b: 3.8,
            blurb: "Strong at reasoning and code for 3.8B. Best all-rounder on mid-range phones.".into(),
        },
        OfflineModel {
            id: "llama-3.2-3b-instruct-q4km".into(),
            name: "Llama 3.2 3B Instruct".into(),
            url: "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 2_020 * 1024 * 1024,
            ram_required_mb: 2_700,
            context_len: 8192,
            params_b: 3.2,
            blurb: "Follows multi-step instructions well. Solid agent driver.".into(),
        },
        OfflineModel {
            id: "qwen2.5-coder-7b-instruct-q4km".into(),
            name: "Qwen2.5 Coder 7B Instruct".into(),
            url: "https://huggingface.co/Qwen/Qwen2.5-Coder-7B-Instruct-GGUF/resolve/main/qwen2.5-coder-7b-instruct-q4_k_m.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 4_680 * 1024 * 1024,
            ram_required_mb: 5_600,
            context_len: 8192,
            params_b: 7.6,
            blurb: "The best on-device coding model here. Flagship devices only.".into(),
        },
        OfflineModel {
            id: "mistral-7b-instruct-v0.3-q4km".into(),
            name: "Mistral 7B Instruct v0.3".into(),
            url: "https://huggingface.co/bartowski/Mistral-7B-Instruct-v0.3-GGUF/resolve/main/Mistral-7B-Instruct-v0.3-Q4_K_M.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 4_370 * 1024 * 1024,
            ram_required_mb: 5_300,
            context_len: 8192,
            params_b: 7.2,
            blurb: "General-purpose 7B. Reliable but slower than Qwen Coder on code.".into(),
        },
        OfflineModel {
            id: "llama-3.1-8b-instruct-q4km".into(),
            name: "Llama 3.1 8B Instruct".into(),
            url: "https://huggingface.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 4_920 * 1024 * 1024,
            ram_required_mb: 5_900,
            context_len: 8192,
            params_b: 8.0,
            blurb: "Highest quality offline option. Expect 3-6 tokens/sec.".into(),
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_tokens: i64,
    pub blurb: String,
    /// USD per million input / output tokens, for the cost hint in the UI.
    pub input_price: f32,
    pub output_price: f32,
}

pub fn cloud_catalog() -> Vec<CloudModel> {
    vec![
        CloudModel {
            id: "claude-opus-5".into(),
            name: "Claude Opus 5".into(),
            provider: Provider::Anthropic.as_str().into(),
            context_tokens: 1_000_000,
            blurb: "Best agentic coding model available. The default for cloud VONs.".into(),
            input_price: 5.0,
            output_price: 25.0,
        },
        CloudModel {
            id: "claude-sonnet-5".into(),
            name: "Claude Sonnet 5".into(),
            provider: Provider::Anthropic.as_str().into(),
            context_tokens: 1_000_000,
            blurb: "Near-Opus quality on coding at lower cost.".into(),
            input_price: 3.0,
            output_price: 15.0,
        },
        CloudModel {
            id: "claude-haiku-4-5".into(),
            name: "Claude Haiku 4.5".into(),
            provider: Provider::Anthropic.as_str().into(),
            context_tokens: 200_000,
            blurb: "Fastest and cheapest. Good for small edits and quick questions.".into(),
            input_price: 1.0,
            output_price: 5.0,
        },
        CloudModel {
            id: "gpt-4o".into(),
            name: "GPT-4o".into(),
            provider: Provider::OpenAi.as_str().into(),
            context_tokens: 128_000,
            blurb: "OpenAI's general-purpose multimodal model.".into(),
            input_price: 2.5,
            output_price: 10.0,
        },
        CloudModel {
            id: "gpt-4o-mini".into(),
            name: "GPT-4o mini".into(),
            provider: Provider::OpenAi.as_str().into(),
            context_tokens: 128_000,
            blurb: "Cheap and fast; weaker at long tool-use chains.".into(),
            input_price: 0.15,
            output_price: 0.6,
        },
        CloudModel {
            id: "gemini-2.0-flash".into(),
            name: "Gemini 2.0 Flash".into(),
            provider: Provider::Gemini.as_str().into(),
            context_tokens: 1_000_000,
            blurb: "Very fast with a large context window.".into(),
            input_price: 0.1,
            output_price: 0.4,
        },
    ]
}

/// Registers the catalog in SQLite, preserving download state for entries the
/// user already has on disk.
pub fn seed_catalog() -> Result<()> {
    let existing = db::list_local_models()?;
    for model in offline_catalog() {
        let prior = existing.iter().find(|m| m.id == model.id);
        db::upsert_local_model(&LocalModelRow {
            id: model.id.clone(),
            name: model.name.clone(),
            url: model.url.clone(),
            quant: model.quant.clone(),
            size_bytes: model.size_bytes,
            ram_required_mb: model.ram_required_mb,
            context_len: model.context_len,
            file_path: prior.map(|p| p.file_path.clone()).unwrap_or_default(),
            downloaded: prior.map(|p| p.downloaded).unwrap_or(false),
        })?;
    }
    Ok(())
}

/// The red / yellow / green indicator on each model card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compatibility {
    /// Comfortable headroom.
    Green,
    /// Will run, but slowly or with little room to spare.
    Yellow,
    /// Should not be attempted on this device.
    Red,
}

impl Compatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
        }
    }
}

/// Judges a model against a measured device profile.
///
/// RAM is the hard gate — a model that does not fit will OOM-kill the app no
/// matter how fast the CPU is. Tier only downgrades green to yellow, because a
/// slow-but-working model is a user's choice to make.
pub fn compatibility(model: &OfflineModel, profile: &DeviceProfile) -> (Compatibility, String) {
    let total_mb = profile.total_ram_mb as i64;
    let required = model.ram_required_mb;

    if total_mb > 0 && required > (total_mb * 70) / 100 {
        return (
            Compatibility::Red,
            format!(
                "Needs ~{:.1}GB but this device has {:.1}GB total.",
                required as f32 / 1024.0,
                total_mb as f32 / 1024.0
            ),
        );
    }
    if total_mb > 0 && required > (total_mb * 45) / 100 {
        return (
            Compatibility::Yellow,
            format!(
                "Tight fit at ~{:.1}GB. Close other apps before running.",
                required as f32 / 1024.0
            ),
        );
    }
    if model.params_b > 6.0 && profile.tier < DeviceTier::Flagship {
        return (
            Compatibility::Yellow,
            "Fits in memory, but expect slow generation on this CPU.".into(),
        );
    }
    if model.params_b > 3.0 && profile.tier == DeviceTier::Low {
        return (
            Compatibility::Yellow,
            "Will run, but slowly. A smaller model will feel much better.".into(),
        );
    }
    (
        Compatibility::Green,
        format!("Runs comfortably (~{:.1}GB).", required as f32 / 1024.0),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub received_bytes: i64,
    pub total_bytes: i64,
    pub done: bool,
    pub error: Option<String>,
}

/// Streams a GGUF file to disk, reporting progress.
///
/// Downloads to `<name>.part` and renames on completion, so an interrupted
/// download can never be mistaken for a usable model file. Resumes with a
/// `Range` request when a partial file is already present — these are multi-
/// gigabyte downloads on a mobile connection.
pub async fn download_model(
    model_id: &str,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let row = db::get_local_model(model_id)?
        .ok_or_else(|| VonError::NotFound(format!("model '{model_id}'")))?;

    let dir = &state::paths()?.models;
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(format!("{model_id}.gguf"));
    let part_path = dir.join(format!("{model_id}.gguf.part"));

    if final_path.is_file() {
        mark_downloaded(&row, &final_path)?;
        on_progress(DownloadProgress {
            model_id: model_id.to_string(),
            received_bytes: row.size_bytes,
            total_bytes: row.size_bytes,
            done: true,
            error: None,
        });
        return Ok(final_path);
    }

    let resume_from = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);

    let mut request = state::http().get(&row.url);
    if resume_from > 0 {
        request = request.header("range", format!("bytes={resume_from}-"));
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(VonError::Provider {
            provider: "download".into(),
            status: status.as_u16(),
            body: format!("could not download {}", row.name),
        });
    }

    // A server that ignores our Range header restarts the file from zero.
    let resuming = status.as_u16() == 206;
    let mut received = if resuming { resume_from } else { 0 };
    let total = response
        .content_length()
        .map(|len| received + len)
        .unwrap_or(row.size_bytes.max(0) as u64);

    let mut file = if resuming {
        std::fs::OpenOptions::new().append(true).open(&part_path)?
    } else {
        std::fs::File::create(&part_path)?
    };

    let mut stream = response.bytes_stream();
    let mut last_report = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        received += chunk.len() as u64;

        // Throttle progress events: at ~1MB chunks an unthrottled stream would
        // push thousands of frames through the FFI bridge per download.
        if last_report.elapsed() >= std::time::Duration::from_millis(250) {
            last_report = std::time::Instant::now();
            on_progress(DownloadProgress {
                model_id: model_id.to_string(),
                received_bytes: received as i64,
                total_bytes: total as i64,
                done: false,
                error: None,
            });
        }
    }
    file.sync_all()?;
    drop(file);

    let written = std::fs::metadata(&part_path)?.len();
    if total > 0 && written < total {
        return Err(VonError::Network(format!(
            "download ended early ({written} of {total} bytes); tap download again to resume"
        )));
    }

    std::fs::rename(&part_path, &final_path)?;
    mark_downloaded(&row, &final_path)?;

    on_progress(DownloadProgress {
        model_id: model_id.to_string(),
        received_bytes: written as i64,
        total_bytes: written as i64,
        done: true,
        error: None,
    });
    Ok(final_path)
}

fn mark_downloaded(row: &LocalModelRow, path: &PathBuf) -> Result<()> {
    let mut updated = row.clone();
    updated.file_path = path.to_string_lossy().to_string();
    updated.downloaded = true;
    updated.size_bytes = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(row.size_bytes);
    db::upsert_local_model(&updated)
}

/// Deletes a downloaded model's weights to reclaim storage (PRD Page 2).
pub fn delete_model(model_id: &str) -> Result<()> {
    let row = db::get_local_model(model_id)?
        .ok_or_else(|| VonError::NotFound(format!("model '{model_id}'")))?;

    // Drop it from memory first — deleting the file under a live mmap is how
    // you get a SIGBUS mid-generation.
    if db::get_setting("local_model_id")?.as_deref() == Some(model_id) {
        crate::llm::local::release_model();
    }
    for path in [
        PathBuf::from(&row.file_path),
        state::paths()?.models.join(format!("{model_id}.gguf.part")),
    ] {
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
    }

    let mut updated = row;
    updated.file_path = String::new();
    updated.downloaded = false;
    db::upsert_local_model(&updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(total_ram_mb: u64, tier: DeviceTier) -> DeviceProfile {
        DeviceProfile {
            tier,
            cores: 8,
            total_ram_mb,
            available_ram_mb: total_ram_mb / 2,
            score: 100.0,
            max_local_params_b: 8.0,
            notes: Vec::new(),
        }
    }

    fn model(id: &str, ram_mb: i64, params_b: f32) -> OfflineModel {
        OfflineModel {
            id: id.into(),
            name: id.into(),
            url: String::new(),
            quant: "Q4_K_M".into(),
            size_bytes: ram_mb * 1024 * 1024,
            ram_required_mb: ram_mb,
            context_len: 4096,
            params_b,
            blurb: String::new(),
        }
    }

    #[test]
    fn catalog_ids_are_unique_and_urls_present() {
        let models = offline_catalog();
        let mut ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate model id in the catalog");
        assert!(models.iter().all(|m| m.url.starts_with("https://")));
        assert!(models.iter().all(|m| m.quant == "Q4_K_M"));
    }

    #[test]
    fn ram_is_the_hard_gate() {
        // 8B model on a 4GB phone: refuse outright.
        let (verdict, why) = compatibility(&model("big", 5_900, 8.0), &profile(4_096, DeviceTier::Mid));
        assert_eq!(verdict, Compatibility::Red);
        assert!(why.contains("this device has"));
    }

    #[test]
    fn tight_fits_warn_rather_than_block() {
        let (verdict, _) =
            compatibility(&model("mid", 3_000, 3.8), &profile(6_144, DeviceTier::Mid));
        assert_eq!(verdict, Compatibility::Yellow);
    }

    #[test]
    fn a_big_model_on_a_slow_cpu_is_yellow_not_green() {
        let (verdict, why) =
            compatibility(&model("7b", 5_600, 7.6), &profile(16_384, DeviceTier::Mid));
        assert_eq!(verdict, Compatibility::Yellow);
        assert!(why.contains("slow"));
    }

    #[test]
    fn small_models_are_green_on_capable_devices() {
        let (verdict, _) =
            compatibility(&model("small", 700, 0.5), &profile(8_192, DeviceTier::Flagship));
        assert_eq!(verdict, Compatibility::Green);
    }

    #[test]
    fn cloud_catalog_providers_all_parse() {
        for m in cloud_catalog() {
            assert!(Provider::parse(&m.provider).is_ok(), "{}", m.provider);
        }
    }
}
