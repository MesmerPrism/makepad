# Morphospace Makepad Fork Agent Notes

This checkout can be used as the maintained Makepad fork branch for the
Morphospace Makepad Quest lane. Upstream Makepad remains the framework source
of truth; the Morphospace Makepad patch branch should stay a shallow patch
queue for Android packaging, Quest/Horizon OS Vulkan window-swapchain
correctness, workspace metadata, and branch-local documentation.

Rusty Morphospace is the top-level project/platform umbrella for the refactor
repo family. This Makepad fork remains a toolkit/adapter dependency, not a
Morphospace authority or module namespace. Keep Morphospace, Matter, Lattice,
Manifold, Optics, Studio, and Quest contracts outside the fork unless a change
is a general Makepad adapter or public example requirement.

For Morphospace Makepad tasks in this repo, read these first:

- `MORPHOSPACE_MAKEPAD_FORK_NOTES.md`
- `MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md`
- `MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md`
- `MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md`
- `MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md`
- `MORPHOSPACE_MAKEPAD_ACTIVITY_SPLIT_PLAN.md`
- Legacy Rusty-XR public docs:
  - `docs/MAKEPAD_FORK_RELATIONSHIP.md`
  - `docs/MAKEPAD_CAMERA_PARALLEL_APPROACH_COMPARISON.md`
  - `docs/MAKEPAD_XR_GPU_PAGE_FAULT_INVESTIGATION.md`
  - `docs/MAKEPAD_STEREO_COMPARISON_ITERATION.md`
- Legacy Rusty-XR example-local instructions:
  - `examples/makepad-q2q-camera-shell/AGENTS.md`
  - `examples/makepad-camera-shell/AGENTS.md`

Those legacy Rusty-XR docs live in the legacy Rusty-XR repo, not in this Makepad checkout.
Do not copy private planning notes, local paths, generated APKs, device logs,
package identities, SDK caches, or downstream tuning into this branch.

## Morphospace Patch Boundaries

Current acceptable Makepad-side changes for the Morphospace Makepad lane are:

- Android `cargo-makepad` packaging fixes needed by the public Makepad Quest
  example lane.
- Dependent Rust shared-library bundling for Android APK output.
- Windows path normalization for generated Android wrapper inputs.
- Targeted Android Vulkan frame-fence waits before destroying/recreating
  swapchain-backed window resources after suboptimal or out-of-date returns.
- Public-safe Android activity/bootstrap phase markers used by the Morphospace
  Makepad Quest validation lane before Camera2 work starts.
- Quest manifest camera permission and optional camera feature declarations
  needed by public examples that exercise Android NDK Camera2 diagnostics.
- Quest manifest camera-permission opt-out through
  `cargo makepad android --variant=quest --quest-camera-permissions=false` for
  generated XR APKs that must keep `MakepadAppXr`/OpenXR metadata but must not
  request Android/headset/spatial camera access.
- Android video-input discovery gating so XR apps that have camera streaming
  disabled can avoid passive `ACameraManager` enumeration while still retaining
  explicit camera/video playback paths when the app opts in.
- Quest manifest XR launch metadata and non-resizeable activity declarations
  used to keep generated Quest XR activities on the immersive path instead of
  Horizon OS volumetric-window handling.
- Quest OpenXR environment-depth fallback behavior when the runtime refuses
  depth provider, depth swapchain, depth image, or depth start calls.
- Makepad `Video` widget camera-permission routing when headset raw-camera
  sources require a different runtime permission from ordinary app cameras.
- Android external H.264 video-source plumbing that stays generic: Manifold
  command-envelope defaults, `/manifold/v1/events`, `RMANVID1` stream framing,
  explicit legacy `RXYRVID1` compatibility, MediaCodec decode, stream-header
  metadata events, and a CPU-YUV decoded handoff for Vulkan/XR paths without a
  GL external texture handle.
- Android Java-message coalescing and dispatch now lives in
  `platform/src/os/linux/android/android_java_messages.rs`. Keep
  `platform/src/os/linux/android/android.rs` focused on Android platform loop,
  backend, surface, camera, and media ownership glue; future message-dispatch
  changes should stay in the child module unless they are backend lifecycle
  helpers with a clearer owner.
- Generic OpenXR hand-mesh bind-data access for runtimes that expose
  `XR_FB_hand_tracking_mesh`, kept as mesh counts/status plus an on-demand API
  rather than app-specific recording behavior.
- Generic XR/Vulkan storage-buffer command/readback probes used by downstream
  app-shell evidence to prove command-buffer submission and bounded readback.
  Keep these APIs data-limited and generic; downstream crates own any
  Quest-Makepad marker contract, and this fork must not define Matter
  field/particle semantics or GPU compute readiness.
- XR/Vulkan compute probes must not turn `platform/src/os/linux/vulkan.rs` into
  a monolithic probe file. Keep the root file to Vulkan instance/device,
  window-swapchain, render-pass/offscreen target, and lifecycle orchestration.
  Keep host-visible buffer allocation, memory-type lookup, upload helpers, and
  geometry-resource cache ownership in `buffer_resources.rs`, reusable
  per-frame buffers plus descriptor-pool allocation/reset/destruction in
  `frame_resources.rs`, draw-list traversal,
  draw-packet assembly, descriptor writes, packet-buffer upload, and draw
  command recording in `draw_recording.rs`, OpenXR multiview
  targets/session/readback/draw in `openxr_targets.rs`, graphics pipeline keys,
  shader descriptor layouts, immutable video sampler keying, shader modules,
  and graphics pipeline creation in `pipeline_resources.rs`, reflected shader
  descriptor-kind mapping shared by draw/pipeline code in `shader_descriptors.rs`,
  texture/image resources in `texture_resources.rs`, video hardware-buffer import
  and texture retirement in `video_hardware_buffer.rs` / `texture_lifetime.rs`, and
  probe-specific shader/dispatch/readback code in child modules under
  `platform/src/os/linux/vulkan/` such as `basic_compute_probe.rs` and
  `skinning_mesh_probe.rs`. Future mesh-to-SDF, dense-SDF, ADF, or particle
  probes should be sibling modules unless they are small helpers shared by
  multiple probes.
- Workspace metadata excludes for standalone CSG leaf crates.
- Public-safe fork and agent notes.

Keep legacy Rusty-XR app behavior in the legacy Rusty-XR repo. Keep camera transport,
projection policy, scorecard markers, runtime profile keys, and public example
code out of this Makepad fork unless the change is a general Makepad adapter or
an upstreamable Makepad fix.

When extracting generic tracked-space output from Makepad/OpenXR events, target
Rusty Lattice naming outside this fork: `Lattice*` contracts and
`rusty.lattice.*` schema IDs for spaces, transforms, tracked poses, view sets,
spatial input roles, frame-state binding, calibration, validity, confidence,
and runtime capability snapshots. Keep Makepad `Xr*` names where they describe
Makepad or OpenXR APIs.

## Sustainable Design Guardrails

- Treat monolithic file pressure as an ownership problem, not a line-count
  problem. Split only by durable authority, schema, route, validation, adapter,
  or test-family boundaries; preserve facades, schema IDs, serde fields,
  fixture outputs, CLI behavior, validation outcomes, and dependency boundaries.
- After a split, update the nearest distributed file map: this `AGENTS.md`,
  `README.md`, `docs/ARCHITECTURE.md`, fixture docs, validation docs, or the
  planning `agent-state\iteration-events.jsonl`.
- Keep `AGENTS.md`, README, and skill files as concise routing indexes. Move
  lane-specific recipes, device/build detail, compatibility ledgers, and long
  validation flows into named docs or runbooks.
- Keep legacy Rusty-XR names as explicit compatibility surfaces only. New
  schemas, routes, and types use the owning lane (`rusty.manifold.*`,
  `rusty.lattice.*`, `rusty.matter.*`, `rusty.optics.*`, `rusty.quest.*`, or
  repo-local names); do not introduce `rusty.morphospace.*` schemas or
  `Morphospace*` core types by default.
## Downstream Dependency Boundary

The maintained fork is allowed as a dependency only in downstream app-shell or
UI lanes:

- Hostess Makepad shell crates;
- Studio Makepad/UI shell crates;
- legacy/public Rusty XR Makepad examples.

Keep Manifold, Manifold packages, Rusty core/CLI crates, descriptor repos, and
schema/fixture workspaces Makepad-free. Do not use this fork to define
Manifold command/session/stream authority.

## Morphospace Validation Ladder

Use focused validation for this fork branch:

```powershell
python tools\makepad_fork_format.py --changed --check
python tools\check_morphospace_makepad_guards.py
python tools\check_android_generated_output_stability.py
cargo metadata --manifest-path libs\csg\csg_math\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_mesh\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_sdf\Cargo.toml --no-deps --format-version 1
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
cargo build -p cargo-makepad --release
```

Do not use `cargo fmt --all` in this fork. Cargo's `--all` formatter route
also walks local path dependencies, which includes vendored crates with pruned
tests, benches, and examples. Use `python tools\makepad_fork_format.py --changed`
or `--changed --check` for edited files; the script derives workspace-member
roots from Cargo metadata instead of hard-coding a file list. If a repo-wide
audit is needed, use `python tools\makepad_fork_format.py --workspace --check`;
that excludes local path dependencies but may still report existing first-party
Makepad formatting drift.

If Android Java bridge code or `cargo-makepad` generated-template code changes,
run a touched-class Java compile against the Android target platform jar, then
reinstall `cargo-makepad` from this checkout before rebuilding downstream APKs.
The downstream Rust dependency lockfile does not update the installed packager.
For the Hostess Matter/SDF/particle Quest APK specifically, the known-good
debug build route from `S:\Work\repos\active\rusty-hostess\apps\hostess-t-makepad`
is:

```powershell
& 'S:\Work\tools\Quest\Use-QuestTooling.ps1'
cargo install --path S:\Work\repos\active\makepad-morphospace\tools\cargo_makepad --force
cargo makepad android --variant=quest --abi=aarch64 --sdk-path="$env:ANDROID_HOME" --package-name=io.github.mesmerprism.rustyhostess.makepad --app-label="Rusty Hostess Makepad" --quest-camera-permissions=false build -p hostess-t-makepad
```

Do not replace this with an app-local AndroidManifest template just to remove
camera permissions. Use the generated Quest manifest plus the explicit
camera-permission opt-out so OpenXR broker queries and `.MakepadAppXr` stay
intact.

Broker H.264 stream semantics matter for validation: `max_packets=0` means
live/unbounded, not one packet. A run that only proves stream-header metadata
does not prove decoded source parity; require prepared decode state, CPU-YUV
texture readiness or another explicit texture handoff, nonzero texture-update
cadence, and zero decode errors before comparing projection stages.

For Quest comparison work, keep the ladder ordered:

1. Minimal Makepad Quest/Vulkan surface smoke.
2. Morphospace Makepad synthetic OpenXR shell.
3. Morphospace synthetic stereo projection marker/scene.
4. Camera metadata and acquisition logging.
5. Hardware-buffer import.
6. Stereo projection parity against the non-Makepad Morphospace Quest APK lane.

## Agent Runbooks

This file is the L0 agent index for the Morphospace Makepad fork. Read only the focused runbook needed for the current task:

- `docs\agent-runbooks\studio-remote-runbook.md`: Studio remote protocol, runnable launch policy, JSONL requests, screenshots, widget queries, clicks, typing, and UI-run reliability notes.
- `docs\agent-runbooks\makepad-script-dsl-guide.md`: Makepad `script_mod!` syntax, widget/shader patterns, app structure, templates, PortalList/FileTree usage, and common script DSL pitfalls.

Do not paste long runbook updates back into `AGENTS.md`. Add detailed protocol or DSL notes to the focused runbook and keep this file as the navigation and fork-boundary surface.
