//! Code execution (SWE §3.4).
//!
//! Shipping a real Python or Node runtime on Android means bundling a ~40MB
//! interpreter, an APK that fights Play's size limits, and a process the OS can
//! kill independently of us. Instead there are two contained backends:
//!
//! * **Wasmtime** — runs `.wasm` the agent compiled (Rust/C/AssemblyScript),
//!   with WASI preopened *only* on the project sandbox, a hard memory ceiling,
//!   and epoch-based interruption for the timeout.
//! * **RustPython** — an embedded pure-Rust interpreter for Python the agent
//!   writes directly. No `subprocess`, no `socket`, no host escape surface.
//!
//! Both are gated by the governor: a thermally throttled device does not get to
//! spin up a compile-and-run cycle.

use serde::{Deserialize, Serialize};

use crate::error::{Result, VonError};
use crate::governor;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::sandbox::Sandbox;

/// Ceiling on guest linear memory (SWE §3.4 specifies 128MB).
pub const MAX_WASM_MEMORY_BYTES: usize = 128 * 1024 * 1024;
/// Default wall-clock budget for one execution.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// The most any single execution may be given, on a device with room to spare.
///
/// A minute rather than the old flat thirty seconds. Thirty was chosen to
/// protect the battery, which is the right instinct through the wrong
/// mechanism — it charged a cold, plugged-in flagship the same as a hot budget
/// phone, and killed legitimate work on both. The governor knows the
/// difference, so the ceiling asks it.
///
/// Not the two minutes this first shipped as. A program that overruns cannot be
/// killed — the interpreter thread is detached and runs to completion — so the
/// budget is also how long a runaway keeps a phone's memory. Two minutes of
/// that, three prompts in a row, is an out-of-memory kill, which the user
/// experiences as the app vanishing.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

/// The ceiling when the device is warm, low on memory, or nearly flat.
pub const THROTTLED_TIMEOUT_MS: u64 = 20_000;

// Checked at compile time rather than in a test, matching the loop bounds in
// the agent runner: a throttled ceiling above the healthy one would mean a hot
// phone is allowed to work *longer*, which no test run should be needed to
// catch.
const _: () = assert!(THROTTLED_TIMEOUT_MS < MAX_TIMEOUT_MS);
const _: () = assert!(DEFAULT_TIMEOUT_MS <= THROTTLED_TIMEOUT_MS);
/// Captured output is truncated to this before going into the model's context —
/// a runaway print loop must not blow the context window.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Interpreter threads still running, including ones already given up on.
///
/// A Python execution that overruns its budget cannot be killed: the thread is
/// detached and left to finish, because tearing down the VM mid-allocation is
/// worse than waiting. That is survivable once and compounding thereafter —
/// three overruns in a row is three interpreters competing for the memory of a
/// phone, and Android resolves that by killing the app.
///
/// So a new execution refuses to start while an old one is still going, and
/// says so in a way the agent can act on.
static LIVE_INTERPRETERS: AtomicUsize = AtomicUsize::new(0);

/// Guard that keeps the count honest across every exit path, including a panic
/// inside the interpreter.
struct InterpreterSlot;

impl InterpreterSlot {
    fn take() -> Self {
        LIVE_INTERPRETERS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for InterpreterSlot {
    fn drop(&mut self) {
        LIVE_INTERPRETERS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub truncated: bool,
    /// Which backend ran it: `wasm` or `python`.
    pub engine: String,
}

impl ExecOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }

    /// Rendered form fed back to the model as an observation.
    pub fn as_observation(&self) -> String {
        let mut out = String::new();
        if self.timed_out {
            out.push_str(&format!(
                "TIMEOUT after {}ms — the program was killed.\n",
                self.duration_ms
            ));
        } else {
            out.push_str(&format!(
                "exit code {} ({}ms)\n",
                self.exit_code, self.duration_ms
            ));
        }
        if !self.stdout.is_empty() {
            out.push_str(&format!("--- stdout ---\n{}\n", self.stdout));
        }
        if !self.stderr.is_empty() {
            out.push_str(&format!("--- stderr ---\n{}\n", self.stderr));
        }
        if self.truncated {
            out.push_str("(output truncated)\n");
        }
        if out.trim().is_empty() {
            out.push_str("(no output)\n");
        }
        out
    }
}

fn truncate(mut s: String) -> (String, bool) {
    if s.len() <= MAX_OUTPUT_BYTES {
        return (s, false);
    }
    // Keep the head and the tail: the head has the program's normal output, the
    // tail usually has the traceback that explains the failure.
    let head = MAX_OUTPUT_BYTES * 2 / 3;
    let tail = MAX_OUTPUT_BYTES - head;
    let head_end = floor_char_boundary(&s, head);
    let tail_start = ceil_char_boundary(&s, s.len() - tail);
    s = format!(
        "{}\n\n... [{} bytes elided] ...\n\n{}",
        &s[..head_end],
        tail_start - head_end,
        &s[tail_start..]
    );
    (s, true)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// How long this execution may run, given what the device can currently afford.
///
/// A budget the agent asked for is honoured up to a ceiling that moves with the
/// device: a cool phone with charge to spare gets the full two minutes, a warm
/// or nearly flat one gets twenty seconds. The agent is told which it got, so a
/// timeout on a hot device reads as "the device is busy" rather than "your code
/// is too slow".
fn effective_timeout(requested: Option<u64>) -> u64 {
    requested.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(100, timeout_ceiling())
}

fn timeout_ceiling() -> u64 {
    let stats = governor::stats();

    // Anything above Normal means the SoC is already shedding heat it cannot
    // lose fast enough; a two-minute burn is how a warm phone becomes a hot one.
    if stats.thermal > governor::ThermalState::Normal {
        return THROTTLED_TIMEOUT_MS;
    }
    if stats.available_ram_mb > 0 && stats.available_ram_mb < 400 {
        return THROTTLED_TIMEOUT_MS;
    }
    // -1 means the platform did not tell us, which is not the same as flat.
    if stats.battery_pct >= 0 && stats.battery_pct < 20 {
        return THROTTLED_TIMEOUT_MS;
    }
    MAX_TIMEOUT_MS
}

/// Shared preflight: refuse to start work the device cannot afford.
fn preflight() -> Result<()> {
    if LIVE_INTERPRETERS.load(Ordering::SeqCst) > 0 {
        return Err(VonError::Throttled(
            "a previous run is still going — it overran its time limit and cannot be stopped \
             early. Wait for it to finish, or make the program smaller so it fits the budget."
                .into(),
        ));
    }

    let stats = governor::stats();
    if stats.thermal >= governor::ThermalState::Severe {
        return Err(VonError::Throttled(format!(
            "device is {} ({:.0}°C); not starting a new execution",
            stats.thermal.as_str(),
            stats.temperature_c
        )));
    }
    if stats.available_ram_mb > 0 && stats.available_ram_mb < 150 {
        return Err(VonError::Throttled(format!(
            "only {}MB RAM free; not starting a new execution",
            stats.available_ram_mb
        )));
    }
    Ok(())
}

// ---- WASM backend -------------------------------------------------------

#[cfg(feature = "wasm-exec")]
mod wasm_backend {
    use super::*;
    use std::time::{Duration, Instant};
    use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
    use wasmtime_wasi::pipe::MemoryOutputPipe;
    use wasmtime_wasi::preview1::{self, WasiP1Ctx};
    use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

    struct Host {
        wasi: WasiP1Ctx,
        limits: StoreLimits,
    }

    pub fn run(
        sandbox: &Sandbox,
        wasm_rel_path: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> Result<ExecOutcome> {
        let wasm_path = sandbox.resolve(wasm_rel_path)?;
        if !wasm_path.is_file() {
            return Err(VonError::NotFound(format!(
                "'{wasm_rel_path}' does not exist; compile it first"
            )));
        }
        let bytes = std::fs::read(&wasm_path)?;
        if bytes.len() < 4 || &bytes[..4] != b"\0asm" {
            return Err(VonError::InvalidArgument(format!(
                "'{wasm_rel_path}' is not a WebAssembly module"
            )));
        }

        let mut config = Config::new();
        // Epoch interruption is how we stop an infinite loop: a ticker thread
        // bumps the epoch and the guest traps at the next safepoint. Cheaper
        // than fuel metering, which taxes every instruction.
        config.epoch_interruption(true);
        config.wasm_backtrace(true);
        // No JIT-spray surface and a smaller RSS on a phone.
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        config.max_wasm_stack(1024 * 1024);

        let engine = Engine::new(&config)
            .map_err(|e| VonError::Execution(format!("wasm engine: {e}")))?;

        let stdout = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);

        let mut builder = WasiCtxBuilder::new();
        builder
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .arg(wasm_rel_path);
        for arg in args {
            builder.arg(arg);
        }
        // The *only* host directory the guest can see, mounted at `/sandbox`.
        // No environment is inherited: no API keys, no device paths.
        builder
            .preopened_dir(
                sandbox.root(),
                "/sandbox",
                DirPerms::all(),
                FilePerms::all(),
            )
            .map_err(|e| VonError::Execution(format!("preopen sandbox: {e}")))?;

        let host = Host {
            wasi: builder.build_p1(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_WASM_MEMORY_BYTES)
                .instances(16)
                .tables(64)
                .build(),
        };

        let mut store = Store::new(&engine, host);
        store.limiter(|h| &mut h.limits);
        // One "tick" per 100ms; trap once we have burned the whole budget.
        let ticks = (timeout_ms / 100).max(1);
        store.set_epoch_deadline(ticks);

        let mut linker: Linker<Host> = Linker::new(&engine);
        preview1::add_to_linker_sync(&mut linker, |h: &mut Host| &mut h.wasi)
            .map_err(|e| VonError::Execution(format!("link wasi: {e}")))?;

        let module = Module::new(&engine, &bytes)
            .map_err(|e| VonError::Execution(format!("invalid module: {e}")))?;

        // Ticker thread drives the epoch clock; it exits with the guard.
        let ticker_engine = engine.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticker_stop = stop.clone();
        let ticker = std::thread::spawn(move || {
            while !ticker_stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                ticker_engine.increment_epoch();
            }
        });

        let started = Instant::now();
        let result = (|| -> std::result::Result<(), wasmtime::Error> {
            let instance = linker.instantiate(&mut store, &module)?;
            let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
            start.call(&mut store, ())
        })();
        let duration_ms = started.elapsed().as_millis() as u64;

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = ticker.join();

        // Drop the store so the pipes are no longer borrowed before reading.
        drop(store);
        let out = String::from_utf8_lossy(&stdout.contents()).to_string();
        let mut err = String::from_utf8_lossy(&stderr.contents()).to_string();

        let mut exit_code = 0;
        let mut timed_out = false;

        if let Err(e) = result {
            // A clean `exit(n)` surfaces as an I32Exit trap, not a failure.
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                exit_code = exit.0;
            } else if e.to_string().contains("epoch deadline")
                || matches!(
                    e.downcast_ref::<wasmtime::Trap>(),
                    Some(wasmtime::Trap::Interrupt)
                )
            {
                timed_out = true;
                exit_code = 124;
            } else {
                exit_code = 1;
                if !err.is_empty() {
                    err.push('\n');
                }
                err.push_str(&format!("{e:?}"));
            }
        }

        let (stdout_s, t1) = truncate(out);
        let (stderr_s, t2) = truncate(err);

        Ok(ExecOutcome {
            stdout: stdout_s,
            stderr: stderr_s,
            exit_code,
            duration_ms,
            timed_out,
            truncated: t1 || t2,
            engine: "wasm".into(),
        })
    }
}

#[cfg(not(feature = "wasm-exec"))]
mod wasm_backend {
    use super::*;
    pub fn run(_: &Sandbox, _: &str, _: &[String], _: u64) -> Result<ExecOutcome> {
        Err(VonError::Execution(
            "this build was compiled without the wasm-exec feature".into(),
        ))
    }
}

// ---- Python backend -----------------------------------------------------

#[cfg(feature = "python-exec")]
mod python_backend {
    use super::*;
    use std::time::{Duration, Instant};
    use std::sync::mpsc;

    /// Prelude injected before the agent's code.
    ///
    /// Two jobs: capture output (RustPython writes to the host stdout
    /// otherwise, which on Android goes to logcat where we cannot read it), and
    /// install a cooperative watchdog. `settrace` fires per call, so a
    /// call-free hot loop slips past it — the detached-thread timeout below is
    /// the real backstop. It exists to give the agent a clean `TimeoutError`
    /// traceback in the common case, which is far more useful than an opaque
    /// kill.
    ///
    /// `sys.stdout` must stay a *native* object. It was briefly a small Python
    /// class that mirrored writes to a file, so the host could tail them and
    /// show output as it was produced. That made every `print()` a Python-level
    /// call, and every Python-level call fires the `settrace` hook above —
    /// which is itself Python, and calls `time.monotonic()`. On a VM that does
    /// not suppress tracing inside the trace function that recurses until the
    /// stack is gone, and the app dies instantly and natively the moment the
    /// program prints anything. Streaming is not worth putting a Python frame
    /// in that path.
    const PRELUDE: &str = r#"
import sys, io, time
__af_out = io.StringIO()
__af_err = io.StringIO()
sys.stdout = __af_out
sys.stderr = __af_err
__af_deadline = time.monotonic() + __AF_TIMEOUT_S__
def __af_watchdog(frame, event, arg):
    if time.monotonic() > __af_deadline:
        raise TimeoutError("execution exceeded __AF_TIMEOUT_S__s")
    return __af_watchdog
sys.settrace(__af_watchdog)
"#;

    const EPILOGUE: &str = r#"
sys.settrace(None)
__af_result = (__af_out.getvalue(), __af_err.getvalue())
"#;

    pub fn run(
        sandbox: &Sandbox,
        code: &str,
        timeout_ms: u64,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<ExecOutcome> {
        let timeout_s = timeout_ms as f64 / 1000.0;

        let program = format!(
            "{}\ntry:\n{}\nexcept SystemExit as __af_e:\n    __af_exit = __af_e.code or 0\nexcept BaseException:\n    import traceback; traceback.print_exc(); __af_exit = 1\nelse:\n    __af_exit = 0\n{}",
            PRELUDE.replace("__AF_TIMEOUT_S__", &timeout_s.to_string()),
            indent(code),
            EPILOGUE
        );

        let root = sandbox.root().to_path_buf();
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();

        // Own thread so the timeout is enforceable and a deep recursion blows
        // this stack rather than the caller's.
        std::thread::Builder::new()
            .name("von-python".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                // Dropped when this thread ends, which for an overrun is long
                // after the caller stopped waiting — that is exactly the window
                // a second execution must not start in.
                let _slot = InterpreterSlot::take();
                let result = execute(&program, &root);
                // Receiver may already be gone after a timeout; that is fine.
                let _ = tx.send(result);
            })
            .map_err(|e| VonError::Execution(e.to_string()))?;

        // Polled rather than waited on, so the run can report that it is still
        // going. A minute of blank screen is the same complaint as a model that
        // thinks in silence — but the report is a clock rather than the
        // program's output, because putting a Python object in the stdout path
        // is what made this crash. Elapsed time claims nothing it cannot see.
        let deadline = Duration::from_millis(timeout_ms + 500);
        let mut announced = 0u64;
        let outcome = loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(result) => break Ok(result),
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let seconds = started.elapsed().as_secs();
                    if seconds > announced {
                        announced = seconds;
                        on_progress(&format!(
                            "running… {seconds}s of {}s",
                            timeout_ms / 1000
                        ));
                    }
                    if started.elapsed() >= deadline {
                        break Err(());
                    }
                }
            }
        };

        match outcome {
            Ok((stdout, stderr, exit_code)) => {
                let (stdout, t1) = truncate(stdout);
                let (stderr, t2) = truncate(stderr);
                Ok(ExecOutcome {
                    stdout,
                    stderr,
                    exit_code,
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: false,
                    truncated: t1 || t2,
                    engine: "python".into(),
                })
            }
            Err(()) => {
                // The interpreter thread is detached and still running. It holds
                // no locks the rest of the app needs, and the governor will see
                // the CPU load and throttle; a hard kill would risk leaving the
                // VM's allocator inconsistent.
                tracing::warn!("python execution exceeded {timeout_ms}ms; thread detached");
                Ok(ExecOutcome {
                    stdout: String::new(),
                    stderr: format!("TimeoutError: execution exceeded {timeout_ms}ms"),
                    exit_code: 124,
                    duration_ms: started.elapsed().as_millis() as u64,
                    timed_out: true,
                    truncated: false,
                    engine: "python".into(),
                })
            }
        }
    }

    fn execute(program: &str, root: &std::path::Path) -> (String, String, i32) {
        use rustpython_vm as vm;

        let interpreter = vm::Interpreter::with_init(Default::default(), |vm| {
            // Native modules (math, time, _sre …) and the frozen pure-Python
            // half (io, json, encodings …). Both are needed: with only the
            // former the VM cannot even initialise its codecs, and `import io`
            // fails — which is what the agent's own output capture relies on.
            vm.add_native_modules(rustpython_stdlib::get_module_inits());
            vm.add_frozen(rustpython_pylib::FROZEN_STDLIB);
        });

        interpreter.enter(|vm| {
            let scope = vm.new_scope_with_builtins();

            // Only the project sandbox is importable, so `import helper` finds
            // the agent's own modules and nothing from the host.
            if let Ok(sys_path) = vm.sys_module.get_attr("path", vm) {
                let _ = vm.call_method(
                    &sys_path,
                    "insert",
                    (0, vm.ctx.new_str(root.to_string_lossy().to_string())),
                );
            }

            let compiled = match vm.compile(
                program,
                vm::compiler::Mode::Exec,
                "<agent>".to_owned(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    return (String::new(), format!("SyntaxError: {e}"), 1);
                }
            };

            match vm.run_code_obj(compiled, scope.clone()) {
                Ok(_) => {
                    let out = fetch_str(vm, &scope, "__af_out");
                    let err = fetch_str(vm, &scope, "__af_err");
                    let code = scope
                        .globals
                        .get_item("__af_exit", vm)
                        .ok()
                        .and_then(|v| v.try_into_value::<i32>(vm).ok())
                        .unwrap_or(0);
                    (out, err, code)
                }
                Err(e) => {
                    // `write_exception` wants rustpython's own `py_io::Write`,
                    // which String implements and Vec<u8> does not — the
                    // traceback is already text, so there is nothing to decode.
                    let mut trace = String::new();
                    vm.write_exception(&mut trace, &e).ok();
                    let out = fetch_str(vm, &scope, "__af_out");
                    (out, trace, 1)
                }
            }
        })
    }

    fn fetch_str(
        vm: &rustpython_vm::VirtualMachine,
        scope: &rustpython_vm::scope::Scope,
        name: &str,
    ) -> String {
        scope
            .globals
            .get_item(name, vm)
            .ok()
            .and_then(|buf| vm.call_method(&buf, "getvalue", ()).ok())
            .and_then(|v| v.try_into_value::<String>(vm).ok())
            .unwrap_or_default()
    }

    /// The agent's code goes inside a `try:` block, so it needs indenting.
    fn indent(code: &str) -> String {
        code.lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(not(feature = "python-exec"))]
mod python_backend {
    use super::*;
    pub fn run(
        _: &Sandbox,
        _: &str,
        _: u64,
        _: &mut dyn FnMut(&str),
    ) -> Result<ExecOutcome> {
        Err(VonError::Execution(
            "this build was compiled without the python-exec feature".into(),
        ))
    }
}

// ---- public surface -----------------------------------------------------

/// Runs a `.wasm` module from the sandbox with WASI.
pub fn run_wasm(
    sandbox: &Sandbox,
    wasm_rel_path: &str,
    args: &[String],
    timeout_ms: Option<u64>,
) -> Result<ExecOutcome> {
    preflight()?;
    let _guard = governor::begin_inference();
    wasm_backend::run(sandbox, wasm_rel_path, args, effective_timeout(timeout_ms))
}

/// Runs Python source in the embedded interpreter.
///
/// `on_progress` is called about once a second with how long the program has
/// been running. Not its output: `sys.stdout` has to stay a native object, for
/// the reason spelled out on the prelude above.
pub fn run_python(
    sandbox: &Sandbox,
    code: &str,
    timeout_ms: Option<u64>,
    on_progress: &mut dyn FnMut(&str),
) -> Result<ExecOutcome> {
    preflight()?;
    if code.trim().is_empty() {
        return Err(VonError::InvalidArgument("no code to run".into()));
    }
    let _guard = governor::begin_inference();
    python_backend::run(sandbox, code, effective_timeout(timeout_ms), on_progress)
}

/// Runs a Python file already written into the sandbox.
pub fn run_python_file(
    sandbox: &Sandbox,
    rel_path: &str,
    timeout_ms: Option<u64>,
    on_progress: &mut dyn FnMut(&str),
) -> Result<ExecOutcome> {
    let code = sandbox.read_file(rel_path)?;
    run_python(sandbox, &code, timeout_ms, on_progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::open(dir.path().join("p")).unwrap();
        (dir, sb)
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let long = format!("{}MIDDLE{}", "A".repeat(40_000), "Z".repeat(40_000));
        let (out, was_truncated) = truncate(long);
        assert!(was_truncated);
        assert!(out.starts_with("AAAA"));
        assert!(out.ends_with("ZZZZ"));
        assert!(out.contains("elided"));
    }

    #[test]
    fn timeout_is_clamped_to_sane_bounds() {
        assert_eq!(effective_timeout(None), DEFAULT_TIMEOUT_MS);
        assert_eq!(effective_timeout(Some(0)), 100);
        // The ceiling moves with the device, so assert against what it
        // currently is rather than a constant the governor may have lowered.
        assert_eq!(effective_timeout(Some(10_000_000)), timeout_ceiling());
    }

    #[test]
    fn observation_reports_exit_code_and_streams() {
        let outcome = ExecOutcome {
            stdout: "42\n".into(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 12,
            timed_out: false,
            truncated: false,
            engine: "python".into(),
        };
        let obs = outcome.as_observation();
        assert!(obs.contains("exit code 0"));
        assert!(obs.contains("42"));
        assert!(outcome.succeeded());
    }

    #[cfg(feature = "python-exec")]
    #[test]
    fn python_captures_stdout() {
        let (_d, sb) = sandbox();
        let out = run_python(&sb, "print('hello from the sandbox')", Some(5_000), &mut |_| {}).unwrap();
        assert!(out.stdout.contains("hello from the sandbox"), "{out:?}");
        assert_eq!(out.exit_code, 0);
    }

    #[cfg(feature = "python-exec")]
    /// A long budget is for a device that can afford one. The old flat 30s
    /// charged a cold plugged-in flagship the same as a hot budget phone.
    #[test]
    fn the_timeout_ceiling_follows_the_device() {
        // Nothing has polled the governor in a test process, so the stats are
        // the neutral defaults and the full ceiling applies.
        assert_eq!(timeout_ceiling(), MAX_TIMEOUT_MS);
        // A request under the ceiling is honoured as asked.
        assert_eq!(effective_timeout(Some(45_000)), 45_000);
        // And the floor still holds.
        assert_eq!(effective_timeout(Some(1)), 100);
    }

    #[test]
    fn python_reports_tracebacks_rather_than_erroring_out() {
        let (_d, sb) = sandbox();
        let out = run_python(&sb, "raise ValueError('boom')", Some(5_000), &mut |_| {}).unwrap();
        assert_eq!(out.exit_code, 1);
        assert!(out.stderr.contains("ValueError"), "{out:?}");
    }

    #[cfg(feature = "wasm-exec")]
    #[test]
    fn wasm_rejects_non_wasm_files() {
        let (_d, sb) = sandbox();
        sb.write_file("fake.wasm", "not a module").unwrap();
        assert!(matches!(
            run_wasm(&sb, "fake.wasm", &[], Some(1_000)),
            Err(VonError::InvalidArgument(_))
        ));
    }

    #[test]
    fn execution_refuses_paths_outside_the_sandbox() {
        let (_d, sb) = sandbox();
        assert!(matches!(
            run_wasm(&sb, "../../evil.wasm", &[], Some(1_000)),
            Err(VonError::PermissionDenied(_))
        ));
    }
}
