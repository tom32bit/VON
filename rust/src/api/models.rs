//! The Model Hub (PRD Page 2): offline weights, cloud keys, downloads.

use flutter_rust_bridge::frb;

use crate::api::types::{
    CloudModelDto,
    DeviceProfileDto,
    DownloadProgressDto,
    OfflineModelDto,
    VonException,
};
use crate::error::VonError;
use crate::frb_generated::StreamSink;
use crate::llm::{self, catalog, Provider};
use crate::{db, governor, state};

/// Offline models with their download state and a red/yellow/green verdict for
/// this specific device.
pub fn list_offline_models() -> Result<Vec<OfflineModelDto>, VonException> {
    let rows = db::list_local_models().map_err(VonException::from)?;
    let default_id = db::get_setting("local_model_id")
        .map_err(VonException::from)?
        .unwrap_or_default();

    // Use the measured profile when the user has run the benchmark; otherwise
    // fall back to live stats so the indicator is never blank.
    let profile = current_profile();

    Ok(catalog::offline_catalog()
        .into_iter()
        .map(|model| {
            let row = rows.iter().find(|r| r.id == model.id);
            let (verdict, note) = catalog::compatibility(&model, &profile);
            OfflineModelDto {
                downloaded: row.map(|r| r.downloaded).unwrap_or(false),
                is_default: default_id == model.id,
                compatibility: verdict.as_str().to_string(),
                compatibility_note: note,
                id: model.id,
                name: model.name,
                quant: model.quant,
                size_bytes: model.size_bytes,
                ram_required_mb: model.ram_required_mb,
                context_len: model.context_len,
                params_b: model.params_b,
                blurb: model.blurb,
            }
        })
        .collect())
}

fn current_profile() -> governor::DeviceProfile {
    if let Ok(Some(row)) = db::load_device_profile() {
        return governor::DeviceProfile {
            tier: governor::DeviceTier::from_i32(row.tier),
            cores: row.cores as u32,
            total_ram_mb: row.total_ram_mb as u64,
            available_ram_mb: governor::stats().available_ram_mb,
            score: row.score,
            max_local_params_b: 0.0,
            notes: Vec::new(),
        };
    }
    let stats = governor::stats();
    governor::DeviceProfile {
        // Unbenchmarked: assume mid-range so we neither block a capable device
        // nor promise a weak one it can run an 8B model.
        tier: governor::DeviceTier::Mid,
        cores: stats.cpu_cores,
        total_ram_mb: stats.total_ram_mb,
        available_ram_mb: stats.available_ram_mb,
        score: 0.0,
        max_local_params_b: 0.0,
        notes: Vec::new(),
    }
}

pub fn list_cloud_models() -> Result<Vec<CloudModelDto>, VonException> {
    let configured = db::providers_with_keys().map_err(VonException::from)?;
    Ok(catalog::cloud_catalog()
        .into_iter()
        .map(|model| CloudModelDto {
            key_configured: configured.contains(&model.provider),
            id: model.id,
            name: model.name,
            provider: model.provider,
            context_tokens: model.context_tokens,
            blurb: model.blurb,
            input_price: model.input_price,
            output_price: model.output_price,
            curated: true,
        })
        .collect())
}

/// Every model a provider says this key can reach.
///
/// The curated list is a starting point, not a limit — providers ship models
/// faster than an app store review cycle, and an account may have access to
/// something VON has never heard of. Anything recognised keeps its description
/// and prices; the rest arrives with whatever the provider volunteered.
///
/// Without a key there is nobody to ask, so this falls back to the curated
/// entries instead of erroring: an empty picker teaches the user nothing.
pub fn list_provider_models(provider: String) -> Result<Vec<CloudModelDto>, VonException> {
    let parsed = Provider::parse(&provider).map_err(VonException::from)?;
    let configured = db::providers_with_keys().map_err(VonException::from)?;
    let has_key = configured.iter().any(|p| p == &provider);

    let models = if has_key {
        llm::discovery::list_models(parsed).map_err(VonException::from)?
    } else {
        llm::discovery::curated(parsed)
    };

    Ok(models
        .into_iter()
        .map(|model| CloudModelDto {
            id: model.id,
            name: model.name,
            provider: provider.clone(),
            context_tokens: model.context_tokens,
            blurb: model.blurb,
            input_price: model.input_price,
            output_price: model.output_price,
            key_configured: has_key,
            curated: model.curated,
        })
        .collect())
}

/// Downloads a model's weights, streaming progress to the UI.
///
/// Resumable: calling this again after an interrupted download continues from
/// where it stopped rather than starting the multi-gigabyte transfer over.
pub fn download_model(
    sink: StreamSink<DownloadProgressDto>,
    model_id: String,
) -> Result<(), VonException> {
    let result = state::block_on(async {
        catalog::download_model(&model_id, |progress| {
            // A closed sink means the user left the screen; the download still
            // completes so the bytes aren't wasted.
            let _ = sink.add(DownloadProgressDto::from(progress));
        })
        .await
    })
    .map_err(VonException::from)?;

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let exception = VonException::from(e);
            let _ = sink.add(DownloadProgressDto {
                model_id,
                received_bytes: 0,
                total_bytes: 0,
                done: true,
                error: Some(exception.message.clone()),
            });
            Err(exception)
        }
    }
}

/// Deletes downloaded weights to reclaim storage.
pub fn delete_downloaded_model(model_id: String) -> Result<(), VonException> {
    catalog::delete_model(&model_id).map_err(VonException::from)
}

/// Sets the model used by local projects.
pub fn set_default_local_model(model_id: String) -> Result<(), VonException> {
    let row = db::get_local_model(&model_id)
        .map_err(VonException::from)?
        .ok_or_else(|| VonException::from(VonError::NotFound(format!("model '{model_id}'"))))?;
    if !row.downloaded {
        return Err(VonError::InvalidArgument(format!(
            "'{}' has not been downloaded yet",
            row.name
        ))
        .into());
    }
    // A different model means different weights; drop the resident one so the
    // next run doesn't keep using the old file.
    crate::llm::local::release_model();
    db::set_setting("local_model_id", &model_id).map_err(VonException::from)
}

// ---- API keys -----------------------------------------------------------

/// Stores (or clears, with an empty string) a provider's API key.
///
/// Keys live in the app's private SQLite database, which Android keeps outside
/// other apps' reach. They are never written to logs and never leave the device
/// except to the provider they belong to.
pub fn set_api_key(provider: String, key: String) -> Result<(), VonException> {
    Provider::parse(&provider).map_err(VonException::from)?;
    db::set_api_key(&provider, key.trim()).map_err(VonException::from)
}

/// Whether a key is stored — the key itself is never returned to Dart.
pub fn has_api_key(provider: String) -> Result<bool, VonException> {
    Ok(db::get_api_key(&provider)
        .map_err(VonException::from)?
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false))
}

/// Masked form for display, e.g. `sk-ant…4f2a`.
pub fn api_key_hint(provider: String) -> Result<Option<String>, VonException> {
    Ok(db::get_api_key(&provider)
        .map_err(VonException::from)?
        .filter(|k| k.len() > 12)
        .map(|k| format!("{}…{}", &k[..6], &k[k.len() - 4..])))
}

/// "Test connection" button: makes one tiny real request.
pub fn test_api_key(provider: String, key: String, model_id: String) -> Result<(), VonException> {
    let provider = Provider::parse(&provider).map_err(VonException::from)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(VonError::InvalidArgument("enter a key first".into()).into());
    }
    // `model_id` is no longer needed — the key is checked against the model
    // listing rather than by spending tokens on a real completion — but it
    // stays in the signature so the Dart call site does not have to change in
    // the same release.
    let _ = model_id;
    llm::discovery::test_key(provider, key).map_err(VonException::from)
}

/// The measured device profile, if the user has run the benchmark.
#[frb(sync)]
pub fn cached_device_profile() -> Option<DeviceProfileDto> {
    db::load_device_profile()
        .ok()
        .flatten()
        .map(|row| DeviceProfileDto {
            tier: row.tier,
            tier_label: governor::DeviceTier::from_i32(row.tier).label().to_string(),
            cores: row.cores,
            total_ram_mb: row.total_ram_mb,
            available_ram_mb: governor::stats().available_ram_mb as i64,
            score: row.score,
            max_local_params_b: 0.0,
            notes: Vec::new(),
        })
}
