#!/usr/bin/env python3
"""Check Android package-generation identity surfaces for no-op drift."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]

STATIC_WITNESSES = [
    (
        "tools/cargo_makepad/src/android/compile/wrapper_manifest.rs",
        "write_file_if_changed",
        "wrapper manifest writes stay changed-file only",
    ),
    (
        "tools/cargo_makepad/src/android/compile/wrapper_manifest.rs",
        ".makepad-source-lock.hash",
        "source lock hash cache remains explicit",
    ),
    (
        "tools/cargo_makepad/src/android/compile/rust_build.rs",
        "CARGO_TARGET_DIR",
        "Android target-dir identity stays explicit",
    ),
    (
        "tools/cargo_makepad/src/android/compile/rust_build.rs",
        "ANDROID_NDK_ROOT",
        "NDK root export stays explicit",
    ),
    (
        "tools/cargo_makepad/src/android/compile/toolchain.rs",
        "Resolved Android SDK: platform=",
        "selected SDK/JDK/NDK paths stay observable",
    ),
    (
        "tools/cargo_makepad/src/android/compile/toolchain.rs",
        "resolve_ndk_prebuilt_root",
        "NDK prebuilt selection stays centralized",
    ),
    (
        "tools/cargo_makepad/src/android/compile/packaging_inputs.rs",
        "AndroidManifest.xml",
        "manifest output stays in packaging inputs",
    ),
    (
        "tools/cargo_makepad/src/android/compile/java_build.rs",
        "javac.inputs",
        "Java input cache identity remains explicit",
    ),
]

GENERATED_PATTERNS = [
    ("wrapper_manifest", "makepad-android-wrapper/*/Cargo.toml"),
    ("wrapper_lock", "makepad-android-wrapper/*/Cargo.lock"),
    ("wrapper_source_lock_hash", "makepad-android-wrapper/*/.makepad-source-lock.hash"),
    ("apk_manifest", "makepad-android-apk/*/tmp/AndroidManifest.xml"),
    ("apk_makepad_app_java", "makepad-android-apk/*/tmp/**/MakepadApp.java"),
    ("apk_makepad_app_xr_java", "makepad-android-apk/*/tmp/**/MakepadAppXr.java"),
    ("apk_java_inputs_cache", "makepad-android-apk/*/java/javac.inputs"),
    ("aab_manifest", "makepad-android-aab/*/manifest/AndroidManifest.xml"),
]

ENV_KEYS = [
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "ANDROID_PLATFORM",
    "ANDROID_SDK_VERSION",
    "ANDROID_API_LEVEL",
    "ANDROID_BUILD_TOOLS_VERSION",
    "ANDROID_NDK_ROOT",
    "ANDROID_NDK_PREBUILT_ROOT",
    "JAVA_HOME",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_TOOLCHAIN",
    "MAKEPAD",
]


def rel_path(path: Path) -> str:
    resolved = path.resolve()
    try:
        return resolved.relative_to(REPO_ROOT.resolve()).as_posix()
    except ValueError:
        return resolved.as_posix()


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def check_static_witnesses() -> list[str]:
    failures: list[str] = []
    for rel, needle, label in STATIC_WITNESSES:
        path = REPO_ROOT / rel
        if not path.is_file():
            failures.append(f"{rel}: missing witness file for {label}")
            continue
        if needle not in read_text(path):
            failures.append(f"{rel}: missing {label}: {needle!r}")
    return failures


def snapshot_generated_surface(target_root: Path, require_generated: bool) -> dict[str, Any]:
    target_root = target_root if target_root.is_absolute() else REPO_ROOT / target_root
    files: list[dict[str, Any]] = []
    if target_root.exists():
        for surface, pattern in GENERATED_PATTERNS:
            for path in sorted(target_root.glob(pattern)):
                if path.is_file():
                    files.append(
                        {
                            "surface": surface,
                            "path": rel_path(path),
                            "size": path.stat().st_size,
                            "sha256": sha256_file(path),
                        }
                    )

    if require_generated and not files:
        raise ValueError(f"no generated Android package surfaces found under {target_root}")

    env = {key: os.environ.get(key, "") for key in ENV_KEYS}
    return {
        "schema": "rusty.makepad.android.generated_output_stability.v1",
        "repo_root": str(REPO_ROOT.resolve()),
        "target_root": rel_path(target_root),
        "environment": env,
        "files": files,
    }


def load_snapshot(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema") != "rusty.makepad.android.generated_output_stability.v1":
        raise ValueError(f"{path}: unsupported snapshot schema {data.get('schema')!r}")
    return data


def compare_snapshots(before: dict[str, Any], after: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    before_env = before.get("environment", {})
    after_env = after.get("environment", {})
    for key in ENV_KEYS:
        if before_env.get(key, "") != after_env.get(key, ""):
            failures.append(
                f"environment changed: {key}: {before_env.get(key, '')!r} -> {after_env.get(key, '')!r}"
            )

    def file_map(snapshot: dict[str, Any]) -> dict[tuple[str, str], dict[str, Any]]:
        return {
            (str(item["surface"]), str(item["path"])): item
            for item in snapshot.get("files", [])
        }

    before_files = file_map(before)
    after_files = file_map(after)
    before_keys = set(before_files)
    after_keys = set(after_files)

    for key in sorted(before_keys - after_keys):
        failures.append(f"generated surface disappeared: {key[0]} {key[1]}")
    for key in sorted(after_keys - before_keys):
        failures.append(f"generated surface appeared: {key[0]} {key[1]}")
    for key in sorted(before_keys & after_keys):
        before_item = before_files[key]
        after_item = after_files[key]
        if before_item.get("size") != after_item.get("size"):
            failures.append(
                f"generated surface size changed: {key[0]} {key[1]}: "
                f"{before_item.get('size')} -> {after_item.get('size')}"
            )
        if before_item.get("sha256") != after_item.get("sha256"):
            failures.append(f"generated surface content changed: {key[0]} {key[1]}")

    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target-root",
        default="target/android",
        help="Android target root to snapshot, default: target/android",
    )
    parser.add_argument("--snapshot-out", help="write a generated surface snapshot JSON")
    parser.add_argument("--before", help="previous generated surface snapshot JSON")
    parser.add_argument("--after", help="later generated surface snapshot JSON")
    parser.add_argument(
        "--require-generated",
        action="store_true",
        help="fail if the target root has no generated Android package surfaces",
    )
    parser.add_argument("--json", action="store_true", help="print the current snapshot JSON")
    args = parser.parse_args()

    failures = check_static_witnesses()
    if failures:
        for failure in failures:
            print(f"[FAIL] {failure}", file=sys.stderr)
        print(
            f"Android generated-output stability: fail ({len(failures)} static failures)",
            file=sys.stderr,
        )
        return 1

    if args.before or args.after:
        if not args.before or not args.after:
            print("[FAIL] --before and --after must be supplied together", file=sys.stderr)
            return 1
        try:
            before = load_snapshot(Path(args.before))
            after = load_snapshot(Path(args.after))
        except Exception as exc:  # noqa: BLE001
            print(f"[FAIL] {exc}", file=sys.stderr)
            return 1
        compare_failures = compare_snapshots(before, after)
        if compare_failures:
            for failure in compare_failures:
                print(f"[FAIL] {failure}", file=sys.stderr)
            print(
                f"Android generated-output stability: fail ({len(compare_failures)} diffs)",
                file=sys.stderr,
            )
            return 1
        print("Android generated-output stability: pass (snapshots match)")
        return 0

    try:
        snapshot = snapshot_generated_surface(Path(args.target_root), args.require_generated)
    except Exception as exc:  # noqa: BLE001
        print(f"[FAIL] {exc}", file=sys.stderr)
        return 1

    if args.snapshot_out:
        out_path = Path(args.snapshot_out)
        if not out_path.is_absolute():
            out_path = REPO_ROOT / out_path
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    if args.json:
        print(json.dumps(snapshot, indent=2, sort_keys=True))
    else:
        print(
            "Android generated-output stability: pass "
            f"({len(STATIC_WITNESSES)} static checks, {len(snapshot['files'])} generated files)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
