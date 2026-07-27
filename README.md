# VON

**Local-first AI sandboxes for Android.**

Work happens inside a **VON** — an isolated sandbox holding one task, its files,
and its agent conversation. The agent writes code, runs it, reads the output,
and fixes what breaks, driven either by a GGUF model on the device itself or by
a cloud API, switchable mid-conversation. A VON can only touch itself: not your
other VONs, not the rest of the phone.

Flutter for the UI, Rust for everything underneath. No Java or Kotlin source in
the repository.

## On the word "VON"

It is the product's noun for a sandbox, used consistently in the UI ("VON 3 ·
Sales Dashboard", "Initializing VON…", "Delete VON 3?") and taught to the agent
in its system prompt, so the agent and the user name the same thing the same
way.

**Internally the code still says `project`** — `ProjectRow`, `projectId`,
`create_project`. That split is deliberate. Renaming the storage layer would
churn the schema and every FFI signature for a branding decision, and product
vocabulary is the thing most likely to change again. The one place the two meet
is `ProjectDto.ordinal`, the user-facing VON number.

VON numbers are assigned at creation from `MAX(ordinal) + 1` and never reused.
Deleting VON 2 leaves a gap, which is correct: the number is an identity, not a
position. Renumbering the survivors would silently rename VONs the user already
knows by number, and sorting the grid by "last modified" would make a positional
number jump around on every edit.

---

## Status

All of `PRD.md`, `Design.md`, and `SWE.md` is implemented: the sandbox, the
agent loop, local (`llama.cpp`) and cloud inference, WASM and Python execution,
the thermal governor, the localhost preview server, and all five screens.
Beyond the spec, a project syncs with GitHub as a real commit — a three-way
merge against the branch, from the Workspace tab or by asking for it in the
chat.

It was authored on a machine with no Flutter, Rust, or Android SDK, so CI is
what proves it builds. The Rust core compiles and all 78 tests pass; the APK is
built and published by `.github/workflows/build.yml`.

The version drift predicted here did materialise, and is fixed:

- **`rustpython-stdlib 0.3` vs `rusqlite 0.31`** both declared `links =
  "sqlite3"` with incompatible `libsqlite3-sys` bounds. Cargo permits one
  package per `links` value, so this was unresolvable by feature flags —
  rustpython 0.4 agrees with rusqlite on `^0.28`.
- **`llama-cpp-2`** did move: `sample_token_greedy` is gone in favour of
  `LlamaSampler`, and `token_to_str` gave way to `token_to_piece`. Now written
  against 0.1.152 and pinned there, since `"0.1.54"` means `^0.1.54` and
  silently floated.
- **`flutter_rust_bridge`** does not export `StreamSink` from the runtime
  crate — it is generated — and does not resolve a generic type alias in a
  return position, so `ApiResult<T>` produced junk Dart types.

---

## Architecture

```text
┌─────────────────────────── Flutter (Dart) ───────────────────────────┐
│  ui/          neomorphic widget library, 5 screens, 3 workspace tabs │
│  core/        Riverpod graph, theme (Graphite + Twitter Blue)        │
│  models/      chat state machine, consumes the agent event stream    │
│  services/    CoreApi — the single seam to Rust                      │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ flutter_rust_bridge (FFI + StreamSink)
┌──────────────────────────────┴───────────────────────────────────────┐
│  api/         the whole Dart-facing contract; nothing below is       │
│               reachable from Flutter                                 │
│  agent/       plan → call tools → observe loop, prompts, tool surface│
│  sandbox.rs   path validation, symlink defence, quotas               │
│  llm/         cloud SSE (Anthropic/OpenAI/Gemini), llama.cpp, catalog│
│  executor.rs  Wasmtime (WASI) + embedded RustPython                  │
│  governor.rs  thermal + RAM throttling, device tiering               │
│  server.rs    tiny_http preview on 127.0.0.1                         │
│  github.rs    publish a sandbox as a commit via the Git Data API     │
│  db.rs        SQLite: projects, transcripts, memory, keys            │
└──────────────────────────────────────────────────────────────────────┘
```

### The parts worth reading first

**`rust/src/sandbox.rs`** — the security boundary. Every agent-supplied path
goes through `Sandbox::resolve`, which rejects rather than normalises: lexical
check for `..` and absolute paths, then a component-by-component symlink walk,
then a canonical containment re-check, then quotas. The symlink walk is the
layer that matters — a symlink planted inside the sandbox is otherwise a clean
escape, and it is the one the lexical check alone would miss.

**`rust/src/governor.rs`** — the "anti-blast" component. Polls thermal state
(`AThermalManager` via `dlsym`, falling back to `/sys/class/thermal` on API
< 30) and RAM, and publishes a `ThrottlePlan` that `llm` and `executor` must
consult. Throttling applies *per token*, not per prompt, so a device that heats
up mid-answer slows down immediately. It never hands every core to the model —
the UI and raster threads need one, and saturating all cores is what makes a
phone hot fastest for very little extra throughput.

**`rust/src/agent/runner.rs`** — the loop. Two invariants: every completed turn
is written to SQLite immediately (Android can kill the process between any two
steps), and no tool call is ever left without a result, including on
cancellation — every provider rejects an assistant turn carrying a `tool_use`
with no matching `tool_result`, which would make the whole transcript unusable
on resume.

**`lib/ui/widgets/neumorphic.dart`** — the design system. `NeuSurface` is the
only place shadows are defined; extruded means actionable, concave means
receptive or active. Flutter has no inset `BoxShadow`, so `_InnerShadowPainter`
builds the region outside the squircle, offsets and blurs it, and clips it back
inside.

---

## Getting it on your phone

You do not need a toolchain for this. Push to `main`, let CI build, then:

**<https://github.com/tom32bit/VON/releases/latest>**

Open that on the phone, tap the `.apk`, allow the install when Chrome asks.
The release is a rolling `dev` tag, replaced on every green build of `main`.

Three things to expect:

- **"Unsafe app blocked" / "install unknown apps".** The APK is signed with the
  debug key, because there is no upload keystore in this repo. Allow it for
  your browser once. To sign properly, drop a keystore and an
  `android/key.properties` (`storeFile`, `storePassword`, `keyAlias`,
  `keyPassword`) and the release config picks it up automatically.
- **arm64 only.** Every phone from roughly the last decade. If yours is 32-bit
  it will refuse to install — build locally with `--abi arm` instead.
- **The variant matters.** The APK is named `von-full-*.apk` or
  `von-cloud-only-*.apk`. `cloud-only` means the llama.cpp cross-compile failed
  and on-device models are absent from that build; everything else — sandbox,
  agent loop, Python/WASM execution, preview server, all five screens — is
  identical, and works with an API key from the Model Hub.

For a build without waiting on CI, `./tool/build_rust.sh --abi arm64 --cloud-only`
then `flutter build apk --release --target-platform android-arm64` is by far the
fastest path, since it skips compiling llama.cpp entirely.

---

## Building

Nothing here builds without the toolchain. In order:

```bash
# 1. Rust + the Android targets
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk flutter_rust_bridge_codegen --locked

# 2. NDK (must match ndkVersion in android/app/build.gradle)
sdkmanager "ndk;27.0.12077973"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"

# 3. Generate the FFI bindings — lib/src/rust/ is gitignored, so this is
#    required before anything in lib/ will resolve
flutter pub get
mkdir -p lib/src/rust && flutter_rust_bridge_codegen generate

# 4. Cross-compile the core into android/app/src/main/jniLibs/
./tool/build_rust.sh                 # all ABIs, release
./tool/build_rust.sh --abi arm64     # much faster while iterating

# 5. Run
flutter run
```

**Iterating on Rust:** re-run steps 3 and 4 after any change under
`rust/src/api/`; step 4 alone is enough for changes elsewhere. Hot reload does
not cross the FFI boundary.

### Cargo features

| Feature | Default | What it costs to disable |
| --- | --- | --- |
| `local-llm` | on | No on-device inference; cloud providers still work. Cuts build time a lot — `llama.cpp` is the slow part. |
| `wasm-exec` | on | `run_wasm` returns an error the agent can read. |
| `python-exec` | on | `run_python` returns an error the agent can read. |

`build_features()` reports what shipped, and the Model Hub hides what it cannot
do rather than offering a button that fails.

---

## Testing

```bash
cd rust
# `--no-default-features` drops the `ffi` feature, so the api/ module and its
# generated `frb_generated.rs` are excluded. That is why these run without
# codegen.
cargo test --no-default-features --features wasm-exec,python-exec
cargo test --no-default-features sandbox::   # the escape tests specifically

cd ..
flutter test
```

The Rust tests concentrate on the three places a bug has real consequences:
sandbox escape (path traversal, symlinks, quota accounting), the governor's
throttling thresholds, and the provider wire formats — Anthropic's tool results
ride in a *user* turn, OpenAI wants one message per result, and Gemini rejects
several JSON Schema keywords the others accept. Those are the details that fail
silently in production and are cheap to pin down in a test.

The Flutter tests assert on the extruded/concave state of controls rather than
on golden images: that distinction is the entire visual grammar, and a golden
would break on every unrelated shadow tweak.

CI gates sandbox isolation in its own job so a flaky sibling can't skip it.

---

## Deliberate decisions

**Local-only, no accounts.** PRD Page 1 offers email/Google sign-in; this build
ships only the Local-Only path. The splash goes straight to the dashboard.
Adding auth later touches the splash and one provider — nothing in the core
assumes a user id.

**No shell, no package manager.** The agent gets an embedded Python interpreter
with the standard library and a WASI runtime. Not numpy, not pip. This is stated
plainly in the system prompt, because an agent that discovers the limitation by
failing wastes a turn every time.

**Keys live in app-private SQLite, not the keystore.** They are readable by
anything with root or a backup exploit. Backup and device-transfer are disabled
in `data_extraction_rules.xml` to close the obvious path, but this is a real
trade-off: Android Keystore would be strictly better and is the natural next
change.

**Python timeouts detach rather than kill.** RustPython has no safe way to
interrupt a running interpreter from outside. A `settrace` watchdog catches the
common case with a clean `TimeoutError`; a call-free hot loop escapes it, and
the thread is detached rather than killed — killing it risks leaving the VM's
allocator inconsistent. The governor sees the CPU load and throttles. This is
the weakest guarantee in the system and it is documented in
`executor.rs` where someone changing it will read it.

**Cloud requests carry no sampling parameters.** `temperature`, `top_p`, and
`top_k` are rejected outright by current Anthropic models, and unset is the
right default for an agent everywhere else.

**The file tree crosses FFI flat.** A depth-tagged list, not a recursive struct.
The Sandbox Explorer renders an indented list anyway, and recursion across the
bridge buys nothing.

**`rust/src/github.rs`** — publishing, and the one piece of real distributed-
systems reasoning in the app. It speaks GitHub's Git Data API rather than the
git wire protocol, because the alternatives are libgit2 (a C dependency to
cross-compile for every ABI, with its own TLS stack) and gitoxide (which cannot
push yet).

A sync is a **three-way merge at file granularity**. `github_links` records the
tree VON last pushed for a project, and that base is what makes the interesting
cases decidable: without it, a file present on the remote and absent locally is
equally consistent with "the user deleted it" and "somebody else added it", and
guessing wrong destroys work either way. With it, changed-here uploads,
changed-there is written back into the sandbox, deleted-here deletes, and
changed-in-both refuses by name instead of picking a side. Granularity is the
file, not the line — diff3 on a phone would turn every awkward case into a
half-merged file, which is worse than being told two people edited the same
thing.

Two consequences worth knowing. Blob ids are content-addressed and computed
locally, so a file GitHub already has is never uploaded again — a one-line
change costs one blob, not the project. And the ref update is *not* forced: the
commit's parent is the head read at the start of the sync, so a fast-forward is
guaranteed unless somebody pushed in between, which is exactly the case worth
hearing about.

The filter for `.env`, `node_modules`, private keys and friends lives here
rather than in the sandbox — publishing is the single moment a scratch directory
becomes public. Filtered paths sit outside the merge entirely: they pass through
from the remote untouched and are never written into the sandbox, because
treating "absent locally because we filtered it" as a deletion would erase a
`.env` somebody deliberately committed, on the *second* sync rather than the
first.

---

## Known gaps

- **Never run on a physical device.** It compiles and the tests pass; nobody
  has yet watched the agent write a file on real hardware.
- **Greedy sampling only.** No temperature or top-p on the local path, so local
  output is deterministic and can loop on repetitive prompts. Deliberate for
  now — a sampled token in the middle of a JSON tool call is a wasted turn —
  but it should become configurable.
- **Context shifting is extractive, not summarised.** `shift_context` keeps the
  task statement and recent turns and marks the gap. SWE §8 calls for
  summarising the dropped span with a cloud model; that costs an extra request
  per run and was left out deliberately.
- **No `patrol` integration tests.** SWE §7.1 asks for them on physical
  hardware, which CI cannot provide.
- **Model download URLs are pinned to specific HuggingFace paths.** They rot.
  There is no fallback mirror.
- **Merging is per-file, never per-line.** Two people editing the same file is
  reported as a conflict and refused, even when the edits would not have
  overlapped. Deliberate: see the note on `github.rs` above.
- **A sync holds the changed files in memory.** The tree has to be posted as a
  unit; the sandbox quota is what keeps that bounded.
- **Incoming files are fetched one blob at a time.** Importing a large
  repository is one request per file. A tarball fetch would be one request
  total, but needs a second code path and `tar` support that isn't in the
  dependency set.
- **A truncated tree listing refuses rather than guesses.** GitHub truncates the
  recursive listing for very large repositories, and a truncated answer reads as
  "those files do not exist" — which the merge would act on as deletions.

---

## Layout

```text
von/
├── rust/src/
│   ├── api/          FFI surface: init, projects, models, device, agent,
│   │                 files, github, preview, types
│   ├── agent/        runner (the loop), tools, prompt
│   ├── llm/          mod (transcript + Engine), cloud, local, catalog
│   ├── sandbox.rs  db.rs  governor.rs  server.rs  executor.rs  state.rs
│   ├── github.rs
│   └── error.rs
├── lib/
│   ├── core/         theme, providers
│   ├── models/       chat state machine
│   ├── services/     core_api — the single import seam
│   └── ui/           widgets/, screens/, tabs/, sheets/
├── android/          manifest, gradle, network + backup rules (no JVM source)
├── tool/             build_rust.sh
└── test/             widget and markdown tests
```
