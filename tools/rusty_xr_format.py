#!/usr/bin/env python3
"""Format this Makepad fork without traversing vendored path dependencies.

`cargo fmt --all` intentionally formats workspace packages plus every local
path dependency. This fork vendors many crates with pruned tests, benches, and
examples, so that command can touch unrelated code before failing. This helper
uses Cargo metadata to stay on the workspace-member surface.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Iterable


REPO_ROOT = Path(__file__).resolve().parents[1]


def run(args: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=REPO_ROOT,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def output(proc: subprocess.CompletedProcess[str]) -> str:
    return (proc.stdout or "") + (proc.stderr or "")


def load_metadata() -> dict:
    proc = run(["cargo", "metadata", "--format-version", "1", "--no-deps"], capture=True)
    if proc.returncode != 0:
        sys.stderr.write(output(proc))
        raise SystemExit(proc.returncode)
    return json.loads(proc.stdout)


def workspace_packages(metadata: dict) -> list[dict]:
    workspace_ids = set(metadata["workspace_members"])
    return [pkg for pkg in metadata["packages"] if pkg["id"] in workspace_ids]


def workspace_roots(metadata: dict) -> list[Path]:
    roots = []
    for pkg in workspace_packages(metadata):
        roots.append(Path(pkg["manifest_path"]).resolve().parent)
    return sorted(set(roots), key=lambda path: len(str(path)), reverse=True)


def rel(path: Path) -> str:
    return os.path.relpath(path, REPO_ROOT).replace(os.sep, "/")


def is_under(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def changed_rust_files(metadata: dict) -> tuple[list[Path], list[Path]]:
    changed_proc = run(
        ["git", "diff", "--name-only", "--diff-filter=ACMR", "HEAD", "--", "*.rs"],
        capture=True,
    )
    if changed_proc.returncode != 0:
        sys.stderr.write(output(changed_proc))
        raise SystemExit(changed_proc.returncode)
    untracked_proc = run(
        ["git", "ls-files", "--others", "--exclude-standard", "--", "*.rs"],
        capture=True,
    )
    if untracked_proc.returncode != 0:
        sys.stderr.write(output(untracked_proc))
        raise SystemExit(untracked_proc.returncode)

    roots = workspace_roots(metadata)
    selected: list[Path] = []
    skipped: list[Path] = []
    candidate_paths = sorted(
        set(changed_proc.stdout.splitlines()) | set(untracked_proc.stdout.splitlines())
    )
    for raw in candidate_paths:
        path = (REPO_ROOT / raw).resolve()
        if any(is_under(path, root) for root in roots):
            selected.append(path)
        else:
            skipped.append(path)
    return sorted(selected), sorted(skipped)


def chunked(items: list[str], *, max_chars: int = 24_000) -> Iterable[list[str]]:
    chunk: list[str] = []
    size = 0
    for item in items:
        item_size = len(item) + 1
        if chunk and size + item_size > max_chars:
            yield chunk
            chunk = []
            size = 0
        chunk.append(item)
        size += item_size
    if chunk:
        yield chunk


def rustfmt_files(files: list[Path], *, check: bool) -> int:
    if not files:
        print("No changed workspace Rust files to format.")
        return 0

    paths = [str(path) for path in files]
    base = ["rustfmt"]
    if check:
        base.append("--check")
    base.extend(["--config", "skip_children=true"])

    exit_code = 0
    for group in chunked(paths):
        proc = run(base + group)
        if proc.returncode != 0:
            exit_code = proc.returncode
    return exit_code


def cargo_fmt_workspace(metadata: dict, *, check: bool, allow_write: bool) -> int:
    if not check and not allow_write:
        sys.stderr.write(
            "Refusing workspace-wide formatting writes. Re-run with "
            "--allow-workspace-write when a bulk first-party format change is "
            "intentional.\n"
        )
        return 2

    specs = [pkg["name"] for pkg in workspace_packages(metadata)]
    exit_code = 0
    for group in chunked(specs):
        cmd = ["cargo", "fmt"]
        for spec in group:
            cmd.extend(["-p", spec])
        if check:
            cmd.extend(["--", "--check"])
        proc = run(cmd)
        if proc.returncode != 0:
            exit_code = proc.returncode
    return exit_code


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--changed",
        action="store_true",
        help="format only changed Rust files inside workspace members (default)",
    )
    mode.add_argument(
        "--workspace",
        action="store_true",
        help="run cargo fmt over workspace members, excluding local path deps",
    )
    parser.add_argument("--check", action="store_true", help="check formatting only")
    parser.add_argument(
        "--allow-workspace-write",
        action="store_true",
        help="permit --workspace without --check",
    )
    parser.add_argument("--list", action="store_true", help="print selected targets")
    args = parser.parse_args()

    metadata = load_metadata()
    if args.workspace:
        if args.list:
            for pkg in workspace_packages(metadata):
                print(pkg["name"])
            return 0
        return cargo_fmt_workspace(
            metadata,
            check=args.check,
            allow_write=args.allow_workspace_write,
        )

    selected, skipped = changed_rust_files(metadata)
    if args.list:
        for path in selected:
            print(rel(path))
        for path in skipped:
            print(f"skipped non-workspace path: {rel(path)}", file=sys.stderr)
        return 0

    for path in skipped:
        print(f"Skipping non-workspace Rust path: {rel(path)}", file=sys.stderr)
    return rustfmt_files(selected, check=args.check)


if __name__ == "__main__":
    raise SystemExit(main())
