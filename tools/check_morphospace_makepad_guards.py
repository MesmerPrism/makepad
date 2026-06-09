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
        "tools/cargo_makepad/src/android/java/dev/makepad/android/RustyXrActivitySupport.java",
        "tools/cargo_makepad/src/android/java/dev/makepad/android/RustyXrMediaProjectionHelper.java",
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


def main():
    checks = Checks()
    check_h264_defaults(checks)
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
