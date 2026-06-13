# Morphospace Makepad Fork Notes

This branch is a narrow Makepad patch queue used by the Morphospace Makepad
Quest lane. Upstream Makepad remains the source of truth for the framework;
this branch carries only the small deltas needed to validate the Quest
Android/Vulkan build path against Morphospace app-shell contracts.

`MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md` classifies the current local patch families,
upstream-candidate boundaries, split-pressure watchlist, naming rules, and
validation slots. `MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md` classifies active
Manifold defaults, explicit legacy aliases, and rename-on-touch diagnostic
markers. Update those files when adding a new patch family, landing a split,
or changing a command/stream default.

## Relationship To Morphospace

Morphospace core crates stay framework-neutral and do not depend on Makepad.
The Morphospace Makepad example depends on Makepad as an app shell and renderer
route, while sharing Morphospace runtime-profile and diagnostic contracts with
the non-Makepad Quest examples.

The intended dependency direction is:

```text
Morphospace core crates
  -> Morphospace Quest examples
  -> Morphospace Makepad Quest example
       -> this Makepad fork branch
```

Do not move downstream app behavior, downstream package identity, generated APKs,
device logs, local SDK caches, or private validation artifacts into this branch.

## Dependency Boundary

Makepad dependencies are allowed only in downstream app-shell/UI lanes:

- Hostess Makepad shell crates;
- Studio Makepad/UI shell crates;
- legacy/public Rusty XR Makepad examples.

Keep Manifold, Manifold packages, Rusty core/CLI crates, descriptor repos, and
schema/fixture workspaces Makepad-free. This fork can prove rendering,
packaging, Android, OpenXR, Vulkan, and generated-shell behavior, but it must
not become command/session/stream authority or a core Rusty dependency.

## Current Patch Scope

This branch currently carries:

- Host/profile-aware Android packaging resolution. The packager preflights the
  selected SDK path and resolves installed platform, build-tools, Java tools,
  host NDK prebuilt, clang API level, and host executable names before the
  Android package build proceeds.
- Android manifest/package input reconciliation from upstream Makepad:
  generated manifests now separate min SDK from target SDK and can derive
  package id, label, version code, version name, and min SDK from generic Cargo
  metadata or explicit Android command-line flags.
- Android API-floor cleanup from upstream Makepad: default generated builds now
  use API 26 as the native/minimum SDK floor. API-newer Java and native
  surfaces are guarded or kept out of strong native linkage before the floor is
  lowered.
- Additional upstream `#1091` Android tooling alignment: Android Rust builds
  now route through the stable Rust toolchain, generated manifests default to
  target SDK 35, the Makepad-managed SDK can install bundletool, and app crates
  may provide `resources/android/AndroidManifest.xml.template` for explicit
  manifest customization.
- Additive upstream `#1091` Android App Bundle route: `cargo makepad android
  build-aab` stages resources/assets/native libraries into a bundle module,
  runs bundletool, and signs with jarsigner when a keystore is supplied. APK
  builds keep the existing side-load route and dynamic-std behavior.
- Android packaging fixes for the tested Windows-to-Quest build lane.
- Android generated-wrapper cache stability for no-change rebuilds: the
  packager keeps the generated wrapper directory, wrapper manifest, and
  generated patched lockfile stable when their inputs have not changed, and can
  emit opt-in `MAKEPAD_ANDROID_TIMING` phase markers for APK packaging steps.
- Dependent Rust shared-library bundling for Android APK output.
- Windows path normalization for generated Android wrapper inputs.
- A targeted Android Vulkan frame-fence wait before recreating
  swapchain-backed window resources after suboptimal or out-of-date present
  paths on Quest/Horizon OS.
- Public-safe Android activity/bootstrap phase markers for the Rusty XR Makepad
  Quest validation lane. These bracket Java activity entry, native library
  loading, native activity handoff, Java init/surface bootstrap, EGL setup,
  Vulkan backend startup, and handoff into Makepad's main loop.
- Quest manifest camera permission and optional camera feature declarations so
  public examples can validate Android NDK Camera2 metadata and acquisition
  through the generated Android shell.
- A Quest manifest camera-permission opt-out flag,
  `--quest-camera-permissions=false`, for generated XR APKs that still need
  `.MakepadAppXr`, OpenXR broker queries, and Quest VR metadata but must not
  declare Android, headset, or spatial camera permissions.
- Android video-input discovery gating so camera-free XR apps can opt out of
  passive `ACameraManager` enumeration. This is separate from manifest
  permission filtering: camera-free apps need both no camera permissions in the
  generated APK and no passive runtime camera discovery when streaming is
  disabled.
- Quest manifest launch semantics for generated XR activities, including
  VR-only/focus-aware metadata and non-resizeable activity declarations needed
  to distinguish immersive presentation from Horizon OS volumetric-window
  launch handling.
- A Quest OpenXR environment-depth fallback that keeps passthrough and
  projection startup alive when the runtime declines depth provider setup.
- A `Video` widget camera-permission option so camera playback can explicitly
  request headset-camera access on platforms where raw headset cameras are
  gated separately from the ordinary camera permission.
- An Android-only external H.264 video source that uses the platform WebSocket
  command path, Manifold command-envelope defaults, `/manifold/v1/events`,
  `RMANVID1` stream framing, explicit legacy `RXYRVID1` compatibility, and
  Android MediaCodec so public examples can consume Manifold/broker-managed
  synthetic or camera streams. On GL paths it can use Makepad's existing
  external-video texture handoff; on Quest Vulkan/XR paths it can fall back to
  decoded CPU-YUV plane upload because no GL external texture handle is
  available. The source descriptor can carry a requested camera ID and source
  frame rate for broker-camera runs.
- A video-source metadata event that forwards broker stream-header projection
  metadata to app code before projection-stage rows are derived.
- Optional `VideoTextureUpdated` metadata for camera texture lanes. Android
  camera paths can carry camera input/format identity, camera frame
  sequence/timestamp, acquire/upload/import timing, texture resource path,
  descriptor shape, Vulkan format facts, and fallback state without forcing app
  adapters to reconstruct those facts from marker text.
- A small shader builtin, `xr_view_id()`, that exposes Makepad's existing
  backend multiview index to application shader code for XR per-eye texture
  selection without requiring app shaders to reference backend-specific
  symbols.
- Android Vulkan external video import markers that report the
  `AHardwareBuffer`/YCbCr resource shape and the current separate
  sampled-image plus sampler descriptor binding shape before any combined
  image-sampler shader/resource fix is attempted.
- A reflected Vulkan shader-resource interface from Naga/WGSL lowering, used
  to construct descriptor layouts from the shader-declared resource shape and
  to log the current video texture/sampler interface before changing it.
- App-visible XR event-state fields for the active per-eye OpenXR local-space
  pose/FOV, so public examples can compute display-eye projection mappings from
  runtime view state instead of hard-coded display constants.
- App-visible OpenXR hand-mesh bind data for runtimes that expose
  `XR_FB_hand_tracking_mesh`. The fork keeps this generic: compact mesh
  status/counts in XR hand state, and full bind poses, joint parents, vertices,
  normals, UVs, blend indices/weights, and indices behind an on-demand Cx API.
- Generic XR/Vulkan basic probe plumbing now lives in
  `platform/src/os/linux/vulkan/basic_compute_probe.rs`: storage-buffer
  fill/copy/readback, bounded u32 compute, and bounded f32 force sampling
  proofs. Keep these probes generic Makepad adapter evidence; downstream
  crates own field, particle, force-mode, and readiness semantics.
- Generic XR/Vulkan f32 mesh-to-SDF proof plumbing that caches shader modules,
  descriptor-set layout, pipeline layout, compute pipelines, source mesh
  buffers, and capacity-rounded derived skinned-position/dense-SDF buffers for
  the renderer lifetime when no unfinished proof still needs the previous
  output. It keeps descriptor pools, command buffers, fences, params, grids,
  and compact readback resources scoped to each bounded proof submission. It
  reports program, source-buffer, and derived-buffer generation/reuse without
  defining Matter field, particle, or SDF authority in Makepad. Resident
  capacity bytes are kept as resource evidence; field sampling and field-force
  sampling use the logical grid dimensions for voxel-count validation.
  defining Matter field, particle, or SDF authority in Makepad.
- An XR environment camera guard so `XrEnv` does not acquire a passthrough
  camera stream unless the environment cube is enabled. This keeps custom
  raw-camera projection examples from competing with Makepad's environment
  capture path when `env.env_cube` is false.
- Workspace metadata exclusions for standalone CSG leaf crates that are outside
  the main Makepad workspace validation path.
- A local generated-target ignore rule for Android control builds.
- A local Rusty XR Makepad guard script,
  `tools/check_morphospace_makepad_guards.py`, that verifies Manifold H.264
  defaults, explicit legacy aliases, stale-doc pointers, split helper files,
  and Android package-generation stability hooks.
- A local generated-output stability checker,
  `tools/check_android_generated_output_stability.py`, that can snapshot and
  compare generated wrapper manifests, lock/hash caches, generated manifests,
  app Java sources, javac input caches, and selected SDK/JDK/NDK/Cargo path
  environment across no-op package-generation runs.

Installer defaults are separate from packaging defaults. The current
Makepad-managed Android-33/ext4 payload constants should stay internally
coherent until intentionally upgraded as a set; do not partially bump SDK URLs,
platform names, build-tools versions, directory names, or NDK version.

Keep future changes reviewable as independent Makepad fixes. Portability,
packaging, workspace metadata, and renderer-correctness fixes should be shaped
so they can become upstream PRs when possible.

## CPU-YUV Upload Accounting

The Android CPU-YUV Quest lane currently moves one full I420 camera frame per
camera input stream before Vulkan upload. The camera callback publishes I420
frames into a latest-wins `CameraFrameRing`; `AndroidCameraPlayer::poll_frame`
then consumes only a pending or newly observed frame, swaps Y/U/V plane buffers
into three `VecRu8` textures, and emits one `cpu-yuv-upload` metadata marker
for that camera frame.

On the Vulkan side, dirty vector textures are uploaded once per repaint after
draw-list traversal deduplicates texture IDs. For the stereo camera evidence
lane, the measured repaint payload is therefore expected to be two camera input
streams times three I420 plane textures. At 1280x1280 this is about 2.34 MiB per
camera upload marker and about 4.69 MiB per repaint when both streams update in
the same repaint window.

Treat this as the baseline headroom cost for the CPU-YUV reference path. Future
optimizations should target reducing CPU copy/staging/upload work, avoiding
unnecessary full-plane uploads, or making the hardware-buffer path color-correct;
do not assume the current six texture uploads per repaint are a duplicate-upload
bug without new evidence from per-plane/frame markers.

## Validation

For this branch, use focused validation instead of claiming full Makepad
repo-wide formatting hygiene:

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

Use `tools\makepad_fork_format.py` instead of `cargo fmt --all`. Cargo's `--all`
formatter route includes local path dependencies, so it reaches vendored crates
that are not part of this fork's patch surface. The helper derives the
workspace-member roots from Cargo metadata and formats only changed first-party
Rust files by default. Its `--workspace --check` mode is available for audits,
but existing Makepad-wide rustfmt drift can still make that audit fail.

When Android Java bridge code changes, also compile the touched Java classes
against the Android platform jar used by the target SDK. When generated Android
templates or `cargo-makepad` packaging code changes, reinstall
`cargo-makepad` from this checkout before rebuilding a downstream APK; a
downstream `Cargo.lock` pin alone does not update the packager binary.

Quest smoke validation should start with a minimal Makepad Android/Vulkan
surface before downstream camera, broker, stream, or renderer measurements are
interpreted.

For broker H.264 validation, preserve `max_packets=0` as the live/unbounded
stream request. Clamping it to one packet can still produce stream-header
metadata but leaves MediaCodec without enough frames to prove decoded input
parity. Treat CPU-YUV decoded cadence and zero-copy surface-texture transport
as separate performance conclusions.

Broker H.264 stress validation now treats live stereo cadence, per-eye texture
updates, paired-frame markers, and projection-mapping readiness as one gate.
Makepad owns the UI/rendering consumer path; the broker owns stream identity,
module state, and optional sidecar modules such as external Linux/Python
processors or dedicated biometric communication modules.

The current validated path assumes the broker stream remains live for the full
measurement window. If broker stream leases expire or the stream server
restarts, the Makepad consumer should reconnect or surface a hard stale state
instead of silently continuing with the last decoded texture.

The GL `SurfaceTexture` path remains useful when the renderer is actually
OpenGL ES: Android MediaCodec and camera preview APIs naturally output to a
`SurfaceTexture` backed by `GL_TEXTURE_EXTERNAL_OES`. That does not by itself
solve the Quest Vulkan/XR path, where a GL texture handle is not a Vulkan image.
If Rusty XR explores a video-only OpenGL ES receiver, treat that as a separate
OpenXR+GL app architecture rather than as evidence that the current
Makepad/Vulkan CPU-YUV bridge is a final performance path.
