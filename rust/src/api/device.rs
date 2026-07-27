//! Device Profiler (PRD Page 4) and the live stats feed.

use flutter_rust_bridge::frb;

use crate::frb_generated::StreamSink;

use crate::api::types::{DeviceProfileDto, DeviceStatsDto, VonException};
use crate::error::VonError;
use crate::{db, governor};

/// Runs the benchmark behind the Device Profiler sheet and stores the result.
///
/// Blocking for its full duration by design — the sheet shows a filling ring
/// while it runs, and the measurement is only meaningful if nothing else in the
/// app is competing for cores.
pub fn run_device_benchmark(duration_ms: u64) -> Result<DeviceProfileDto, VonException> {
    let profile = governor::run_benchmark(duration_ms);

    db::save_device_profile(&db::DeviceProfileRow {
        tier: profile.tier.as_i32(),
        cores: profile.cores as i32,
        total_ram_mb: profile.total_ram_mb as i64,
        score: profile.score,
        measured_at: chrono::Utc::now().timestamp_millis(),
    })
    .map_err(VonException::from)?;

    Ok(DeviceProfileDto::from(profile))
}

/// Current stats without waiting for the next poll.
#[frb(sync)]
pub fn device_stats() -> DeviceStatsDto {
    DeviceStatsDto::from(governor::stats())
}

/// Streams stats for the live readout in the Model Hub and workspace header.
///
/// Emits only when something actually changed, so an idle screen isn't
/// re-rendering a battery percentage every second.
pub fn watch_device_stats(sink: StreamSink<DeviceStatsDto>) -> Result<(), VonException> {
    std::thread::Builder::new()
        .name("von-stats-feed".into())
        .spawn(move || {
            let mut last_generation = u32::MAX;
            loop {
                let generation = governor::generation();
                if generation != last_generation {
                    last_generation = generation;
                    // A closed sink means Dart dropped the subscription — the
                    // only signal we get that the screen is gone.
                    if sink.add(DeviceStatsDto::from(governor::stats())).is_err() {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(750));
            }
        })
        .map_err(|e| VonException::from(VonError::Other(e.to_string())))?;
    Ok(())
}

/// Whether a task should be allowed to start on this hardware (PRD Page 4:
/// "disables the action, offering a cloud alternative").
///
/// Returns an empty string when the task is fine, or the reason it is blocked.
/// `weight` is the task's demand: `light`, `moderate`, or `heavy`.
pub fn check_task_allowed(weight: String, provider: String) -> Result<String, VonException> {
    // Cloud work costs the device almost nothing; never block it on hardware.
    if provider != "local" {
        return Ok(String::new());
    }

    let tier = db::load_device_profile()
        .map_err(VonException::from)?
        .map(|row| governor::DeviceTier::from_i32(row.tier))
        .unwrap_or(governor::DeviceTier::Mid);
    let stats = governor::stats();

    if stats.thermal >= governor::ThermalState::Severe {
        return Ok(format!(
            "The device is at {:.0}°C and needs to cool down. Switch this project to a cloud \
             model to keep working.",
            stats.temperature_c
        ));
    }

    match weight.as_str() {
        "heavy" if tier == governor::DeviceTier::Low => Ok(
            "This is a Tier 1 device — a task this heavy will not finish on-device. Use a cloud \
             model instead."
                .into(),
        ),
        "heavy" if stats.available_ram_mb > 0 && stats.available_ram_mb < 1_200 => Ok(format!(
            "Only {}MB of RAM is free. Close some apps, or use a cloud model.",
            stats.available_ram_mb
        )),
        "moderate" if tier == governor::DeviceTier::Low && stats.available_ram_mb < 600 => Ok(
            "This device is low on memory right now. A cloud model will be much more reliable."
                .into(),
        ),
        _ => Ok(String::new()),
    }
}
