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
| SDK/JDK/NDK/tool resolution | path helpers, platform/build-tools/Java/NDK preflight, clang wrappers | Later `compile/toolchain.rs` or `compile/sdk_tools.rs`. |
| Keystore sidecar and keystore creation | `keystore_sidecar_path`, `KeystoreSidecar`, `read_keystore_sidecar`, `KeystoreCreateOpts`, `keystore_create` | Completed in `compile/keystore.rs`. |
| Generated wrapper manifest | manifest path rewriting, workspace patch extraction, wrapper arg stripping, lock cache | Later `compile/wrapper_manifest.rs`. |
| Packaging identity and manifest inputs | `ResolvedPackagingInputs`, `resolve_packaging_inputs`, `substitute_manifest_template`, `prepare_build` | Later `compile/packaging_inputs.rs` or `compile/manifest.rs`. |
| Rust build setup | `rust_build`, `compose_android_rustflags`, cargo target dir helpers | Later `compile/rust_build.rs`. |
| Java/R/dex build | `build_r_class`, `compile_java`, `build_dex` | Later `compile/java_build.rs`. |
| APK assembly/signing | `build_unaligned_apk`, `add_rust_library`, resources, zipalign, apksigner | Later split only after shared-lib/resource families are isolated. |
| Shared-library dependency bundling | NDK/local `readelf` scanning and `NEEDED` copy loops | Later `compile/shared_libs.rs`. |
| Resource and font staging | APK and AAB asset/resource helpers | Later `compile/assets.rs`. |
| AAB assembly/signing | AAB path prep, asset/native-lib staging, aapt2, bundletool, jarsigner | Later `compile/aab.rs` after shared-lib/assets extraction. |
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

## Later Slices

Recommended next slices:

1. Split generated wrapper manifest helpers:
   `has_explicit_lib_target`, TOML path rewriting, workspace patch extraction,
   wrapper arg stripping, `write_file_if_changed`, and
   `generate_android_wrapper_manifest`.
2. Split SDK/JDK/NDK toolchain resolution only after wrapper movement, because
   build, AAB, Java, signing, and shared-library paths all use it.
3. Split shared-library dependency bundling before broad APK/AAB assembly
   movement.
4. Split ADB helpers last among tooling-only families unless a device command
   bug requires them sooner.

## Validation

For documentation-only preflight changes:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --no-deps --format-version 1
git diff --check
```

For Rust source movement:

```powershell
python tools\rusty_xr_format.py --changed --check
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
