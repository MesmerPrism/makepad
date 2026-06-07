# Rusty XR Android Compile Split Plan

This is a mechanical split preflight for
`tools/cargo_makepad/src/android/compile.rs`.

The goal is pressure release, not packaging behavior change. `compile.rs`
should remain the Android build orchestration facade while fork-owned helper
families move into focused modules under `tools/cargo_makepad/src/android/compile/`.

## Current Facade Contract

Keep these stable during split intervals:

- public commands routed through `tools/cargo_makepad/src/android/mod.rs`;
- `compile::build`, `compile::build_aab`, `compile::run`, `compile::adb`,
  `compile::adb_tcp`, `compile::java`, and `compile::javac`;
- `compile::KeystoreCreateOpts`, `compile::keystore_create`, and
  `compile::read_keystore_sidecar`;
- generated wrapper manifest path and lockfile behavior;
- APK and AAB output paths and filenames;
- package id, label, version, minSdk, targetSdk, debuggable, icon, and manifest
  template substitution behavior;
- Rust `CARGO_TARGET_DIR`, Android target env vars, Java/SDK/NDK env vars, and
  rustflags behavior;
- native shared-library dependency bundling behavior;
- resource/font asset staging behavior;
- signing behavior for APK and AAB outputs.

Do not add Manifold, Hostess, Studio, Quest runtime, or app-specific authority
to this Makepad tooling module. It remains package-generation tooling.

## Responsibility Map

| Responsibility | Current location | Split direction |
| --- | --- | --- |
| Android build orchestration | `build`, `build_aab`, `run` | Keep in `compile.rs` until helper families are split. |
| SDK/JDK/NDK/tool resolution | path helpers, platform/build-tools/Java/NDK preflight, clang wrappers | Completed in `compile/toolchain.rs`. |
| Keystore sidecar and keystore creation | `keystore_sidecar_path`, `KeystoreSidecar`, `read_keystore_sidecar`, `KeystoreCreateOpts`, `keystore_create` | Completed in `compile/keystore.rs`. |
| Generated wrapper manifest | manifest path rewriting, workspace patch extraction, wrapper arg stripping, lock cache | Completed in `compile/wrapper_manifest.rs`. |
| Packaging identity and manifest inputs | `ResolvedPackagingInputs`, `resolve_packaging_inputs`, `substitute_manifest_template`, `prepare_build` | Completed in `compile/packaging_inputs.rs`. |
| Rust build setup | `rust_build`, `compose_android_rustflags`, cargo target dir helpers | Completed in `compile/rust_build.rs`. |
| Java/R/dex build | `build_r_class`, `compile_java`, `build_dex` | Completed in `compile/java_build.rs`. |
| APK assembly/signing | `build_unaligned_apk`, `add_rust_library`, zipalign, apksigner | Completed in `compile/apk_assembly.rs`. |
| Shared-library dependency bundling | NDK/local `readelf` scanning and `NEEDED` copy loops | Completed in `compile/shared_libs.rs`. |
| Resource and font staging | APK and AAB asset/resource helpers | Completed in `compile/assets.rs`. |
| AAB assembly/signing | AAB path prep, aapt2 proto APK, base module zip, bundletool, jarsigner | Completed in `compile/aab_assembly.rs`. |
| ADB/device helpers | install/run, `adb`, `adb_tcp`, device/IP parsing | Later `compile/adb.rs`, but only after build packaging helpers are stable. |

## First Code Slice

Status: completed. `compile/keystore.rs` now owns keystore sidecar parsing,
sidecar writing, and upload-keystore creation. `compile.rs` re-exports the
previous public names so `android/mod.rs` call sites remain unchanged.

Completed movement:

1. Add `tools/cargo_makepad/src/android/compile/keystore.rs`.
2. Move `keystore_sidecar_path`, `KeystoreSidecar`,
   `read_keystore_sidecar`, `write_keystore_sidecar`,
   `KeystoreCreateOpts`, and `keystore_create`.
3. Keep `keytool_path` and `resolve_java_home` in the facade for now because
   they are also part of SDK/JDK tool resolution.
4. Preserve all user-facing error strings, keystore sidecar format, keytool
   arguments, `JAVA_HOME` env behavior, and public command routing.

## Second Code Slice

Status: completed. `compile/wrapper_manifest.rs` now owns generated Android
wrapper Cargo manifest generation, wrapper path normalization, workspace patch
section extraction, wrapper cargo-arg stripping, changed-file writes, and
source lockfile hash caching. `compile.rs` still owns the `rust_build`
orchestration call site and imports only the helper functions it needs.

Completed movement:

1. Add `tools/cargo_makepad/src/android/compile/wrapper_manifest.rs`.
2. Move `has_explicit_lib_target`, TOML path rewriting helpers,
   `extract_workspace_patch_sections`, `strip_generated_wrapper_args`,
   `write_file_if_changed`, and `generate_android_wrapper_manifest`.
3. Keep `rust_build`, cargo target-dir derivation, Android target env vars,
   rustflags, and SDK/NDK resolution in the facade for now.
4. Preserve generated wrapper manifest path, workspace patch copying,
   source lockfile hash behavior, stripped cargo args, and user-facing error
   strings.

## Third Code Slice

Status: completed. This interval deliberately groups two related package
tooling families so validation covers a larger but still cohesive movement:
toolchain resolution and native shared-library dependency bundling.

`compile/toolchain.rs` now owns SDK/JDK/NDK path resolution, selected
platform/build-tools values, Java tool lookup, NDK prebuilt selection, clang
wrapper API selection, tool path helpers, and SDK preflight reporting.

`compile/shared_libs.rs` now owns APK and AAB native shared-library dependency
handling: `llvm-readelf` `NEEDED` scans, NDK sysroot filtering, local Rust
dylib dependency traversal, APK `aapt add` insertion, AAB native-lib staging,
and Quest OpenXR loader native-lib staging.

Completed movement:

1. Add `tools/cargo_makepad/src/android/compile/toolchain.rs`.
2. Move SDK/JDK/NDK path helpers, platform/build-tools resolvers, Java tool
   lookup, clang wrapper detection, NDK prebuilt selection, `ndk_bin_path`, and
   `preflight_android_sdk`.
3. Add `tools/cargo_makepad/src/android/compile/shared_libs.rs`.
4. Move APK and AAB shared-library dependency helpers, including local and NDK
   dependency scanning/staging.
5. Keep `rust_build`, `add_rust_library`, `build`, `build_aab`, Java/R/Dex,
   APK/AAB assembly, command routing, and cargo target-dir derivation in the
   facade for now.
6. Preserve tool path selection, env-var fallback order, preflight error
   strings, shared-library inclusion/exclusion behavior, AAB native lib layout,
   and Quest OpenXR loader staging behavior.

## Fourth Code Slice

Status: completed. This interval continues the broader-batch cadence by moving
three package-generation families together: packaging identity/manifest input
preparation, resource/font asset staging, and Java/R/Dex helper execution.

`compile/packaging_inputs.rs` now owns package id, app label, version code,
version name, minSdk override validation, custom/default AndroidManifest
template substitution, generated `MakepadApp`/`MakepadAppXr` Java source, icon
presence checks, and APK output filename derivation.

`compile/assets.rs` now owns APK and AAB resource/font staging, small-font
replacement, dependency resource traversal, duplicate font filtering, Quest
widget-resource pruning, and APK `aapt add` asset insertion.

`compile/java_build.rs` now owns generated R class creation, javac source
hashing and cache stamp behavior, expected class output checks, javac
invocation, class file discovery, and D8 Dex generation.

Completed movement:

1. Add `tools/cargo_makepad/src/android/compile/packaging_inputs.rs`.
2. Add `tools/cargo_makepad/src/android/compile/assets.rs`.
3. Add `tools/cargo_makepad/src/android/compile/java_build.rs`.
4. Keep `build`, `build_aab`, `rust_build`, `add_rust_library`, APK/AAB
   assembly, signing, timing wrappers, command routing, and cargo target-dir
   derivation in the facade for now.
5. Preserve package id/label/version/minSdk behavior, manifest template output,
   generated Java source output, launcher icon warnings, APK/AAB resource
   paths, small-font replacement, Java input hash caching, javac args, and D8
   output behavior.

## Fifth Code Slice

Status: completed. This interval intentionally broadens the batch size by
moving the remaining package assembly/signing helpers and the Rust build setup
helpers together while preserving `compile.rs` as the public command facade.

`compile/apk_assembly.rs` now owns APK packaging and signing helpers:
unaligned APK creation, Rust shared-library insertion, APK zipalign, debug
keystore signing, and Quest OpenXR loader insertion into the APK.

`compile/aab_assembly.rs` now owns AAB assembly and signing helpers: AAB path
preparation, `aapt2` resource compilation/linking, proto-APK extraction into
the base module layout, bundletool execution, jarsigner lookup, and optional
AAB signing options.

`compile/rust_build.rs` now owns Android Rust build setup: generated wrapper
manifest routing, Android cargo target-dir derivation, NDK compiler env vars,
Android SDK/JDK/NDK env vars, Quest `MAKEPAD` env selection, rustflags
composition, and compose-rustflags tests.

Completed movement:

1. Add `tools/cargo_makepad/src/android/compile/apk_assembly.rs`.
2. Add `tools/cargo_makepad/src/android/compile/aab_assembly.rs`.
3. Add `tools/cargo_makepad/src/android/compile/rust_build.rs`.
4. Keep `build`, `build_aab`, `run`, `adb`, `adb_tcp`, `java`, `javac`, phase
   timing, public result structs, and command routing in `compile.rs`.
5. Re-export `compile::AabSigningOpts` from `compile/aab_assembly.rs` so
   `android/mod.rs` call sites remain unchanged.
6. Preserve APK/AAB filenames and paths, `aapt`/`aapt2`/zipalign/apksigner/
   jarsigner/bundletool arguments, debug signing behavior, Rust target-dir
   behavior, generated wrapper behavior, Android env vars, Quest OpenXR loader
   staging, and existing compose-rustflags tests.

## Later Slices

Recommended next slices:

1. Split ADB/device helpers into `compile/adb.rs` if the remaining facade still
   carries device-command pressure.
2. Reassess whether `java`/`javac` passthrough and timing helpers should remain
   in the facade. Do not continue splitting by line count alone once
   `compile.rs` is cohesive.

## Generated Output Stability Preflight

The helper split is not enough by itself; Android packaging is only safe if a
no-change build keeps identity inputs stable. Before changing wrapper,
manifest, Rust build setup, SDK/JDK/NDK resolution, assets, Java/R/Dex, APK,
AAB, signing, or shared-library handling, record the expected stability surface:

| Surface | Stable witness |
| --- | --- |
| Generated wrapper manifest | `compile/wrapper_manifest.rs` keeps `write_file_if_changed`, wrapper path normalization, workspace patch extraction, and source lockfile hash caching. |
| Cargo target identity | `compile/rust_build.rs` owns `CARGO_TARGET_DIR` handling and Android Rust env vars. |
| Toolchain identity | `compile/toolchain.rs` owns selected SDK platform, build-tools, Java tools, NDK prebuilt, and clang API selection. |
| Package identity | `compile/packaging_inputs.rs` owns package id, label, version, min SDK, target manifest template, and output filename derivation. |
| Native library payload | `compile/shared_libs.rs`, `compile/apk_assembly.rs`, and `compile/aab_assembly.rs` own copied `.so` sets, OpenXR loader staging, APK insertion, and AAB native-lib layout. |
| Timing evidence | `compile.rs` keeps `MAKEPAD_ANDROID_TIMING phase=...` markers around the orchestration steps. |

For a behavior-affecting packaging change, run a before/after package generation
comparison or timing-marker review in the downstream Rusty XR Makepad example.
For mechanical helper movement, the repo-local guard is sufficient to prove the
stability hooks are still present.

Use the repo-local snapshot helper for the no-op package surface:

```powershell
python tools\check_android_generated_output_stability.py --snapshot-out target\android\stability-before.json
# Run the same no-op package generation command again, without changing source,
# SDK/JDK/NDK paths, target dir, Cargo home, or package flags.
python tools\check_android_generated_output_stability.py --snapshot-out target\android\stability-after.json
python tools\check_android_generated_output_stability.py --before target\android\stability-before.json --after target\android\stability-after.json
```

The snapshot compares generated wrapper manifests, generated wrapper lockfiles,
source-lock hash cache, generated Android manifests, generated Makepad app Java
sources, javac input cache identity, and the selected SDK/JDK/NDK/Cargo path
environment. Use `--require-generated` when a behavior slice is claiming a real
generated-output comparison rather than only checking static hooks.

## Validation

For documentation-only preflight changes:

```powershell
python tools\rusty_xr_format.py --changed --check
python tools\check_android_generated_output_stability.py
cargo metadata --no-deps --format-version 1
git diff --check
```

For Rust source movement:

```powershell
python tools\rusty_xr_format.py --changed --check
python tools\check_rusty_xr_makepad_guards.py
python tools\check_android_generated_output_stability.py
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
git diff --check
git diff --cached --check
```

For behavior-affecting packaging changes, add:

```powershell
cargo build -p cargo-makepad --release
```

Do not treat an APK install, ADB run, Quest validation, or downstream generated
wrapper rebuild as required for mechanical helper movement unless generated
template behavior, installed packager behavior, or device-facing packaging
behavior changes.

## Stop Conditions

Stop and reassess before committing if a split requires any of these:

- changing generated manifest, wrapper, APK, or AAB outputs;
- changing command-line flags or public command routing;
- changing package ids, labels, version values, SDK versions, or debuggable
  defaults;
- changing cargo target dir, rustflags, Java/SDK/NDK env behavior, or tool
  path resolution;
- changing shared-library dependency inclusion/exclusion behavior;
- adding Manifold, Hostess, Studio, Quest runtime, or app-specific authority to
  Makepad packaging tooling.
