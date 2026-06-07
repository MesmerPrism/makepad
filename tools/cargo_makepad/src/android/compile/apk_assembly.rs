use super::super::sdk::AndroidSDKUrls;
use super::{
    rust_build::cargo_target_dir,
    shared_libs::{bundle_local_shared_deps, bundle_ndk_shared_deps},
    toolchain::{
        aapt_path, android_jar_path, apksigner_jar_path, java_tool_path, resolve_java_home,
        zipalign_path,
    },
    BuildPaths,
};
use crate::android::{AndroidTarget, AndroidVariant, HostOs};
use crate::makepad_shell::{cp, mkdir, shell_env, shell_env_cap};
use crate::utils::get_profile_from_args;
use std::path::{Path, PathBuf};

pub(super) fn build_unaligned_apk(
    sdk_dir: &Path,
    host_os: HostOs,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    let java_home = resolve_java_home(sdk_dir, host_os);

    shell_env(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        aapt_path(sdk_dir, urls).to_str().unwrap(),
        &[
            "package",
            "-f",
            "-F",
            (build_paths.dst_unaligned_apk.to_str().unwrap()),
            "-I",
            (android_jar_path(sdk_dir, urls).to_str().unwrap()),
            "-M",
            (build_paths.manifest_file.to_str().unwrap()),
            "-S",
            (build_paths.res_dir.to_str().unwrap()),
            (build_paths.out_dir.to_str().unwrap()),
        ],
    )?;

    Ok(())
}

pub(super) fn add_rust_library(
    sdk_dir: &Path,
    host_os: HostOs,
    underscore_target: &str,
    build_paths: &BuildPaths,
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
        mkdir(&build_paths.out_dir.join(format!("lib/{abi}")))?;

        let android_target_dir = android_target.toolchain();
        let binary_path = format!("lib/{abi}/libmakepad.so");
        if profile == "debug" {
            println!("WARNING - compiling a DEBUG build of the application, this creates a very slow and big app. Try adding --release for a fast, or --profile=small for a small build.");
        }
        let src_lib = target_dir.join(format!(
            "{android_target_dir}/{profile}/lib{underscore_target}.so"
        ));
        let current_build_dir = target_dir.join(format!("{android_target_dir}/{profile}"));
        build_dir = Some(current_build_dir.clone());
        let dst_lib = build_paths.out_dir.join(binary_path.clone());
        cp(&src_lib, &dst_lib, false)?;

        shell_env_cap(
            &[],
            &build_paths.out_dir,
            aapt_path(sdk_dir, urls).to_str().unwrap(),
            &[
                "add",
                (build_paths.dst_unaligned_apk.to_str().unwrap()),
                &binary_path,
            ],
        )?;

        // Scan libmakepad.so for NEEDED shared library dependencies and bundle
        // any that come from the NDK sysroot (e.g. libc++_shared.so).
        bundle_ndk_shared_deps(
            sdk_dir,
            host_os,
            urls,
            android_target,
            &dst_lib,
            abi,
            build_paths,
        )?;
        bundle_local_shared_deps(
            sdk_dir,
            host_os,
            urls,
            android_target,
            &src_lib,
            abi,
            build_paths,
            &current_build_dir,
        )?;
    }
    // for the quest variant add the precompiled openXR loader
    if let AndroidVariant::Quest = variant {
        let cargo_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        for (binary_path, src_lib) in [
            (
                "lib/arm64-v8a/libopenxr_loader.so",
                "quest/libopenxr_loader.so",
            ),
            //("lib/arm64-v8a/libktx.so", "tools/cargo_makepad/quest/libktx.so"),
            //("lib/arm64-v8a/libktx_read.so", "tools/cargo_makepad/quest/libktx_read.so"),
            //("lib/arm64-v8a/libobjUtil.a", "tools/cargo_makepad/quest/libobjUtil.a"),
        ] {
            //let binary_path = format!("lib/arm64-v8a/libopenxr_loader.so");
            let src_lib = cargo_manifest_dir.join(src_lib);
            let dst_lib = build_paths.out_dir.join(binary_path);
            cp(&src_lib, &dst_lib, false)?;
            shell_env_cap(
                &[],
                &build_paths.out_dir,
                aapt_path(sdk_dir, urls).to_str().unwrap(),
                &[
                    "add",
                    (build_paths.dst_unaligned_apk.to_str().unwrap()),
                    &binary_path,
                ],
            )?;
        }
    }

    Ok(build_dir.unwrap())
}

pub(super) fn build_zipaligned_apk(
    sdk_dir: &Path,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    shell_env_cap(
        &[],
        &build_paths.out_dir,
        zipalign_path(sdk_dir, urls).to_str().unwrap(),
        &[
            "-v",
            "-f",
            "4",
            (build_paths.dst_unaligned_apk.to_str().unwrap()),
            (build_paths.dst_apk.to_str().unwrap()),
        ],
    )?;

    Ok(())
}

pub(super) fn sign_apk(
    sdk_dir: &Path,
    host_os: HostOs,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    let cargo_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let java_home = resolve_java_home(sdk_dir, host_os);

    shell_env_cap(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        java_tool_path(&java_home, "java").to_str().unwrap(),
        &[
            "-jar",
            (apksigner_jar_path(sdk_dir, urls).to_str().unwrap()),
            "sign",
            "-v",
            "-ks",
            (cargo_manifest_dir.join("debug.keystore").to_str().unwrap()),
            "--ks-key-alias",
            "androiddebugkey",
            "--ks-pass",
            "pass:android",
            (build_paths.dst_apk.to_str().unwrap()),
        ],
    )?;

    Ok(())
}
