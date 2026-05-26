# Rusty XR Fork Notes

This branch is a narrow Makepad patch queue used by the Rusty XR
Makepad-first Quest lane. Upstream Makepad remains the source of truth for the
framework; this branch carries only the small deltas needed to validate the
Quest Android/Vulkan build path against Rusty XR's public contracts.

## Relationship To Rusty XR

Rusty XR core crates stay framework-neutral and do not depend on Makepad. The
Makepad-first Rusty XR example depends on Makepad as an app shell and renderer
route, while sharing Rusty XR runtime-profile and diagnostic contracts with the
non-Makepad Quest examples.

The intended dependency direction is:

```text
Rusty XR core crates
  -> Rusty XR Quest examples
  -> Rusty XR Makepad-first Quest example
       -> this Makepad fork branch
```

Do not move Rusty XR app behavior, downstream package identity, generated APKs,
device logs, local SDK caches, or private validation artifacts into this branch.

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
  use API 26 as the native/minimum SDK floor while keeping the current target
  SDK at 33 for this branch. API-newer Java and native surfaces are guarded or
  kept out of strong native linkage before the floor is lowered.
- Android packaging fixes for the tested Windows-to-Quest build lane.
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
- Quest manifest launch semantics for generated XR activities, including
  VR-only/focus-aware metadata and non-resizeable activity declarations needed
  to distinguish immersive presentation from Horizon OS volumetric-window
  launch handling.
- A Quest OpenXR environment-depth fallback that keeps passthrough and
  projection startup alive when the runtime declines depth provider setup.
- A `Video` widget camera-permission option so camera playback can explicitly
  request headset-camera access on platforms where raw headset cameras are
  gated separately from the ordinary camera permission.
- An Android-only broker H.264 video source that uses the platform WebSocket
  command path, framed TCP H.264 packets, and Android MediaCodec so public
  examples can consume broker-managed synthetic or camera streams. On GL paths
  it can use Makepad's existing external-video texture handoff; on Quest
  Vulkan/XR paths it can fall back to decoded CPU-YUV plane upload because no
  GL external texture handle is available. The source descriptor can carry a
  requested camera ID and source frame rate for broker-camera runs.
- A video-source metadata event that forwards broker stream-header projection
  metadata to app code before projection-stage rows are derived.
- Optional `VideoTextureUpdated` metadata for camera texture lanes. Android
  camera paths can carry camera frame sequence/timestamp, acquire/upload/import
  timing, texture resource path, descriptor shape, Vulkan format facts, and
  fallback state without forcing app adapters to reconstruct those facts from
  marker text.
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
- An XR environment camera guard so `XrEnv` does not acquire a passthrough
  camera stream unless the environment cube is enabled. This keeps custom
  raw-camera projection examples from competing with Makepad's environment
  capture path when `env.env_cube` is false.
- Workspace metadata exclusions for standalone CSG leaf crates that are outside
  the main Makepad workspace validation path.
- A local generated-target ignore rule for Android control builds.

Installer defaults are separate from packaging defaults. The current
Makepad-managed Android-33/ext4 payload constants should stay internally
coherent until intentionally upgraded as a set; do not partially bump SDK URLs,
platform names, build-tools versions, directory names, or NDK version.

Keep future changes reviewable as independent Makepad fixes. Portability,
packaging, workspace metadata, and renderer-correctness fixes should be shaped
so they can become upstream PRs when possible.

## Validation

For this branch, use focused validation instead of claiming full Makepad
repo-wide formatting hygiene:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --manifest-path libs\csg\csg_math\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_mesh\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_sdf\Cargo.toml --no-deps --format-version 1
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
cargo build -p cargo-makepad --release
```

Use `tools\rusty_xr_format.py` instead of `cargo fmt --all`. Cargo's `--all`
formatter route includes local path dependencies, so it reaches vendored crates
that are not part of this fork's patch surface. The helper derives the
workspace-member roots from Cargo metadata and formats only changed first-party
Rust files by default. Its `--workspace --check` mode is available for audits,
but existing Makepad-wide rustfmt drift can still make that audit fail.

When Android Java bridge code changes, also compile the touched Java classes
against the Android platform jar used by the target SDK. When generated Android
templates or `cargo-makepad` packaging code changes, reinstall
`cargo-makepad` from this checkout before rebuilding a Rusty XR APK; a
downstream `Cargo.lock` pin alone does not update the packager binary.

Quest smoke validation should start with a minimal Makepad Android/Vulkan
surface before Rusty XR camera, broker, stream, or renderer measurements are
interpreted.

For broker H.264 validation, preserve `max_packets=0` as the live/unbounded
stream request. Clamping it to one packet can still produce stream-header
metadata but leaves MediaCodec without enough frames to prove decoded input
parity. Treat CPU-YUV decoded cadence and zero-copy surface-texture transport
as separate performance conclusions.

The GL `SurfaceTexture` path remains useful when the renderer is actually
OpenGL ES: Android MediaCodec and camera preview APIs naturally output to a
`SurfaceTexture` backed by `GL_TEXTURE_EXTERNAL_OES`. That does not by itself
solve the Quest Vulkan/XR path, where a GL texture handle is not a Vulkan image.
If Rusty XR explores a video-only OpenGL ES receiver, treat that as a separate
OpenXR+GL app architecture rather than as evidence that the current
Makepad/Vulkan CPU-YUV bridge is a final performance path.
