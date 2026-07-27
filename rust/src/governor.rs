//! The "anti-blast" resource governor (PRD Process D, SWE 3.5).
//!
//! A phone is not a workstation: sustained local inference will heat the SoC
//! until the kernel throttles it, and an OOM kill loses the user's session. The
//! governor is the component that keeps VON from being the reason a
//! device gets hot, so it is authoritative — `llm` and `executor` ask it for
//! permission and thread counts rather than deciding for themselves.
//!
//! Thermal state comes from `AThermalManager` (NDK, API 30+), resolved at
//! runtime via `dlsym` so the library still loads on older devices, where we
//! fall back to reading `/sys/class/thermal/thermal_zone*/temp`.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{Result, VonError};

/// Thermal bands from SWE.md §3.5, plus a `Critical` step above `Severe` for
/// when the platform itself reports shutdown-imminent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ThermalState {
    Normal,
    Light,
    Severe,
    Critical,
}

impl ThermalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Light => "light",
            Self::Severe => "severe",
            Self::Critical => "critical",
        }
    }

    fn from_celsius(c: f32) -> Self {
        if c >= 48.0 {
            Self::Critical
        } else if c >= 43.0 {
            Self::Severe
        } else if c >= 38.0 {
            Self::Light
        } else {
            Self::Normal
        }
    }

    /// Maps `ATHERMAL_STATUS_*` (0..=6) onto our bands.
    ///
    /// Only reachable from the Android thermal implementation; dead on a
    /// host build, where the tests still exercise `from_celsius`.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn from_ndk_status(status: i32) -> Self {
        match status {
            // NONE, LIGHT
            0 | 1 => Self::Normal,
            // MODERATE
            2 => Self::Light,
            // SEVERE
            3 => Self::Severe,
            // CRITICAL, EMERGENCY, SHUTDOWN
            4..=6 => Self::Critical,
            _ => Self::Normal,
        }
    }
}

/// What the governor currently permits. Recomputed on every poll; consumers read
/// it before each token / each execution step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ThrottlePlan {
    /// Threads local inference may use.
    pub threads: u32,
    /// Milliseconds to sleep between generated tokens.
    pub delay_ms: u64,
    /// Local inference must not proceed at all.
    pub paused: bool,
    /// The context cache should be dropped to reclaim RAM.
    pub shed_memory: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DeviceStats {
    pub thermal: ThermalState,
    pub temperature_c: f32,
    pub cpu_cores: u32,
    pub cpu_usage_pct: f32,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub battery_pct: i32,
    pub plan: ThrottlePlan,
}

/// Below this the app aggressively sheds its short-term memory cache rather than
/// letting Android's low-memory killer take the whole process.
const LOW_RAM_MB: u64 = 350;
/// Hard floor: refuse to start new local inference at all.
const CRITICAL_RAM_MB: u64 = 180;

struct Inner {
    stats: RwLock<DeviceStats>,
    /// Set while local inference is running, which tightens the poll interval.
    active: AtomicBool,
    running: AtomicBool,
    /// Monotonic counter so consumers can cheaply detect a change.
    generation: AtomicU32,
    last_poll_ms: AtomicI64,
}

static GOVERNOR: Lazy<Arc<Inner>> = Lazy::new(|| {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
    Arc::new(Inner {
        stats: RwLock::new(DeviceStats {
            thermal: ThermalState::Normal,
            temperature_c: 0.0,
            cpu_cores: cores,
            cpu_usage_pct: 0.0,
            total_ram_mb: 0,
            available_ram_mb: 0,
            battery_pct: -1,
            plan: ThrottlePlan {
                threads: inference_threads(cores),
                delay_ms: 0,
                paused: false,
                shed_memory: false,
            },
        }),
        active: AtomicBool::new(false),
        running: AtomicBool::new(false),
        generation: AtomicU32::new(0),
        last_poll_ms: AtomicI64::new(0),
    })
});

/// Never hand every core to the model — the UI thread, the Flutter raster
/// thread and the OS all need one, and saturating all cores is what makes a
/// phone hot fastest for very little extra throughput.
fn inference_threads(cores: u32) -> u32 {
    match cores {
        0..=2 => 2,
        3..=4 => cores - 1,
        5..=8 => cores - 2,
        _ => (cores * 3) / 4,
    }
    .max(1)
}

/// Starts the background poll loop. Idempotent.
pub fn start() {
    let inner = GOVERNOR.clone();
    if inner.running.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("von-governor".into())
        .spawn(move || {
            let mut sys = sysinfo::System::new_all();
            loop {
                poll_once(&inner, &mut sys);
                // 2s while the model is generating (SWE §3.5), 15s idle (PRD
                // Process D) — polling sysinfo is itself not free.
                let interval = if inner.active.load(Ordering::Relaxed) {
                    Duration::from_secs(2)
                } else {
                    Duration::from_secs(15)
                };
                std::thread::sleep(interval);
            }
        })
        .ok();
}

fn poll_once(inner: &Inner, sys: &mut sysinfo::System) {
    sys.refresh_memory();
    sys.refresh_cpu_usage();

    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let available_ram_mb = sys.available_memory() / (1024 * 1024);
    let cpu_usage_pct = sys.global_cpu_info().cpu_usage();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);

    let (thermal, temperature_c) = read_thermal();
    let battery_pct = read_battery_pct();

    let plan = compute_plan(thermal, cores, available_ram_mb);

    {
        let mut w = inner.stats.write();
        *w = DeviceStats {
            thermal,
            temperature_c,
            cpu_cores: cores,
            cpu_usage_pct,
            total_ram_mb,
            available_ram_mb,
            battery_pct,
            plan,
        };
    }
    inner.generation.fetch_add(1, Ordering::Release);
    inner
        .last_poll_ms
        .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
}

fn compute_plan(thermal: ThermalState, cores: u32, available_ram_mb: u64) -> ThrottlePlan {
    let base = inference_threads(cores);
    let (mut threads, mut delay_ms, mut paused) = match thermal {
        ThermalState::Normal => (base, 0, false),
        // Halve the thread count and space tokens out; this is enough to let a
        // warm SoC settle without the user perceiving a stall.
        ThermalState::Light => ((base / 2).max(1), 10, false),
        ThermalState::Severe => ((base / 2).max(1), 40, true),
        ThermalState::Critical => (1, 100, true),
    };

    // RAM pressure independently constrains us: fewer threads means a smaller
    // scratch working set per token.
    let shed_memory = available_ram_mb > 0 && available_ram_mb < LOW_RAM_MB;
    if shed_memory {
        threads = threads.min(2);
        delay_ms = delay_ms.max(5);
    }
    if available_ram_mb > 0 && available_ram_mb < CRITICAL_RAM_MB {
        paused = true;
    }

    ThrottlePlan {
        threads,
        delay_ms,
        paused,
        shed_memory,
    }
}

pub fn stats() -> DeviceStats {
    *GOVERNOR.stats.read()
}

pub fn plan() -> ThrottlePlan {
    GOVERNOR.stats.read().plan
}

pub fn generation() -> u32 {
    GOVERNOR.generation.load(Ordering::Acquire)
}

/// Marks the start of an inference run; tightens polling and is released by
/// dropping the returned guard.
pub fn begin_inference() -> InferenceGuard {
    GOVERNOR.active.store(true, Ordering::Relaxed);
    InferenceGuard(())
}

pub struct InferenceGuard(());

impl Drop for InferenceGuard {
    fn drop(&mut self) {
        GOVERNOR.active.store(false, Ordering::Relaxed);
    }
}

/// Called before starting a new local generation. Returns the reason when the
/// device is in no state to run one.
pub fn check_can_run_local() -> Result<()> {
    let s = stats();
    if s.plan.paused {
        return Err(VonError::Throttled(match s.thermal {
            ThermalState::Severe | ThermalState::Critical => format!(
                "device is at {:.0}°C ({}) — cooling down before more local inference",
                s.temperature_c,
                s.thermal.as_str()
            ),
            _ => format!(
                "only {}MB RAM available; not enough headroom for local inference",
                s.available_ram_mb
            ),
        }));
    }
    Ok(())
}

/// Applies the current inter-token delay. Called by the local decode loop after
/// every token, so throttling takes effect mid-generation rather than only
/// between prompts.
pub fn pace_token() {
    let delay = GOVERNOR.stats.read().plan.delay_ms;
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }
}

// ---- thermal sources ----------------------------------------------------

#[cfg(target_os = "android")]
mod thermal_impl {
    use super::ThermalState;
    use once_cell::sync::Lazy;
    use std::os::raw::{c_char, c_int, c_void};

    type ThermalNewFn = unsafe extern "C" fn() -> *mut c_void;
    type ThermalStatusFn = unsafe extern "C" fn(*mut c_void) -> c_int;

    /// `AThermalManager` only exists from API 30. Resolving it with `dlsym`
    /// rather than linking means the whole `.so` still loads on API 26-29
    /// devices, which then fall back to sysfs.
    struct NdkThermal {
        manager: *mut c_void,
        status_fn: ThermalStatusFn,
    }

    // The NDK documents AThermalManager as safe to query from any thread.
    unsafe impl Send for NdkThermal {}
    unsafe impl Sync for NdkThermal {}

    static NDK_THERMAL: Lazy<Option<NdkThermal>> = Lazy::new(|| unsafe {
        extern "C" {
            fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        const RTLD_NOW: c_int = 2;

        let handle = dlopen(b"libandroid.so\0".as_ptr() as *const c_char, RTLD_NOW);
        if handle.is_null() {
            return None;
        }
        let new_sym = dlsym(handle, b"AThermalManager_new\0".as_ptr() as *const c_char);
        let status_sym = dlsym(
            handle,
            b"AThermalManager_getCurrentThermalStatus\0".as_ptr() as *const c_char,
        );
        if new_sym.is_null() || status_sym.is_null() {
            return None;
        }
        let new_fn: ThermalNewFn = std::mem::transmute(new_sym);
        let status_fn: ThermalStatusFn = std::mem::transmute(status_sym);
        let manager = new_fn();
        if manager.is_null() {
            return None;
        }
        Some(NdkThermal { manager, status_fn })
    });

    pub fn read() -> (ThermalState, f32) {
        if let Some(ndk) = NDK_THERMAL.as_ref() {
            let status = unsafe { (ndk.status_fn)(ndk.manager) };
            if status >= 0 {
                // The platform API reports a band, not a temperature; pair it
                // with a sysfs reading when one is available so the UI can show
                // a number.
                return (
                    ThermalState::from_ndk_status(status),
                    super::read_sysfs_temp().unwrap_or(0.0),
                );
            }
        }
        match super::read_sysfs_temp() {
            Some(c) => (ThermalState::from_celsius(c), c),
            None => (ThermalState::Normal, 0.0),
        }
    }
}

#[cfg(not(target_os = "android"))]
mod thermal_impl {
    use super::ThermalState;

    pub fn read() -> (ThermalState, f32) {
        match super::read_sysfs_temp() {
            Some(c) => (ThermalState::from_celsius(c), c),
            None => (ThermalState::Normal, 0.0),
        }
    }
}

fn read_thermal() -> (ThermalState, f32) {
    thermal_impl::read()
}

/// Hottest zone across `/sys/class/thermal`. Vendors expose wildly different
/// zone sets, so take the max of anything that reads like a CPU/SoC sensor and
/// falls in a physically plausible range.
fn read_sysfs_temp() -> Option<f32> {
    let dir = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut hottest: Option<f32> = None;
    for entry in dir.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let ty = std::fs::read_to_string(entry.path().join("type")).unwrap_or_default();
        let ty = ty.trim().to_lowercase();
        if !(ty.contains("cpu")
            || ty.contains("soc")
            || ty.contains("tsens")
            || ty.contains("ap")
            || ty.contains("skin"))
        {
            continue;
        }
        let raw = match std::fs::read_to_string(entry.path().join("temp")) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let value: f32 = match raw.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Some zones report millidegrees, some degrees.
        let celsius = if value > 1000.0 { value / 1000.0 } else { value };
        if (0.0..=150.0).contains(&celsius) {
            hottest = Some(hottest.map_or(celsius, |h: f32| h.max(celsius)));
        }
    }
    hottest
}

/// Battery percentage from sysfs. `-1` when unavailable (desktop, or a device
/// that hides the node) so the UI can omit it rather than show a wrong 0%.
fn read_battery_pct() -> i32 {
    for path in [
        "/sys/class/power_supply/battery/capacity",
        "/sys/class/power_supply/BAT0/capacity",
    ] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(v) = raw.trim().parse::<i32>() {
                return v;
            }
        }
    }
    -1
}

// ---- device profiler (PRD Page 4) --------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceTier {
    /// Cloud-only realistically; small local models will crawl.
    Low = 1,
    /// 3B-class local models are comfortable.
    Mid = 2,
    /// 7-8B Q4 local models are usable.
    Flagship = 3,
}

impl DeviceTier {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn from_i32(v: i32) -> Self {
        match v {
            3 => Self::Flagship,
            2 => Self::Mid,
            _ => Self::Low,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Tier 1: Low-End",
            Self::Mid => "Tier 2: Mid-Range",
            Self::Flagship => "Tier 3: Flagship",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub tier: DeviceTier,
    pub cores: u32,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    /// Composite benchmark score; higher is faster.
    pub score: f64,
    /// Largest local model, in billions of parameters at Q4, we'd recommend.
    pub max_local_params_b: f32,
    pub notes: Vec<String>,
}

/// The 5-second benchmark behind the Device Profiler sheet.
///
/// Deliberately integer- and memory-bandwidth-bound rather than a synthetic
/// FLOPS test: quantised GGUF inference on ARM is dominated by int8/int4 dot
/// products and memory throughput, so this correlates far better with real
/// tokens/sec than a floating-point loop would.
pub fn run_benchmark(duration_ms: u64) -> DeviceProfile {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_memory();

    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let available_ram_mb = sys.available_memory() / (1024 * 1024);

    let threads = inference_threads(cores);
    let budget = Duration::from_millis(duration_ms.clamp(1_000, 10_000));
    let started = Instant::now();

    let handles: Vec<_> = (0..threads)
        .map(|t| {
            std::thread::spawn(move || {
                // 1 MiB working set per thread: big enough to miss L1/L2 on most
                // mobile SoCs, so we measure memory bandwidth too.
                let mut buf = vec![0u8; 1024 * 1024];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = (i as u8).wrapping_mul(31).wrapping_add(t as u8);
                }
                let mut iterations: u64 = 0;
                let mut acc: u64 = 0;
                while started.elapsed() < budget {
                    // Strided int accumulate — the shape of a quantised dot product.
                    for chunk in buf.chunks_exact(64) {
                        let mut local: u32 = 0;
                        for (a, b) in chunk.iter().zip(chunk.iter().rev()) {
                            local = local.wrapping_add((*a as u32) * (*b as u32));
                        }
                        acc = acc.wrapping_add(local as u64);
                    }
                    iterations += 1;
                    // Defeat the optimiser without perturbing the measurement.
                    buf[(acc % 1024) as usize] = acc as u8;
                }
                iterations
            })
        })
        .collect();

    let total_iterations: u64 = handles.into_iter().filter_map(|h| h.join().ok()).sum();
    let elapsed_s = started.elapsed().as_secs_f64().max(0.001);
    // Normalised so a mid-range 2023 phone lands near 100.
    let score = (total_iterations as f64 / elapsed_s) * 4.0;

    let mut notes = Vec::new();
    let tier = classify(cores, total_ram_mb, score, &mut notes);

    // RAM is the binding constraint for local models far more often than speed.
    // Q4_K_M is roughly 0.6 GB per billion parameters, plus KV cache and the
    // Flutter process itself; budget ~55% of total RAM.
    let usable_mb = (total_ram_mb as f32) * 0.55;
    let max_local_params_b = (usable_mb / 700.0).clamp(0.0, 14.0);

    if max_local_params_b < 2.0 {
        notes.push(
            "Not enough RAM for a useful local model — cloud providers recommended.".into(),
        );
    } else if max_local_params_b < 4.0 {
        notes.push("Stick to 1-3B models (Phi-3 Mini, Gemma 2B) at Q4.".into());
    } else if max_local_params_b < 8.0 {
        notes.push("7B Q4 models should run, expect 3-6 tokens/sec.".into());
    } else {
        notes.push("8B Q4 models run comfortably.".into());
    }

    let (thermal, temp) = read_thermal();
    if thermal >= ThermalState::Light {
        notes.push(format!(
            "Device already warm ({temp:.0}°C) — benchmark score may understate this hardware."
        ));
    }

    DeviceProfile {
        tier,
        cores,
        total_ram_mb,
        available_ram_mb,
        score,
        max_local_params_b,
        notes,
    }
}

fn classify(cores: u32, total_ram_mb: u64, score: f64, notes: &mut Vec<String>) -> DeviceTier {
    // Any one of these being weak caps the tier — a fast SoC with 3GB of RAM
    // still cannot hold a 7B model.
    let ram_tier = match total_ram_mb {
        0..=3_500 => DeviceTier::Low,
        3_501..=7_000 => DeviceTier::Mid,
        _ => DeviceTier::Flagship,
    };
    let cpu_tier = match (cores, score) {
        (c, s) if c >= 8 && s >= 140.0 => DeviceTier::Flagship,
        (c, s) if c >= 6 && s >= 70.0 => DeviceTier::Mid,
        _ => DeviceTier::Low,
    };
    let tier = ram_tier.min(cpu_tier);

    if ram_tier < cpu_tier {
        notes.push(format!(
            "CPU is capable but {total_ram_mb}MB of RAM is the limiting factor."
        ));
    } else if cpu_tier < ram_tier {
        notes.push("Plenty of RAM, but the CPU will bottleneck local generation.".into());
    }
    tier
}

impl PartialOrd for DeviceTier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeviceTier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as i32).cmp(&(*other as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_bands_match_spec() {
        assert_eq!(ThermalState::from_celsius(30.0), ThermalState::Normal);
        assert_eq!(ThermalState::from_celsius(37.9), ThermalState::Normal);
        assert_eq!(ThermalState::from_celsius(38.0), ThermalState::Light);
        assert_eq!(ThermalState::from_celsius(42.9), ThermalState::Light);
        assert_eq!(ThermalState::from_celsius(43.0), ThermalState::Severe);
        assert_eq!(ThermalState::from_celsius(48.0), ThermalState::Critical);
    }

    #[test]
    fn light_throttling_halves_threads_and_adds_delay() {
        let base = inference_threads(8);
        let plan = compute_plan(ThermalState::Light, 8, 4096);
        assert_eq!(plan.threads, (base / 2).max(1));
        assert_eq!(plan.delay_ms, 10);
        assert!(!plan.paused);
    }

    #[test]
    fn severe_throttling_pauses_inference() {
        assert!(compute_plan(ThermalState::Severe, 8, 4096).paused);
        assert!(compute_plan(ThermalState::Critical, 8, 4096).paused);
    }

    #[test]
    fn low_ram_sheds_memory_and_caps_threads() {
        let plan = compute_plan(ThermalState::Normal, 8, 300);
        assert!(plan.shed_memory);
        assert!(plan.threads <= 2);
        assert!(!plan.paused, "300MB is tight but workable");

        let critical = compute_plan(ThermalState::Normal, 8, 100);
        assert!(critical.paused, "under 180MB we must not start inference");
    }

    #[test]
    fn never_uses_every_core() {
        for cores in 2..=16u32 {
            assert!(
                inference_threads(cores) < cores || cores <= 2,
                "{cores} cores should leave headroom for the UI"
            );
        }
    }

    #[test]
    fn tier_is_capped_by_the_weakest_axis() {
        let mut notes = Vec::new();
        // Fast CPU, tiny RAM -> still Low.
        assert_eq!(classify(8, 3_000, 500.0, &mut notes), DeviceTier::Low);
        // Lots of RAM, slow CPU -> Low.
        assert_eq!(classify(4, 12_000, 10.0, &mut notes), DeviceTier::Low);
        assert_eq!(classify(8, 12_000, 200.0, &mut notes), DeviceTier::Flagship);
    }

    #[test]
    fn benchmark_produces_a_plausible_profile() {
        let profile = run_benchmark(1_000);
        assert!(profile.cores >= 1);
        assert!(profile.score > 0.0);
        assert!(!profile.notes.is_empty());
    }
}
