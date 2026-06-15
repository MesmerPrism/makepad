#!/usr/bin/env python3
"""Morphospace Makepad guardrails for the maintained Makepad fork."""

from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[1]


class Checks:
    def __init__(self):
        self.passed = 0
        self.failures = []

    def pass_(self):
        self.passed += 1

    def fail(self, message):
        self.failures.append(message)

    def path(self, rel):
        path = REPO_ROOT / rel
        if not path.exists():
            self.fail(f"{rel}: missing required path")
            return None
        return path

    def text(self, rel):
        path = self.path(rel)
        if path is None:
            return ""
        return path.read_text(encoding="utf-8", errors="replace")

    def contains(self, rel, needle, label):
        data = self.text(rel)
        if needle in data:
            self.pass_()
        else:
            self.fail(f"{rel}: missing {label}: {needle!r}")

    def not_contains(self, rel, needle, label):
        data = self.text(rel)
        if needle not in data:
            self.pass_()
        else:
            self.fail(f"{rel}: forbidden {label}: {needle!r}")

    def literal_is_explicit_legacy(self, rel, literal, legacy_token):
        data = self.text(rel)
        for line_no, line in enumerate(data.splitlines(), start=1):
            if literal in line and legacy_token not in line:
                self.fail(
                    f"{rel}:{line_no}: {literal!r} must stay behind {legacy_token}"
                )
                return
        self.pass_()

    def line_count_at_most(self, rel, limit):
        data = self.text(rel)
        lines = len(data.splitlines())
        if lines <= limit:
            self.pass_()
        else:
            self.fail(f"{rel}: {lines} lines exceeds guard limit {limit}")


def check_h264_defaults(checks):
    command_client = (
        "tools/cargo_makepad/src/android/java/dev/makepad/android/"
        "ManifoldH264CommandClient.java"
    )
    stream_reader = (
        "tools/cargo_makepad/src/android/java/dev/makepad/android/"
        "ManifoldVideoStreamReader.java"
    )
    facade = (
        "tools/cargo_makepad/src/android/java/dev/makepad/android/"
        "BrokerH264VideoPlayer.java"
    )

    checks.contains(
        command_client,
        'MANIFOLD_COMMAND_SCHEMA = "rusty.manifold.command.envelope.v1"',
        "Manifold command schema default",
    )
    checks.contains(
        command_client,
        'MANIFOLD_EVENTS_PATH = "/manifold/v1/events"',
        "Manifold events path default",
    )
    checks.contains(
        command_client,
        'LEGACY_RUSTY_XR_BROKER_COMMAND_SCHEMA = "rusty.xr.broker.command.v1"',
        "explicit legacy command alias",
    )
    checks.contains(
        stream_reader,
        'STREAM_MAGIC = "RMANVID1"',
        "Manifold stream magic default",
    )
    checks.contains(
        stream_reader,
        'LEGACY_STREAM_MAGIC = "RXYRVID1"',
        "explicit legacy stream magic alias",
    )
    checks.not_contains(command_client, "/rustyxr/v1", "legacy route default")
    checks.literal_is_explicit_legacy(
        command_client,
        "rusty.xr.broker.command.v1",
        "LEGACY_RUSTY_XR_BROKER_COMMAND_SCHEMA",
    )
    checks.literal_is_explicit_legacy(
        stream_reader,
        "RXYRVID1",
        "LEGACY_STREAM_MAGIC",
    )
    checks.line_count_at_most(facade, 1100)


def check_upstream_p0_imports(checks):
    """Guard imported/adapted upstream Android and packaging P0 fixes."""

    compile_rs = "tools/cargo_makepad/src/android/compile.rs"
    android_mod = "tools/cargo_makepad/src/android/mod.rs"
    android_sdk = "tools/cargo_makepad/src/android/sdk.rs"
    apk_assembly = "tools/cargo_makepad/src/android/compile/apk_assembly.rs"
    shared_libs = "tools/cargo_makepad/src/android/compile/shared_libs.rs"
    rust_build = "tools/cargo_makepad/src/android/compile/rust_build.rs"
    assets = "tools/cargo_makepad/src/android/compile/assets.rs"
    android_jni = "platform/src/os/linux/android/android_jni.rs"
    android_messages = "platform/src/os/linux/android/android_java_messages.rs"
    android_rs = "platform/src/os/linux/android/android.rs"
    openxr_rs = "platform/src/os/linux/openxr.rs"
    activity = (
        "tools/cargo_makepad/src/android/java/dev/makepad/android/"
        "MakepadActivity.java"
    )

    # makepad/makepad#893: use the crate name, not the binary/app label, for
    # Android Rust shared-library lookup.
    checks.contains(
        compile_rs,
        "let underscore_build_crate = build_crate.replace('-', \"_\");",
        "#893 crate-name shared-library lookup input",
    )
    checks.contains(
        apk_assembly,
        "lib{underscore_target}.so",
        "#893 APK Rust shared-library lookup path",
    )
    checks.contains(
        shared_libs,
        "lib{underscore_target}.so",
        "#893 AAB Rust shared-library lookup path",
    )

    # makepad/makepad#1091 packaging subset: API floor/target split, AAB route,
    # stable Android toolchain, manifest templates, and system bar appearance.
    checks.contains(android_sdk, "sdk_version: 26,", "#1091 min SDK floor")
    checks.contains(android_sdk, "target_sdk_version: 35,", "#1091 target SDK")
    checks.contains(
        android_sdk,
        "ensure_rust_toolchain_installed(\"stable\")",
        "#1091 stable Android Rust toolchain install",
    )
    checks.contains(android_sdk, "BUNDLETOOL_JAR_REL", "#1091 bundletool install")
    checks.contains(android_mod, "build-aab", "#1091 Android App Bundle command")
    checks.contains(android_mod, "keystore-create", "#1091 keystore command")
    checks.contains(
        android_mod,
        "{min_sdk_version}",
        "#1091 manifest minSdk template variable",
    )
    checks.contains(
        android_mod,
        "{target_sdk_version}",
        "#1091 manifest targetSdk template variable",
    )
    checks.contains(
        compile_rs,
        "pub fn build_aab",
        "#1091 AAB build entry point",
    )
    checks.contains(
        rust_build,
        "\"stable\"",
        "#1091 stable Android cargo invocation",
    )
    checks.contains(
        "platform/src/display_context.rs",
        "pub enum SystemBarAppearance",
        "#1091 system bar appearance API",
    )
    checks.contains(
        activity,
        "setSystemBarAppearance",
        "#1091 Android system bar appearance bridge",
    )

    # makepad/makepad#1030: surface destruction is acknowledged synchronously
    # and drawing is gated on surface validity.
    checks.contains(android_jni, "pub type SurfaceAck", "#1030 surface ack type")
    checks.contains(android_jni, "wait_surface_ack", "#1030 surface ack wait")
    checks.contains(
        android_messages,
        "FromJavaMessage::SurfaceDestroyed { ack }",
        "#1030 acknowledged SurfaceDestroyed message",
    )
    checks.contains(
        android_messages,
        "self.os.surface_alive = false",
        "#1030 synchronous surface_alive clear",
    )
    checks.contains(
        android_rs,
        "pub(crate) fn has_drawable_surface",
        "#1030 drawable-surface gate",
    )
    checks.contains(
        android_rs,
        "pub(crate) fn try_make_current",
        "#1030 fallible EGL rebind",
    )

    # makepad/makepad#1043 and #895: launch/resume surface cover and Android
    # splash theme support.
    checks.contains(
        android_rs,
        "hide_surface_cover_after_first_present",
        "#1043 hide launch cover after first present",
    )
    checks.contains(
        android_jni,
        "to_java_set_surface_cover_visible",
        "#1043 surface cover JNI bridge",
    )
    checks.contains(
        activity,
        "mSurfaceRecoveryOverlayVisible",
        "#1043 surface recovery overlay state",
    )
    checks.contains(
        "tools/cargo_makepad/src/android/res/values/styles.xml",
        "MakepadLaunchTheme",
        "#895 pre-Android-12 launch theme",
    )
    checks.contains(
        "tools/cargo_makepad/src/android/res/values-v31/styles.xml",
        "windowSplashScreenBackground",
        "#895 Android 12 splash theme",
    )
    checks.contains(
        android_mod,
        "android:theme=\"@style/MakepadLaunchTheme\"",
        "#895 generated manifest launch theme",
    )

    # makepad/makepad#977: Linux NDK install extracts the whole zip before
    # copying the needed subtree instead of relying on a fragile unzip glob.
    checks.contains(
        android_sdk,
        "Extract the entire NDK zip, then copy the needed subtree(s).",
        "#977 Linux NDK unzip strategy",
    )
    checks.contains(
        android_sdk,
        "android-ndk-r28b",
        "#977 explicit NDK zip root",
    )

    # makepad/makepad#1073: mobile bundles do not duplicate font files that
    # already ship in a sibling resources directory.
    checks.contains(
        assets,
        "Skip files that already ship from the sibling `resources/` dir",
        "#1073 mobile font deduplication",
    )
    checks.contains(
        assets,
        "resource_dir.join(path).is_file()",
        "#1073 sibling-resource font deduplication check",
    )

    # makepad/makepad#989: OpenXR extension setup remains valid for Android
    # builds without the Vulkan backend enabled.
    checks.contains(
        openxr_rs,
        "#[cfg(not(use_vulkan))]\n        let mut exts_needed = vec![",
        "#989 non-Vulkan OpenXR extension list",
    )

    # makepad/makepad#520 was merged to the old `rik` branch, not current
    # upstream dev. Keep only the compatible runtime fallback shape.
    checks.contains(
        android_jni,
        "CHOREOGRAPHER_POST_CALLBACK_FN",
        "#520-compatible Choreographer callback lookup",
    )
    checks.contains(
        android_jni,
        "no_android_choreographer",
        "#520-compatible Choreographer fallback cfg",
    )


def check_text_input_alignment(checks):
    text_input = "widgets/src/text_input.rs"

    checks.contains(
        text_input,
        "pub enum TextInputTextPlacement",
        "TextInput placement enum",
    )
    checks.contains(
        text_input,
        "InnerAlign",
        "TextInput inner alignment placement",
    )
    checks.contains(
        text_input,
        "mod.widgets.TextInputTextPlacement",
        "script-visible TextInput placement enum",
    )
    checks.contains(
        text_input,
        "fn inner_aligned_text_origin",
        "TextInput inner alignment origin helper",
    )
    checks.contains(
        text_input,
        "fn text_layout_align",
        "TextInput neutral internal alignment helper",
    )
    checks.contains(
        text_input,
        "text_input_aligned_origin_y_places_line_box_center",
        "TextInput alignment unit test",
    )
    checks.contains(
        "examples/uizoo/src/tab_textinput.rs",
        "TextInputTextPlacement.InnerAlign",
        "UIZoo TextInput inner alignment example",
    )


def check_split_maps(checks):
    required_modules = [
        "tools/cargo_makepad/src/android/compile/aab_assembly.rs",
        "tools/cargo_makepad/src/android/compile/apk_assembly.rs",
        "tools/cargo_makepad/src/android/compile/assets.rs",
        "tools/cargo_makepad/src/android/compile/java_build.rs",
        "tools/cargo_makepad/src/android/compile/keystore.rs",
        "tools/cargo_makepad/src/android/compile/packaging_inputs.rs",
        "tools/cargo_makepad/src/android/compile/rust_build.rs",
        "tools/cargo_makepad/src/android/compile/shared_libs.rs",
        "tools/cargo_makepad/src/android/compile/toolchain.rs",
        "tools/cargo_makepad/src/android/compile/wrapper_manifest.rs",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ExternalH264Config.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ExternalH264CpuYuvEmitter.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ExternalH264HardwareBufferTarget.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ExternalH264VideoPlaybackFactory.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/H264AnnexBPrimer.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ManifoldH264CommandClient.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/ManifoldVideoStreamReader.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/MorphospaceActivitySupport.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/MorphospaceMediaProjectionHelper.java",
        "platform/src/os/linux/android/android_java_messages.rs",
        "platform/src/os/linux/vulkan/basic_compute_probe.rs",
        "platform/src/os/linux/vulkan/buffer_resources.rs",
        "platform/src/os/linux/vulkan/draw_recording.rs",
        "platform/src/os/linux/vulkan/frame_resources.rs",
        "platform/src/os/linux/vulkan/openxr_targets.rs",
        "platform/src/os/linux/vulkan/pipeline_resources.rs",
        "platform/src/os/linux/vulkan/texture_resources.rs",
        "platform/src/os/linux/vulkan/texture_lifetime.rs",
        "platform/src/os/linux/vulkan/video_hardware_buffer.rs",
        "platform/src/os/linux/vulkan/skinning_probe.rs",
        "platform/src/os/linux/vulkan/skinning_mesh_probe.rs",
        "platform/src/os/linux/vulkan/mesh_sdf_probe.rs",
        "platform/src/os/linux/vulkan/field_sample_probe.rs",
        "platform/src/os/linux/vulkan/field_force_sample_probe.rs",
        "platform/src/os/linux/vulkan/volume_probe.rs",
        "platform/src/os/linux/vulkan/volume_raymarch_preview.rs",
        "platform/src/os/linux/vulkan/volume_image_preview.rs",
    ]
    for rel in required_modules:
        if checks.path(rel) is not None:
            checks.pass_()

    compile_rs = "tools/cargo_makepad/src/android/compile.rs"
    checks.line_count_at_most(compile_rs, 1200)
    checks.line_count_at_most(
        "tools/cargo_makepad/src/android/java/dev/makepad/android/MakepadActivity.java",
        3200,
    )
    checks.line_count_at_most("platform/src/os/linux/android/android.rs", 4200)
    checks.contains(
        "platform/src/os/linux/android/android_java_messages.rs",
        "fn handle_android_surface_message",
        "Android Java surface/window dispatch helper",
    )
    checks.contains(
        "platform/src/os/linux/android/android_java_messages.rs",
        "fn handle_android_input_message",
        "Android Java input/IME dispatch helper",
    )
    checks.contains(
        "platform/src/os/linux/android/android_java_messages.rs",
        "fn handle_android_video_message",
        "Android Java video/camera dispatch helper",
    )
    checks.contains(
        "tools/cargo_makepad/src/android/compile/wrapper_manifest.rs",
        "write_file_if_changed",
        "changed-file wrapper write helper",
    )
    checks.contains(
        "tools/cargo_makepad/src/android/compile/wrapper_manifest.rs",
        "source_lock_hash",
        "source lock hash cache",
    )
    checks.contains(
        "tools/cargo_makepad/src/android/compile/rust_build.rs",
        "CARGO_TARGET_DIR",
        "stable Cargo target-dir handling",
    )
    checks.contains(
        "tools/check_android_generated_output_stability.py",
        "GENERATED_PATTERNS",
        "generated-output stability snapshot surfaces",
    )
    checks.contains(
        "tools/check_android_generated_output_stability.py",
        "compare_snapshots",
        "generated-output stability compare mode",
    )
    checks.contains(
        "tools/check_all.ps1",
        "check_android_generated_output_stability.py",
        "generated-output stability check_all wiring",
    )
    checks.contains(
        compile_rs,
        "MAKEPAD_ANDROID_TIMING phase=",
        "Android timing marker",
    )
    checks.line_count_at_most("platform/src/os/linux/vulkan.rs", 3000)
    checks.contains(
        "platform/src/os/linux/vulkan/buffer_resources.rs",
        "pub(super) fn create_host_buffer",
        "Vulkan host-buffer allocation owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/buffer_resources.rs",
        "pub(super) fn ensure_geometry_resource",
        "Vulkan geometry-resource cache owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/buffer_resources.rs",
        "pub(super) fn find_memory_type",
        "Vulkan memory-type lookup owner",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn create_host_buffer(",
        "root-owned Vulkan host-buffer allocation",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn ensure_geometry_resource(",
        "root-owned Vulkan geometry-resource cache",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn find_memory_type(",
        "root-owned Vulkan memory-type lookup",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/frame_resources.rs",
        "pub(super) fn alloc_frame_descriptor_set",
        "Vulkan frame descriptor allocation owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/frame_resources.rs",
        "fn create_frame_descriptor_pool",
        "Vulkan frame descriptor-pool creation owner",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn alloc_frame_descriptor_set(",
        "root-owned Vulkan frame descriptor allocation",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn create_frame_descriptor_pool(",
        "root-owned Vulkan frame descriptor-pool creation",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/draw_recording.rs",
        "pub(super) fn record_draw_list",
        "Vulkan draw-list traversal owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/draw_recording.rs",
        "pub(super) fn record_draw_packet",
        "Vulkan draw-packet command recording owner",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn record_draw_packet(",
        "root-owned Vulkan draw-packet recording",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn record_draw_list(",
        "root-owned Vulkan draw-list traversal",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/pipeline_resources.rs",
        "pub(super) fn ensure_pipeline",
        "Vulkan graphics pipeline creation owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/pipeline_resources.rs",
        "create_graphics_pipelines",
        "Vulkan graphics pipeline create call owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/pipeline_resources.rs",
        "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_SHADER_INTERFACE",
        "Vulkan video shader interface marker owner",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "fn ensure_pipeline(",
        "root-owned Vulkan graphics pipeline creation",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/basic_compute_probe.rs",
        "submit_xr_storage_buffer_probe",
        "basic storage-buffer probe owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/basic_compute_probe.rs",
        "submit_xr_u32_compute_probe",
        "basic u32 compute probe owner",
    )
    checks.contains(
        "platform/src/os/linux/vulkan/basic_compute_probe.rs",
        "submit_xr_f32_force_probe",
        "basic f32 force probe owner",
    )
    checks.not_contains(
        "platform/src/os/linux/vulkan.rs",
        "XR_GPU_U32_COMPUTE_PROBE_WGSL",
        "root-owned basic compute probe shader",
    )


def check_docs(checks):
    docs = [
        "AGENTS.md",
        "MORPHOSPACE_MAKEPAD_FORK_NOTES.md",
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md",
        "MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md",
        "MORPHOSPACE_MAKEPAD_ACTIVITY_SPLIT_PLAN.md",
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
    ]
    for rel in docs:
        checks.not_contains(
            rel,
            "MAKEPAD_Q2Q_PARALLEL_APPROACH_COMPARISON",
            "stale public Rusty XR doc pointer",
        )

    checks.contains(
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
        "rename-on-touch",
        "marker compatibility classification",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
        "Runtime Marker Decisions",
        "runtime marker decision table",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
        "debug.rustyxr.xr.display.refresh.rate.hz",
        "debug.rustyxr compatibility classification",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
        "rusty.xr.makepad-broker-h264-*",
        "rusty.xr.makepad broker H264 classification",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_MARKER_BOUNDARY.md",
        "Manifold-owned only if the event becomes a Manifold contract",
        "Manifold replacement boundary",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md",
        "Decoder Loop Preflight",
        "decoder loop preflight section",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md",
        "Decoder Loop Ownership Map",
        "decoder loop ownership map",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_H264_ADAPTER_SPLIT_PLAN.md",
        "BrokerH264VideoPlayer.java remains the decoder orchestrator",
        "decoder split stop decision",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md",
        "Generated Output Stability Preflight",
        "generated output stability section",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_ANDROID_COMPILE_SPLIT_PLAN.md",
        "check_android_generated_output_stability.py --snapshot-out",
        "generated output snapshot command",
    )
    checks.contains(
        "AGENTS.md",
        "legacy/public Rusty XR Makepad examples",
        "Makepad dependency boundary in agent notes",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_FORK_NOTES.md",
        "Keep Manifold, Manifold packages, Rusty core/CLI crates, descriptor repos",
        "Makepad dependency boundary in fork notes",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "Hostess Makepad shell crates",
        "Makepad dependency boundary in patch ledger",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "This is a watchlist, not an active split queue",
        "split-pressure watchlist policy",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "do not continue splitting `MakepadActivity.java` by line",
        "MakepadActivity facade stop condition",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "leave `BrokerH264VideoPlayer.java` as the decoder",
        "H264 decoder facade stop condition",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "do not continue splitting `compile.rs` by line",
        "compile facade stop condition",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "Platform/video watchlist",
        "platform video watchlist",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "basic_compute_probe.rs",
        "basic Vulkan probe module map",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "buffer_resources.rs",
        "Vulkan buffer resource module map",
    )
    checks.contains(
        "MORPHOSPACE_MAKEPAD_PATCH_LEDGER.md",
        "pipeline_resources.rs",
        "Vulkan pipeline resource module map",
    )


def main():
    checks = Checks()
    check_h264_defaults(checks)
    check_upstream_p0_imports(checks)
    check_text_input_alignment(checks)
    check_split_maps(checks)
    check_docs(checks)

    if checks.failures:
        for failure in checks.failures:
            print(f"[FAIL] {failure}", file=sys.stderr)
        print(
            f"Morphospace Makepad guardrails: fail "
            f"({len(checks.failures)} failures, {checks.passed} passes)",
            file=sys.stderr,
        )
        return 1

    print(f"Morphospace Makepad guardrails: pass ({checks.passed} checks)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
