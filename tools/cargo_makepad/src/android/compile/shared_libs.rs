use super::super::sdk::AndroidSDKUrls;
use super::{
    cargo_target_dir,
    toolchain::{aapt_path, ndk_bin_path, resolve_compiler_api_level, resolve_ndk_prebuilt_root},
    BuildPaths,
};
use crate::android::{AndroidTarget, AndroidVariant, HostOs};
use crate::makepad_shell::{cp, mkdir, shell_env_cap};
use crate::utils::get_profile_from_args;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn bundle_ndk_shared_deps(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    android_target: &AndroidTarget,
    so_path: &Path,
    abi: &str,
    build_paths: &BuildPaths,
) -> Result<(), String> {
    let (_ndk_version, ndk_prebuilt_root) =
        resolve_ndk_prebuilt_root(sdk_dir, host_os, urls.ndk_version_full)?;
    let compiler_api =
        resolve_compiler_api_level(host_os, urls, &ndk_prebuilt_root, android_target)?;

    let readelf_path = ndk_bin_path(&ndk_prebuilt_root, host_os, "llvm-readelf");
    if !readelf_path.exists() {
        return Ok(());
    }

    let cwd = std::env::current_dir().unwrap();
    let output = shell_env_cap(
        &[],
        &cwd,
        readelf_path.to_str().unwrap(),
        &["-d", so_path.to_str().unwrap()],
    )?;

    let clang_triple = android_target.clang();
    let sysroot_lib_dir = ndk_prebuilt_root.join("sysroot/usr/lib").join(clang_triple);

    for line in output.lines() {
        if !line.contains("(NEEDED)") {
            continue;
        }
        let lib_name = match line.find('[').and_then(|start| {
            line[start + 1..]
                .find(']')
                .map(|end| &line[start + 1..start + 1 + end])
        }) {
            Some(name) => name,
            None => continue,
        };

        let candidate = sysroot_lib_dir.join(lib_name);
        if !candidate.exists() || !candidate.is_file() {
            continue;
        }

        let api_level_stub = sysroot_lib_dir
            .join(compiler_api.to_string())
            .join(lib_name);
        if api_level_stub.exists() {
            continue;
        }

        let binary_path = format!("lib/{abi}/{lib_name}");
        let dst_lib = build_paths.out_dir.join(&binary_path);
        cp(&candidate, &dst_lib, false)?;

        shell_env_cap(
            &[],
            &build_paths.out_dir,
            aapt_path(sdk_dir, urls).to_str().unwrap(),
            &[
                "add",
                build_paths.dst_unaligned_apk.to_str().unwrap(),
                &binary_path,
            ],
        )?;

        println!("  Bundled NDK shared dep: {lib_name} (for {abi})");
    }
    Ok(())
}

fn read_needed_shared_libs(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    so_path: &Path,
) -> Result<Vec<String>, String> {
    let (_ndk_version, ndk_prebuilt_root) =
        resolve_ndk_prebuilt_root(sdk_dir, host_os, urls.ndk_version_full)?;

    let readelf_path = ndk_bin_path(&ndk_prebuilt_root, host_os, "llvm-readelf");
    if !readelf_path.exists() {
        return Ok(Vec::new());
    }

    let cwd = std::env::current_dir().unwrap();
    let output = shell_env_cap(
        &[],
        &cwd,
        readelf_path.to_str().unwrap(),
        &["-d", so_path.to_str().unwrap()],
    )?;

    let mut libs = Vec::new();
    for line in output.lines() {
        if !line.contains("(NEEDED)") {
            continue;
        }
        let Some(lib_name) = line.find('[').and_then(|start| {
            line[start + 1..]
                .find(']')
                .map(|end| &line[start + 1..start + 1 + end])
        }) else {
            continue;
        };
        libs.push(lib_name.to_string());
    }
    Ok(libs)
}

pub(super) fn bundle_local_shared_deps(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    android_target: &AndroidTarget,
    so_path: &Path,
    abi: &str,
    build_paths: &BuildPaths,
    build_dir: &Path,
) -> Result<(), String> {
    let mut pending = vec![so_path.to_path_buf()];
    let mut visited = HashSet::<String>::new();
    let search_dirs = [build_dir.to_path_buf(), build_dir.join("deps")];

    while let Some(current_so) = pending.pop() {
        for lib_name in read_needed_shared_libs(sdk_dir, host_os, urls, &current_so)? {
            if !visited.insert(lib_name.clone()) {
                continue;
            }

            let binary_path = format!("lib/{abi}/{lib_name}");
            let dst_lib = build_paths.out_dir.join(&binary_path);
            if dst_lib.exists() {
                continue;
            }

            let candidate = search_dirs
                .iter()
                .map(|dir| dir.join(&lib_name))
                .find(|path| path.is_file())
                .or_else(|| find_rustup_shared_lib(android_target, &lib_name));
            let Some(candidate) = candidate else {
                continue;
            };

            cp(&candidate, &dst_lib, false)?;
            shell_env_cap(
                &[],
                &build_paths.out_dir,
                aapt_path(sdk_dir, urls).to_str().unwrap(),
                &[
                    "add",
                    build_paths.dst_unaligned_apk.to_str().unwrap(),
                    &binary_path,
                ],
            )?;

            bundle_ndk_shared_deps(
                sdk_dir,
                host_os,
                urls,
                android_target,
                &dst_lib,
                abi,
                build_paths,
            )?;

            eprintln!("  Bundled local shared dep: {lib_name} (for {abi})");
            pending.push(candidate);
        }
    }

    Ok(())
}

fn find_rustup_shared_lib(android_target: &AndroidTarget, lib_name: &str) -> Option<PathBuf> {
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".rustup"))
        })?;
    let toolchains_dir = rustup_home.join("toolchains");
    let tail = Path::new("lib")
        .join("rustlib")
        .join(android_target.toolchain())
        .join("lib")
        .join(lib_name);

    for entry in fs::read_dir(toolchains_dir).ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let candidate = entry.path().join(&tail);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn stage_ndk_shared_deps_for_so(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    android_target: &AndroidTarget,
    so_path: &Path,
    abi: &str,
    libs_root: &Path,
) -> Result<(), String> {
    let (_ndk_version, ndk_prebuilt_root) =
        resolve_ndk_prebuilt_root(sdk_dir, host_os, urls.ndk_version_full)?;
    let compiler_api =
        resolve_compiler_api_level(host_os, urls, &ndk_prebuilt_root, android_target)?;
    let readelf_path = ndk_bin_path(&ndk_prebuilt_root, host_os, "llvm-readelf");
    if !readelf_path.exists() {
        return Ok(());
    }
    let cwd = std::env::current_dir().unwrap();
    let output = shell_env_cap(
        &[],
        &cwd,
        readelf_path.to_str().unwrap(),
        &["-d", so_path.to_str().unwrap()],
    )?;
    let sysroot_lib_dir = ndk_prebuilt_root
        .join("sysroot/usr/lib")
        .join(android_target.clang());
    for line in output.lines() {
        if !line.contains("(NEEDED)") {
            continue;
        }
        let Some(lib_name) = line.find('[').and_then(|start| {
            line[start + 1..]
                .find(']')
                .map(|end| &line[start + 1..start + 1 + end])
        }) else {
            continue;
        };
        let candidate = sysroot_lib_dir.join(lib_name);
        if !candidate.exists() || !candidate.is_file() {
            continue;
        }
        let api_level_stub = sysroot_lib_dir
            .join(compiler_api.to_string())
            .join(lib_name);
        if api_level_stub.exists() {
            continue;
        }
        let dst_lib = libs_root.join(abi).join(lib_name);
        if dst_lib.exists() {
            continue;
        }
        cp(&candidate, &dst_lib, false)?;
        println!("  Bundled NDK shared dep: {lib_name} (for {abi})");
    }
    Ok(())
}

fn stage_local_shared_deps(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    android_target: &AndroidTarget,
    so_path: &Path,
    abi: &str,
    libs_root: &Path,
    build_dir: &Path,
) -> Result<(), String> {
    let mut pending = vec![so_path.to_path_buf()];
    let mut visited = HashSet::<String>::new();
    let search_dirs = [build_dir.to_path_buf(), build_dir.join("deps")];
    while let Some(current_so) = pending.pop() {
        for lib_name in read_needed_shared_libs(sdk_dir, host_os, urls, &current_so)? {
            if !visited.insert(lib_name.clone()) {
                continue;
            }
            let dst_lib = libs_root.join(abi).join(&lib_name);
            if dst_lib.exists() {
                continue;
            }
            let candidate = search_dirs
                .iter()
                .map(|dir| dir.join(&lib_name))
                .find(|path| path.is_file())
                .or_else(|| find_rustup_shared_lib(android_target, &lib_name));
            let Some(candidate) = candidate else {
                continue;
            };
            cp(&candidate, &dst_lib, false)?;
            stage_ndk_shared_deps_for_so(
                sdk_dir,
                host_os,
                urls,
                android_target,
                &dst_lib,
                abi,
                libs_root,
            )?;
            println!("  Bundled local shared dep: {lib_name} (for {abi})");
            pending.push(candidate);
        }
    }
    Ok(())
}

pub(super) fn stage_aab_native_libs(
    sdk_dir: &Path,
    host_os: HostOs,
    underscore_target: &str,
    libs_root: &Path,
    android_targets: &[AndroidTarget],
    args: &[String],
    variant: &AndroidVariant,
    urls: &AndroidSDKUrls,
) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().unwrap();
    let target_dir = cargo_target_dir(&cwd);
    let profile = get_profile_from_args(args);
    let mut build_dir = None;
    for android_target in android_targets {
        let abi = android_target.abi_identifier();
        mkdir(&libs_root.join(abi))?;

        let android_target_dir = android_target.toolchain();
        if profile == "debug" {
            println!("WARNING - compiling a DEBUG build of the application, this creates a very slow and big app. Try adding --release for a fast, or --profile=small for a small build.");
        }
        let src_lib = target_dir.join(format!(
            "{android_target_dir}/{profile}/lib{underscore_target}.so"
        ));
        let current_build_dir = target_dir.join(format!("{android_target_dir}/{profile}"));
        build_dir = Some(current_build_dir.clone());
        let dst_lib = libs_root.join(abi).join("libmakepad.so");
        cp(&src_lib, &dst_lib, false)?;

        stage_ndk_shared_deps_for_so(
            sdk_dir,
            host_os,
            urls,
            android_target,
            &dst_lib,
            abi,
            libs_root,
        )?;
        stage_local_shared_deps(
            sdk_dir,
            host_os,
            urls,
            android_target,
            &src_lib,
            abi,
            libs_root,
            &current_build_dir,
        )?;
    }
    if let AndroidVariant::Quest = variant {
        let cargo_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (rel_path, src_lib) in [("arm64-v8a/libopenxr_loader.so", "quest/libopenxr_loader.so")]
        {
            let src_lib = cargo_manifest_dir.join(src_lib);
            let dst_lib = libs_root.join(rel_path);
            cp(&src_lib, &dst_lib, false)?;
        }
    }
    build_dir.ok_or_else(|| "No Android targets selected for AAB native libs".to_string())
}
