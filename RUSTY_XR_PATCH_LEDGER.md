# Rusty XR Makepad Fork Patch Ledger

This branch is a maintained Makepad fork for the Rusty XR Makepad-first Quest
lane. Upstream Makepad remains the framework source of truth. This ledger keeps
the local patch queue reviewable so the fork stays an app-shell, renderer, and
tooling dependency rather than becoming Rusty runtime authority.

Use this file when adding, rebasing, splitting, or upstreaming fork patches.
Keep entries public-safe: do not add private local paths, device logs, generated
APKs, downstream package identities, SDK caches, or private tuning constants.

## Current Baseline

Baseline branch comparison:

```powershell
git diff --stat upstream/dev...HEAD
git diff --name-status upstream/dev...HEAD
```

Current audit baseline:

- Branch: `dev`
- Head: `a96a63e06 Add controller tracking haptics`
- Upstream comparison: `upstream/dev...HEAD`
- Delta at audit time: `98 files changed, 13791 insertions(+), 1883 deletions(-)`

## Patch Families

| Family | Primary files | Classification | Keep local or upstream? | Validation |
| --- | --- | --- | --- | --- |
| Fork instructions and validation helpers | `AGENTS.md`, `RUSTY_XR_FORK_NOTES.md`, `RUSTY_XR_PATCH_LEDGER.md`, `Justfile`, `tools/check_all.ps1`, `tools/rusty_xr_format.py` | Branch-local documentation and validation routing. | Keep local unless a helper becomes generic Makepad tooling. | `python tools\rusty_xr_format.py --changed --check`; `make check` where appropriate. |
| Workspace metadata and generated-target ignores | `.gitignore`, `Cargo.toml` | Workspace hygiene for standalone leaf crates and local generated Android targets. | Upstream candidate only when generic and not Rusty-specific. | `cargo metadata --no-deps --format-version 1`; focused CSG metadata checks when CSG entries change. |
| Android packaging and cargo-makepad tooling | `tools/cargo_makepad/src/android/compile.rs`, `tools/cargo_makepad/src/android/compile/keystore.rs`, `tools/cargo_makepad/src/android/compile/wrapper_manifest.rs`, `tools/cargo_makepad/src/android/compile/toolchain.rs`, `tools/cargo_makepad/src/android/compile/shared_libs.rs`, `tools/cargo_makepad/src/android/mod.rs`, `tools/cargo_makepad/src/android/sdk.rs`, `tools/cargo_makepad/src/utils.rs` | Packaging/tooling patch family. | Prefer upstreamable slices for portability, stable generated-wrapper identity, SDK/JDK/NDK resolution, bundletool/AAB support, and shared-library bundling. | `cargo check -p cargo-makepad`; `cargo build -p cargo-makepad --release` after behavioral packaging changes. |
| Android Java shell and permissions | `MakepadActivity.java`, `MakepadInputConnection.java`, `MakepadNative.java`, `MediaProjectionStreamService.java`, `VideoPlayer.java` | Local Quest/Android app-shell adapter plus generic Android shell fixes. | Keep Quest/Rusty launch markers local; upstream generic activity, input, permission, or generated-shell fixes when separable. | Java touched-class compile when Java changes; downstream APK source-root build for generated-shell behavior. |
| Manifold external H.264 video adapter | `BrokerH264VideoPlayer.java`, `ExternalH264Config.java`, `ManifoldH264CommandClient.java`, `ManifoldVideoStreamReader.java`, `H264AnnexBPrimer.java`, `ExternalH264HardwareBufferTarget.java`, `ExternalH264CpuYuvEmitter.java`, `platform/src/event/video_playback.rs`, `platform/src/os/linux/android/android_jni.rs`, `platform/src/os/linux/android/android.rs`, `widgets/src/video.rs` | Local adapter and compatibility surface. Manifold defaults are active; old Rusty-XR broker names are explicit legacy aliases only. | Keep local until a generic external H.264 source abstraction is separable. Avoid moving command/session/stream authority into Makepad. | Manifold-default scans; Java touched-class compile when Java changes; `cargo check -p cargo-makepad` when generated Java packaging changes. |
| Android camera and video metadata | `platform/src/os/linux/android/android_camera.rs`, `platform/src/os/linux/android/android_camera_player.rs`, `platform/src/video.rs`, platform video playback stubs | Quest camera/video metadata and texture-readiness evidence. | Keep local unless the API is generic Makepad camera/video metadata. | Makepad format check; downstream camera-shell build and device validation only when behavior changes. |
| Vulkan hardware-buffer import and lifetime | `platform/src/os/linux/vulkan.rs`, `platform/src/os/linux/vulkan_naga.rs`, shader reflection/lowering files | Renderer correctness and external video import evidence. | Upstream only as small generic lifetime/resource/sampler/reflection fixes. Do not broad-split upstream-owned files without a rebase plan. | `cargo check -p makepad-platform` when touching platform Vulkan; downstream GPU/page-fault gate for behavior changes. |
| OpenXR and Quest runtime surfaces | `platform/src/os/linux/openxr.rs`, `openxr_input.rs`, `openxr_opengl.rs`, `openxr_sys.rs`, `openxr_vulkan.rs`, `platform/src/event/xr.rs`, `xr/src/**` | Quest/OpenXR proving support and generic XR capability exposure. | Keep local for Rusty proving markers; upstream generic OpenXR fixes or hand-mesh capability exposure when separable. | Makepad format check; OpenXR/Quest smoke only when runtime behavior changes. |
| Shader, draw, and text intake | `draw/src/shader/**`, `draw/src/text/**`, `platform/script/src/**` | Mixed upstream intake plus XR shader builtin/resource-shape support. | Upstream generic shader/resource fixes; keep XR view-id or Rusty evidence markers local until generalized. | Cargo metadata/checks for affected crates; downstream render validation for behavior changes. |
| Widgets and Studio/UI intake | `widgets/src/**`, `examples/uizoo/**`, `tools/cargo_makepad/src/studio.rs`, `tools/cargo_makepad/src/tunnel.rs` | Upstream UI intake and local proving-surface polish. | Prefer upstream for generic widget fixes. Keep Rusty proving behavior out of generic widgets unless the API is renderer-neutral. | Makepad format check; targeted widget/app checks only when behavior changes. |
| Cross-platform stubs and upstream sync | Apple, Windows, Web, X11, Wayland, Linux video playback files | Usually upstream sync or generic compatibility changes needed to keep enum/API additions compiling across platforms. | Upstream candidate when not Rusty-specific. Keep changes minimal. | `cargo metadata --no-deps --format-version 1`; targeted cargo check if API shape changes. |
| Vendored or generated local touches | `libs/rapier/vendor/**`, generated binding-style files | Avoid churn. Only touch for required upstream sync or compiler compatibility. | Prefer no local edits. Re-check provenance before changing. | Only focused metadata/checks; do not use repo-wide formatting to rewrite vendored paths. |

## Split-Pressure Watchlist

Do not split upstream-owned Makepad files only because they are large. Split
only when the changed area is fork-owned, cohesive by responsibility, and
unlikely to create avoidable upstream merge conflicts.

Current first split candidates:

1. `tools/cargo_makepad/src/android/java/dev/makepad/android/BrokerH264VideoPlayer.java`
   - Keep the public class name initially as a compatibility facade.
   - Use `RUSTY_XR_H264_ADAPTER_SPLIT_PLAN.md` before moving Java code.
   - Config defaults and normalization now live in `ExternalH264Config.java`.
   - Manifold command WebSocket and command JSON now live in
     `ManifoldH264CommandClient.java`.
   - Stream header/framing and packet DTOs now live in
     `ManifoldVideoStreamReader.java`.
   - H.264 Annex-B primer parsing now lives in `H264AnnexBPrimer.java`.
   - Hardware-buffer target, retained frame DTO, and stereo pairing now live in
     `ExternalH264HardwareBufferTarget.java`.
   - CPU-YUV plane copy and callback emission now live in
     `ExternalH264CpuYuvEmitter.java`.
   - Remaining split candidate is the decoder loop; any stereo pairer
     lifecycle cleanup is a behavior slice, not a mechanical movement.
2. `tools/cargo_makepad/src/android/compile.rs`
   - Use `RUSTY_XR_ANDROID_COMPILE_SPLIT_PLAN.md` before moving Rust code.
   - Keystore sidecar parsing and upload-keystore creation now live in
     `tools/cargo_makepad/src/android/compile/keystore.rs`.
   - Generated wrapper manifest path rewriting, workspace patch extraction,
     wrapper arg stripping, changed-file writes, and source lockfile cache now
     live in `tools/cargo_makepad/src/android/compile/wrapper_manifest.rs`.
   - SDK/JDK/NDK path resolution, selected platform/build-tools values, Java
     tool lookup, clang wrapper API selection, NDK prebuilt selection, and SDK
     preflight reporting now live in
     `tools/cargo_makepad/src/android/compile/toolchain.rs`.
   - APK/AAB native shared-library dependency scanning and staging now live in
     `tools/cargo_makepad/src/android/compile/shared_libs.rs`.
   - Move package identity, manifest/template reconciliation, bundletool/AAB,
     resource/font staging, Java/R/Dex helpers, and timing/provenance helpers
     into focused Rust modules.

Hold on broad file-layout changes in `platform/src/os/linux/vulkan.rs`,
`platform/src/os/linux/android/android.rs`, or OpenXR files unless the slice is
small, behavior-driven, and has a clear upstream or fork-owned boundary.

## Naming Rules

- New command/session/stream defaults must use `rusty.manifold.*`,
  `/manifold/v1/...`, and Manifold-owned framing where applicable.
- Old `rusty.xr.broker.*`, `/rustyxr/v1/...`, `debug.rustyxr.*`, and
  `RXYRVID1` names are compatibility aliases only. Keep them explicitly named
  as legacy/compatibility surfaces.
- Diagnostic markers that still use `rusty.xr.makepad-*` are historical
  evidence markers. New markers should prefer a Makepad, Quest, Hostess, or
  Manifold owner name based on the actual authority.

## Update Policy

Before adding behavior to this fork:

1. Classify the changed files in the patch family table.
2. Decide whether the slice is upstreamable, local Quest/Makepad adapter work,
   compatibility alias work, or generated/vendor sync.
3. Run the smallest validation slot that covers the changed family.
4. Update this ledger when a new family appears, a split lands, or a default
   naming rule changes.
