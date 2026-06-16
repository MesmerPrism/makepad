# Morphospace Makepad Fork Patch Ledger

This branch is a maintained Makepad fork for the Morphospace Makepad Quest
lane. Upstream Makepad remains the framework source of truth. This ledger keeps
the local patch queue reviewable so the fork stays an app-shell, renderer, and
tooling dependency rather than becoming Morphospace runtime authority.

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
| Fork instructions and validation helpers | `AGENTS.md`, `MORPHOSPACE_MAKEPAD_FORK_NOTES.md`, `MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md`, `MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md`, `MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md`, `MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md`, `MORPHOSPACE_MAKEPAD_ACTIVITY_SPLIT_PLAN.md`, `Justfile`, `tools/check_all.ps1`, `tools/makepad_fork_format.py`, `tools/check_morphospace_makepad_guards.py`, `tools/check_android_generated_output_stability.py` | Branch-local documentation and validation routing. | Keep local unless a helper becomes generic Makepad tooling. | `python tools\makepad_fork_format.py --changed --check`; `python tools\check_morphospace_makepad_guards.py`; `python tools\check_android_generated_output_stability.py`; `make check` where appropriate. |
| Workspace metadata and generated-target ignores | `.gitignore`, `Cargo.toml` | Workspace hygiene for standalone leaf crates and local generated Android targets. | Upstream candidate only when generic and not Rusty-specific. | `cargo metadata --no-deps --format-version 1`; focused CSG metadata checks when CSG entries change. |
| Android packaging and cargo-makepad tooling | `tools/cargo_makepad/src/android/compile.rs`, `tools/cargo_makepad/src/android/compile/keystore.rs`, `tools/cargo_makepad/src/android/compile/wrapper_manifest.rs`, `tools/cargo_makepad/src/android/compile/toolchain.rs`, `tools/cargo_makepad/src/android/compile/shared_libs.rs`, `tools/cargo_makepad/src/android/compile/packaging_inputs.rs`, `tools/cargo_makepad/src/android/compile/assets.rs`, `tools/cargo_makepad/src/android/compile/java_build.rs`, `tools/cargo_makepad/src/android/compile/apk_assembly.rs`, `tools/cargo_makepad/src/android/compile/aab_assembly.rs`, `tools/cargo_makepad/src/android/compile/rust_build.rs`, `tools/cargo_makepad/src/android/mod.rs`, `tools/cargo_makepad/src/android/sdk.rs`, `tools/cargo_makepad/src/utils.rs` | Packaging/tooling patch family. Includes the generated Quest manifest camera-permission opt-out flag used by camera-free XR apps that still need `MakepadAppXr` and OpenXR metadata. | Prefer upstreamable slices for portability, stable generated-wrapper identity, SDK/JDK/NDK resolution, bundletool/AAB support, manifest feature flags, and shared-library bundling. | `cargo test -p cargo-makepad quest_manifest`; `cargo check -p cargo-makepad`; `cargo build -p cargo-makepad --release` after behavioral packaging changes. |
| Android Java shell and permissions | `MakepadActivity.java`, `MorphospaceActivitySupport.java`, `MorphospaceMediaProjectionHelper.java`, `ExternalH264VideoPlaybackFactory.java`, `MakepadInputConnection.java`, `MakepadNative.java`, `MediaProjectionStreamService.java`, `VideoPlayer.java` | Local Quest/Android app-shell adapter plus generic Android shell fixes. `MakepadActivity.java` remains the generated activity facade; Morphospace phase-marker/intent parsing, MediaProjection glue, and external-H264 entrypoint construction live in package-private helpers. | Keep Quest/Morphospace launch markers local; upstream generic activity, input, permission, or generated-shell fixes when separable. | Java touched-package compile when Java changes; downstream APK source-root build for generated-shell behavior. |
| Manifold external H.264 video adapter | `BrokerH264VideoPlayer.java`, `ExternalH264VideoPlaybackFactory.java`, `ExternalH264Config.java`, `ManifoldH264CommandClient.java`, `ManifoldVideoStreamReader.java`, `H264AnnexBPrimer.java`, `ExternalH264HardwareBufferTarget.java`, `ExternalH264CpuYuvEmitter.java`, `platform/src/event/video_playback.rs`, `platform/src/os/linux/android/android_jni.rs`, `platform/src/os/linux/android/android.rs`, `platform/src/os/linux/android/android_java_messages.rs`, `widgets/src/video.rs` | Local adapter and compatibility surface. Manifold defaults are active; old Rusty-XR broker names are explicit legacy aliases only. | Keep local until a generic external H.264 source abstraction is separable. Avoid moving command/session/stream authority into Makepad. | Manifold-default scans; Java touched-class compile when Java changes; `cargo check -p cargo-makepad` when generated Java packaging changes. |
| Android camera and video metadata | `platform/src/os/linux/android/android_media.rs`, `platform/src/os/linux/android/android_camera.rs`, `platform/src/os/linux/android/android_camera_player.rs`, `platform/src/media_api.rs`, `platform/src/video.rs`, platform video playback stubs | Quest camera/video metadata, texture-readiness evidence, and opt-in passive video-input discovery gating for camera-free XR apps. | Keep local unless the API is generic Makepad camera/video metadata. | Makepad format check; downstream camera-shell build and device validation only when behavior changes. |
| Android Java-message dispatch | `platform/src/os/linux/android/android.rs`, `platform/src/os/linux/android/android_java_messages.rs`, `platform/src/os/linux/openxr.rs` | Android Java/JNI message coalescing and dispatch. The child module owns high-rate touch/stereo hardware-buffer coalescing plus grouped surface/window, input/IME, network/MIDI/permission, video/camera, and lifecycle handlers. | Keep local unless a generic upstream event-dispatch split is intentionally proposed. Do not add Manifold, Matter, Quest-Makepad, or Hostess authority here. | `python tools\makepad_fork_format.py --changed --check`; `python tools\check_morphospace_makepad_guards.py`; `cargo check -p makepad-platform`. |
| Vulkan texture resources, OpenXR targets, frame resources, buffer resources, draw recording, pipeline resources, hardware-buffer import, and lifetime | `platform/src/os/linux/vulkan.rs`, `platform/src/os/linux/vulkan/buffer_resources.rs`, `platform/src/os/linux/vulkan/draw_recording.rs`, `platform/src/os/linux/vulkan/frame_resources.rs`, `platform/src/os/linux/vulkan/openxr_targets.rs`, `platform/src/os/linux/vulkan/pipeline_resources.rs`, `platform/src/os/linux/vulkan/texture_resources.rs`, `platform/src/os/linux/vulkan/texture_lifetime.rs`, `platform/src/os/linux/vulkan/video_hardware_buffer.rs`, `platform/src/os/linux/vulkan_naga.rs`, shader reflection/lowering files | Renderer correctness and external video import evidence. Host-visible buffer allocation, memory-type lookup, upload helpers, alignment, and geometry-resource cache ownership live in `buffer_resources.rs`; descriptor-pool allocation/reset/destruction, packet-buffer slices, and texture-upload slices live in `frame_resources.rs`; draw-list traversal, draw-packet assembly, descriptor writes, packet-buffer upload, and indexed draw command recording live in `draw_recording.rs`; OpenXR multiview target/session/readback/draw ownership lives in `openxr_targets.rs`; graphics pipeline keys, immutable video sampler keying, shader descriptor layouts, shader modules, graphics pipeline creation, and Makepad sampler creation live in `pipeline_resources.rs`; vector texture upload, render/depth target resources, depth dummy resources, and image layout transitions live in `texture_resources.rs`. Texture-resource retirement, video hardware-buffer cache helpers, AHardwareBuffer import, external YCbCr sampler setup, and video texture update entry points live in Vulkan child modules; the root Vulkan file remains lifecycle/window-swapchain/render-pass/offscreen-target orchestration. | Upstream only as small generic texture/resource/lifetime/sampler/reflection fixes. Do not broad-split upstream-owned files without a rebase plan. | `python tools\makepad_fork_format.py --changed --check`; `python tools\check_morphospace_makepad_guards.py`; `cargo check -p makepad-platform --target aarch64-linux-android` when touching Android Vulkan; downstream GPU/page-fault gate for behavior changes. |
| XR Vulkan basic storage/compute probes | `platform/src/cx_api.rs`, `platform/src/os/linux/android/android.rs`, `platform/src/os/linux/vulkan.rs`, `platform/src/os/linux/vulkan/basic_compute_probe.rs` | Generic XR adapter probes for storage-buffer fill/copy/readback, bounded u32 compute, and bounded f32 force sampling. Shader code, descriptor setup, command buffers, readback, and retained probe resources live in `basic_compute_probe.rs`; the root Vulkan file stays lifecycle/resource glue. | Keep local unless a generic Makepad storage/compute probe API becomes upstreamable. This is not Matter/field/particle authority, force-mode authority, or a runtime readiness claim. | `python tools\makepad_fork_format.py --changed --check`; `python tools\check_morphospace_makepad_guards.py`; `cargo check -p makepad-platform`; downstream Quest evidence should keep compute-kernel/readiness claims explicit. |
| XR Vulkan f32 stimulus-volume compute/readback previews | `platform/src/cx_api.rs`, `platform/src/os/linux/android/android.rs`, `platform/src/os/linux/vulkan.rs`, `platform/src/os/linux/vulkan/volume_probe.rs`, `platform/src/os/linux/vulkan/volume_raymarch_preview.rs`, `platform/src/os/linux/vulkan/volume_image_preview.rs` | Generic XR adapter proofs that run bounded f32 volume point-sample, tiny stereo raymarch-preview, and scalable stereo image-atlas preview compute shaders, then return compact storage-buffer or image/readback metrics to downstream app-shell evidence. | Keep local unless a generic Makepad compute/readback preview API becomes upstreamable. This is not Optics profile authority, not a production volume renderer, and not downstream stimulus tuning authority; the image-atlas path may prove texture residency/adoption, but downstream app shells still own runtime texture-binding claims. | `python tools\makepad_fork_format.py --changed --check`; `cargo check -p makepad-platform`; downstream Quest-Makepad and Hostess checks for profile/oracle marker propagation, scalable atlas metadata, and explicit runtime/compute-readiness claims. |
| XR Vulkan f32 skinning, mesh-SDF, and field probes | `platform/src/cx_api.rs`, `platform/src/os/linux/android/android.rs`, `platform/src/os/linux/vulkan.rs`, `platform/src/os/linux/vulkan/skinning_probe.rs`, `platform/src/os/linux/vulkan/skinning_mesh_probe.rs`, `platform/src/os/linux/vulkan/mesh_sdf_probe.rs`, `platform/src/os/linux/vulkan/field_sample_probe.rs`, `platform/src/os/linux/vulkan/field_force_sample_probe.rs` | Generic XR adapter proof plumbing for f32 skinning, skinning-mesh, mesh-to-dense-SDF, resident-field distance sampling, and resident-field force sampling. Renderer-lifetime program reuse, source mesh buffer residency, and capacity-aware derived buffer reuse remain Makepad resource evidence only. | Keep local unless a generic Makepad compute-proof API becomes upstreamable. This is not Matter/SDF/particle authority; downstream crates own CPU oracle semantics, field/force marker contracts, and force-mode selection. | `python tools\makepad_fork_format.py --changed --check`; `cargo check -p makepad-platform`; downstream Quest-Makepad and Hostess checks for marker propagation. |
| OpenXR and Quest runtime surfaces | `platform/src/os/linux/openxr.rs`, `openxr_input.rs`, `openxr_opengl.rs`, `openxr_sys.rs`, `openxr_vulkan.rs`, `platform/src/event/xr.rs`, `xr/src/**` | Quest/OpenXR proving support and generic XR capability exposure. | Keep local for Morphospace proving markers; upstream generic OpenXR fixes or hand-mesh capability exposure when separable. | Makepad format check; OpenXR/Quest smoke only when runtime behavior changes. |
| Shader, draw, and text intake | `draw/src/shader/**`, `draw/src/text/**`, `platform/src/draw_shader.rs`, `platform/script/src/**` | Mixed upstream intake plus XR shader builtin/resource-shape support. Includes an adapted [makepad/makepad#922](https://github.com/makepad/makepad/pull/922) fix for single-slot UInt/SInt instance attribute packing and Metal `IoInstanceRaw` padding. | Upstream generic shader/resource fixes; keep XR view-id or Morphospace evidence markers local until generalized. | `cargo test -p makepad-platform attribute_packing`; `cargo test -p makepad-script metal_instance`; `cargo check -p makepad-platform`; guard signatures for `#922` source reality. |
| TextInput navigation and alignment | `widgets/src/text_input.rs`, `examples/uizoo/src/tab_textinput.rs`, `tools/check_morphospace_makepad_guards.py` | Adapted import of [makepad/makepad#1101](https://github.com/makepad/makepad/pull/1101) for standard keyboard navigation, word/line/page movement, modifier-aware delete ranges, and grapheme-boundary hardening. Preserves fork-local `TextInputTextPlacement.InnerAlign` as a generic widget feature. [makepad/makepad#1085](https://github.com/makepad/makepad/pull/1085) and [makepad/makepad#1109](https://github.com/makepad/makepad/pull/1109) remain deferred for a broader cross-platform IME/platform slice. | Prefer upstream for generic TextInput behavior. Keep `InnerAlign` local until it is upstreamed generically; do not solve questionnaire centering with one-off padding. | `cargo check -p makepad-widgets`; `cargo test -p makepad-widgets text_input`; `python tools\check_morphospace_makepad_guards.py`; `git diff --check`. |
| Widgets and Studio/UI intake | `widgets/src/**`, `examples/uizoo/**`, `tools/cargo_makepad/src/studio.rs`, `tools/cargo_makepad/src/tunnel.rs` | Upstream UI intake and local proving-surface polish. | Prefer upstream for generic widget fixes. Keep Morphospace proving behavior out of generic widgets unless the API is renderer-neutral. | Makepad format check; targeted widget/app checks only when behavior changes. |
| Cross-platform stubs and upstream sync | Apple, Windows, Web, X11, Wayland, Linux video playback files | Usually upstream sync or generic compatibility changes needed to keep enum/API additions compiling across platforms. | Upstream candidate when not Rusty-specific. Keep changes minimal. | `cargo metadata --no-deps --format-version 1`; targeted cargo check if API shape changes. |
| Vendored or generated local touches | `libs/rapier/vendor/**`, generated binding-style files | Avoid churn. Only touch for required upstream sync or compiler compatibility. | Prefer no local edits. Re-check provenance before changing. | Only focused metadata/checks; do not use repo-wide formatting to rewrite vendored paths. |

## Split-Pressure Watchlist

Do not split upstream-owned Makepad files only because they are large. Split
only when the changed area is fork-owned, cohesive by responsibility, and
unlikely to create avoidable upstream merge conflicts.

This is a watchlist, not an active split queue. `MakepadActivity.java`,
`BrokerH264VideoPlayer.java`, and `compile.rs` are now cohesive facades after
the completed pressure-release slices. Do not continue splitting them by line
count. Split again only when behavior work touches a concrete ownership family
and the helper boundary preserves public signatures, generated output,
validation behavior, and legacy compatibility.

1. `tools/cargo_makepad/src/android/java/dev/makepad/android/MakepadActivity.java`
   - Status: preflight complete; first Morphospace-owned app-shell slices are done.
   - Use `MORPHOSPACE_MAKEPAD_ACTIVITY_SPLIT_PLAN.md` before moving Java code.
   - Phase-marker and intent-extra parsing now live in
     `MorphospaceActivitySupport.java`.
   - MediaProjection request/result/service glue now lives in
     `MorphospaceMediaProjectionHelper.java`.
   - H264/external-video config/player/runnable construction now lives in
     `ExternalH264VideoPlaybackFactory.java`; the public method signature and
     video runnable map/thread ownership stay in `MakepadActivity.java`.
   - Current decision: do not continue splitting `MakepadActivity.java` by line
     count. It remains the generated activity facade for Android lifecycle,
     plugin hooks, native callbacks, video map/thread ownership, activity
     switching, and generated template hooks.
   - Future slices must be behavior-led: revisit only when a concrete
     MediaProjection, phase-marker, intent-extra, or external-H264 entrypoint
     change needs a helper and can preserve the public method signature.
   - Do not broad-refactor upstream Android lifecycle, surface recovery,
     keyboard/input, selection, network thread ownership, or activity switching
     by line count alone.
2. `tools/cargo_makepad/src/android/java/dev/makepad/android/BrokerH264VideoPlayer.java`
   - Status: H264 adapter split complete through config, command client,
     stream reader, Annex-B primer, HWB target, CPU-YUV emitter, and activity
     entrypoint factory.
   - Keep the public class name initially as a compatibility facade.
   - Use `MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md` before moving Java code.
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
   - Current decision: leave `BrokerH264VideoPlayer.java` as the decoder
     orchestrator. The decoder loop owns packet sequencing, CSD/primer handoff,
     MediaCodec lifecycle, output routing, timing counters, callbacks, and
     cleanup. Split only if H264 behavior churn resumes and the preflight shows
     a narrow `ExternalH264DecoderLoop.java`-style boundary.
   - Any stereo pairer lifecycle cleanup is a behavior slice, not mechanical
     movement.
3. `tools/cargo_makepad/src/android/compile.rs`
   - Status: Android packaging/tooling pressure release complete; helper
     families are split and `compile.rs` is a cohesive command facade.
   - Use `MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md` before moving Rust code.
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
   - Package identity, manifest/template preparation, generated app Java
     source, and output filename derivation now live in
     `tools/cargo_makepad/src/android/compile/packaging_inputs.rs`.
   - APK/AAB resource/font staging now lives in
     `tools/cargo_makepad/src/android/compile/assets.rs`.
   - R class creation, javac hashing/cache behavior, and Dex generation now
     live in `tools/cargo_makepad/src/android/compile/java_build.rs`.
   - APK assembly/signing now lives in
     `tools/cargo_makepad/src/android/compile/apk_assembly.rs`.
   - AAB assembly/signing now lives in
     `tools/cargo_makepad/src/android/compile/aab_assembly.rs`.
   - Rust build setup, Android target-dir derivation, Android env vars, and
     rustflags composition now live in
     `tools/cargo_makepad/src/android/compile/rust_build.rs`.
   - Current decision: do not continue splitting `compile.rs` by line count.
     It owns `build`/`build_aab`/`run` orchestration, phase timing,
     ADB/device helpers, and Java/Javac passthrough as facade responsibilities.
   - Future packaging behavior changes must use
     `tools/check_android_generated_output_stability.py` for generated-output
     identity checks when they claim no-op package stability.
4. Platform/video watchlist:
   - `platform/src/os/linux/android/android.rs`: Java-message coalescing and
     dispatch now lives in
     `platform/src/os/linux/android/android_java_messages.rs`. The root file
     remains Android platform loop, backend, surface, camera, and media glue.
     Future root splits should still be behavior-led: touch a concrete video,
     hardware-buffer, marker, camera, or backend lifecycle family with a clear
     helper boundary and downstream validation.
   - `platform/src/os/linux/android/android_jni.rs`: monitor adapter API
     growth, but do not preemptively split JNI message handling.
   - `widgets/src/video.rs`: monitor video widget API growth, but keep widget
     event handling intact until a renderer-neutral adapter boundary appears.
   - `platform/src/os/linux/vulkan.rs`: root basic storage/u32/f32 probe code is
     split to `platform/src/os/linux/vulkan/basic_compute_probe.rs`,
      per-frame buffer/descriptor-pool ownership is split to
     `platform/src/os/linux/vulkan/frame_resources.rs`, draw-list traversal and
     draw-packet command recording are split to
     `platform/src/os/linux/vulkan/draw_recording.rs`, OpenXR
     multiview target/session/readback/draw ownership is split to
     `platform/src/os/linux/vulkan/openxr_targets.rs`, host-buffer and
     geometry-resource-cache ownership is split to
     `platform/src/os/linux/vulkan/buffer_resources.rs`, graphics pipeline and
     shader-resource ownership is split to
     `platform/src/os/linux/vulkan/pipeline_resources.rs`, reflected shader
     descriptor-kind mapping shared by draw/pipeline code is split to
     `platform/src/os/linux/vulkan/shader_descriptors.rs`, and texture/image
     resource ownership is split to
     `platform/src/os/linux/vulkan/texture_resources.rs`. Leave the root file
     focused on instance/device lifecycle, window swapchain, render-pass, and
     offscreen target orchestration unless HWB/video-import work or a narrow
     proof-program/resource-lifetime slice forces a clear
     resource-lifetime, command-buffer, descriptor, or pipeline boundary with a
     GPU validation gate. Video import/lifetime work should start in
    `platform/src/os/linux/vulkan/video_hardware_buffer.rs`,
    `platform/src/os/linux/vulkan/texture_lifetime.rs`, or
    `platform/src/os/linux/vulkan/texture_resources.rs` before expanding the
    root file. Probe slices must not claim Matter compute authority or a
    field/particle kernel.
   - `platform/script/src/shader_calls.rs`: XR `xr_view_id` and video texture
     lowering are visible split candidates, but the current functions are
     upstream shader-call dispatch machinery. Move only if a future shader slice
     can isolate a texture/builtin helper without changing DSL behavior.
   - `platform/src/os/linux/openxr.rs`: display-refresh, environment-depth, and
     frame-flow code are candidates for behavior-led extraction, but broad
     session/frame splits would fight upstream OpenXR lifecycle ownership.

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
- Use `MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md` before renaming any `debug.rustyxr.*`,
  `rustyxr.*`, or `rusty.xr.makepad-*` surface. Most of these are
  rename-on-touch compatibility markers, not immediate cleanup targets.
- `tools/check_morphospace_makepad_guards.py` is the repo-local drift check for
  H.264 Manifold defaults, explicit `LEGACY_*` aliases, stale doc pointers,
  split helper files, and generated-output stability hooks.
- `tools/check_android_generated_output_stability.py` snapshots and compares
  no-op Android generated-output identity: wrapper manifests, wrapper
  lock/hash caches, generated Android manifests, app Java sources, javac input
  caches, and selected SDK/JDK/NDK/Cargo path environment.

## Dependency Boundary

Makepad dependencies are allowed only in downstream app-shell/UI lanes:

- Hostess Makepad shell crates;
- Studio Makepad/UI shell crates;
- legacy/public Rusty XR Makepad examples.

Keep Manifold, Manifold packages, Rusty core/CLI crates, descriptor repos, and
schema/fixture workspaces Makepad-free. Makepad may prove app-shell,
packaging, rendering, Android, OpenXR, Vulkan, and generated-shell behavior,
but it must not define Manifold command/session/stream authority.

## Recent Patch Notes

- 2026-06-16: imported/adapted upstream makepad/makepad#922 so attribute
  packing aligns every non-float instance attribute, including single-slot
  UInt/SInt values, to the same vec4 boundary used by GLSL. Metal
  `IoInstanceRaw` generation now inserts explicit float padding fields around
  non-float instance fields so raw struct stride matches the CPU instance
  buffer. Added narrow CPU-packing and Metal-padding unit tests plus guard
  signatures for the old `slots > 1` blind spot.
- 2026-06-16: hardened Android Quest/Vulkan WSI recovery and
  `AHardwareBuffer` texture lifetime by making window-fence and device-idle
  waits advance the GPU completed-submit watermark and collect ready retired
  texture resources. Present-time `ERROR_SURFACE_LOST_KHR` now waits the
  submitted window frame before suspending/destroying the surface, matching the
  existing out-of-date/suboptimal swapchain recovery path.
- 2026-06-16: switched the Vulkan hardware-buffer texture lifetime
  `AHardwareBuffer_getId` lookup to the shared Linux `ModuleLoader` path. The
  cached symbol table now owns the loaded `libandroid.so` module beside the
  function pointer, preserving the previous process-lifetime optional symbol
  behavior without file-local `dlopen`/`dlsym` code in the Vulkan child module.
- 2026-06-16: switched Android AMidi dynamic symbol loading to the shared
  Linux `ModuleLoader` path while preserving API-26-safe optional loading,
  cached function pointers, and graceful fallback when `libamidi.so` is absent.
  Repo audit found the next active Linux direct-loader candidate in Vulkan
  hardware-buffer texture lifetime (`AHardwareBuffer_getId` lookup); keep that
  as a separate GPU/HWB cleanup slice with Vulkan validation rather than mixing
  it into the AMidi refactor.
- 2026-06-16: completed a file-family review of upstream Makepad `#1091`
  against the pushed fork baseline. The Android packaging/lifecycle/IME/system
  bar/runtime-symbol families were already adapted into this fork's split
  modules. Imported the missing TOML parser hardening from `#1091` as a direct
  upstream behavior/test port, with local rustfmt normalization, so Cargo
  metadata parsing survives quoted table headers, inline tables, and multiline
  strings. Expanded guard signatures for the full reviewed `#1091` subset.
  Apple stable-toolchain cleanup remains deferred because it is unrelated to
  the Windows GUI and Quest questionnaire control path.
- 2026-06-16: verified the current upstream P0 Makepad PR candidates against
  `upstream/dev` and this fork, then added executable guard signatures for the
  imported/adapted Android packaging, lifecycle, splash, OpenXR, Choreographer,
  font-deduplication, and TextInput inner-alignment slices. The broad upstream
  `#1091` Android overhaul remains an adapted subset in this fork rather than a
  whole-merge import; exact `#520` import is rejected/deferred because it merged
  to historical `rik`, while this fork keeps the current runtime fallback shape.
- 2026-06-15: fixed `TextInputTextPlacement.InnerAlign` to measure/draw the
  text row with neutral internal horizontal layout while using `label_align` for
  the outer inner-rect placement. This prevents double horizontal alignment.
- 2026-06-15: exported `TextInputTextPlacement` through `mod.widgets` so
  script clients can opt in with `TextInputTextPlacement.InnerAlign` instead
  of relying on an unregistered bare enum variant.
- 2026-06-15: added a generic `TextInput` single-line text placement option.
  The default `Layout` mode preserves existing turtle layout behavior; opt-in
  `InnerAlign` positions the text line box inside the input's inner rectangle
  using `label_align`, so fixed-height inputs can center content without
  per-control padding tweaks. This stays in the widget patch family and does
  not add downstream questionnaire, Quest, Manifold, or Morphospace authority.

## Update Policy

Before adding behavior to this fork:

1. Classify the changed files in the patch family table.
2. Decide whether the slice is upstreamable, local Quest/Makepad adapter work,
   compatibility alias work, or generated/vendor sync.
3. Run the smallest validation slot that covers the changed family.
4. Update this ledger when a new family appears, a split lands, or a default
   naming rule changes.
