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
- Android video cleanup completion when a widget requests cleanup for a video id
  that has no retained platform player or surface resource, preventing the
  widget from remaining in `CleaningUp` during source switches.
- Workspace metadata exclusions for standalone CSG leaf crates that are outside
  the main Makepad workspace validation path.
- A local generated-target ignore rule for Android control builds.

Keep future changes reviewable as independent Makepad fixes. Portability,
packaging, workspace metadata, and renderer-correctness fixes should be shaped
so they can become upstream PRs when possible.

## Validation

For this branch, use focused validation instead of claiming full Makepad
repo-wide formatting hygiene:

```powershell
rustfmt --check platform\src\os\linux\vulkan.rs platform\src\os\linux\android\android.rs tools\cargo_makepad\src\android\compile.rs
cargo metadata --manifest-path libs\csg\csg_math\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_mesh\Cargo.toml --no-deps --format-version 1
cargo metadata --manifest-path libs\csg\csg_sdf\Cargo.toml --no-deps --format-version 1
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
cargo build -p cargo-makepad --release
```

Quest smoke validation should start with a minimal Makepad Android/Vulkan
surface before Rusty XR camera, broker, stream, or renderer measurements are
interpreted.
