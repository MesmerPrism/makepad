use super::super::sdk::AndroidSDKUrls;
use super::{
    toolchain::{
        clang_tool_name, resolve_android_platform, resolve_build_tools_version,
        resolve_compiler_api_level, resolve_java_home, resolve_ndk_prebuilt_root,
        resolve_platform_api,
    },
    wrapper_manifest::{
        generate_android_wrapper_manifest, normalize_toml_path, strip_generated_wrapper_args,
    },
};
use crate::android::{AndroidTarget, AndroidVariant, HostOs};
use crate::makepad_shell::shell_env;
use std::path::{Path, PathBuf};

pub(super) fn rust_build(
    sdk_dir: &Path,
    host_os: HostOs,
    build_crate: &str,
    args: &[String],
    android_targets: &[AndroidTarget],
    variant: &AndroidVariant,
    urls: &AndroidSDKUrls,
    prefer_dynamic: bool,
) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    let target_root = cargo_target_root(&cwd);
    let target_dir = cargo_target_dir(&cwd);
    let target_dir_str = target_dir.to_string_lossy().to_string();
    let wrapper_manifest = generate_android_wrapper_manifest(build_crate, &target_root)?;
    let cargo_cwd = wrapper_manifest
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.clone());
    let cargo_args = if let Some(wrapper_manifest) = &wrapper_manifest {
        let mut cargo_args = vec![format!(
            "--manifest-path={}",
            normalize_toml_path(wrapper_manifest)
        )];
        cargo_args.extend(strip_generated_wrapper_args(args, build_crate));
        cargo_args
    } else {
        args.to_vec()
    };
    let (_ndk_version, ndk_prebuilt_root) =
        resolve_ndk_prebuilt_root(sdk_dir, host_os, urls.ndk_version_full)?;
    // Derive ndk_root from ndk_prebuilt_root by going up through
    // `toolchains/llvm/prebuilt/<host>/` (4 levels).
    let ndk_root = ndk_prebuilt_root
        .parent()
        .unwrap() // prebuilt/
        .parent()
        .unwrap() // llvm/
        .parent()
        .unwrap() // toolchains/
        .parent()
        .unwrap() // ndk root
        .to_path_buf();
    for android_target in android_targets {
        let compiler_api =
            resolve_compiler_api_level(host_os, urls, &ndk_prebuilt_root, android_target)?;

        let bin_name = |bin_filename: &str, windows_extension: &str| match host_os {
            HostOs::WindowsX64 => format!("{bin_filename}.{windows_extension}"),
            HostOs::MacosX64 | HostOs::MacosAarch64 | HostOs::LinuxX64 => bin_filename.to_string(),
            _ => panic!(),
        };
        let full_clang_path = ndk_prebuilt_root.join("bin").join(clang_tool_name(
            android_target,
            compiler_api,
            host_os,
            false,
        ));
        let full_clangpp_path = ndk_prebuilt_root.join("bin").join(clang_tool_name(
            android_target,
            compiler_api,
            host_os,
            true,
        ));
        let full_llvm_ar_path = ndk_prebuilt_root
            .join("bin")
            .join(bin_name("llvm-ar", "exe"));
        let full_llvm_ranlib_path = ndk_prebuilt_root
            .join("bin")
            .join(bin_name("llvm-ranlib", "exe"));

        let toolchain = android_target.toolchain();
        let target_opt = format!("--target={toolchain}");
        let target_dir_arg = format!("--target-dir={target_dir_str}");

        let base_args = &[
            "run",
            "stable",
            "cargo",
            "rustc",
            "--lib",
            "--crate-type=cdylib",
            &target_opt,
            &target_dir_arg,
        ];
        let mut args_out = Vec::new();
        args_out.extend_from_slice(base_args);
        for arg in &cargo_args {
            args_out.push(arg);
        }

        let target_arch_str = android_target.to_str();
        let cfg_flag = format!("--cfg android_target=\"{}\"", target_arch_str);
        let rustflags = compose_android_rustflags(
            std::env::var("RUSTFLAGS").ok().as_deref(),
            &cfg_flag,
            prefer_dynamic,
        );

        let makepad_env = if let AndroidVariant::Quest = variant {
            Some(match std::env::var("MAKEPAD") {
                Ok(makepad_env) if !makepad_env.is_empty() => format!("{makepad_env}+quest"),
                _ => "quest".to_string(),
            })
        } else {
            std::env::var("MAKEPAD")
                .ok()
                .filter(|value| !value.is_empty())
        };

        let android_sdk_version = resolve_platform_api(sdk_dir, urls).to_string();
        let android_api_level = compiler_api.to_string();
        let java_home = resolve_java_home(sdk_dir, host_os)
            .to_string_lossy()
            .to_string();
        let build_tools_version = resolve_build_tools_version(sdk_dir, urls);
        let android_platform = resolve_android_platform(sdk_dir, urls);
        let mut env: Vec<(String, String)> = vec![
            (
                android_target.linker_env_var().to_string(),
                full_clang_path.to_string_lossy().to_string(),
            ),
            (
                "ANDROID_HOME".to_string(),
                sdk_dir.to_string_lossy().to_string(),
            ),
            (
                "ANDROID_SDK_ROOT".to_string(),
                sdk_dir.to_string_lossy().to_string(),
            ),
            (
                "ANDROID_BUILD_TOOLS_VERSION".to_string(),
                build_tools_version,
            ),
            ("ANDROID_PLATFORM".to_string(), android_platform),
            (
                "ANDROID_SDK_VERSION".to_string(),
                android_sdk_version.clone(),
            ),
            ("ANDROID_API_LEVEL".to_string(), android_api_level),
            (
                "ANDROID_SDK_EXTENSION".to_string(),
                urls.sdk_extension.to_string(),
            ),
            ("JAVA_HOME".to_string(), java_home),
            (
                "ANDROID_NDK_ROOT".to_string(),
                ndk_root.to_string_lossy().to_string(),
            ),
            (
                format!("CC_{toolchain}"),
                full_clang_path.to_string_lossy().to_string(),
            ),
            (
                format!("CXX_{toolchain}"),
                full_clangpp_path.to_string_lossy().to_string(),
            ),
            (
                format!("AR_{toolchain}"),
                full_llvm_ar_path.to_string_lossy().to_string(),
            ),
            (
                format!("RANLIB_{toolchain}"),
                full_llvm_ranlib_path.to_string_lossy().to_string(),
            ),
            // The aws-lc-sys crate requires the cmake builder (not the cc builder)
            // for Android cross-compilation targets. With ANDROID_NDK_ROOT set
            // and the full NDK installed (via --full-ndk), cmake's built-in
            // Android module handles the cross-compilation setup automatically.
            ("AWS_LC_SYS_CMAKE_BUILDER".to_string(), "1".to_string()),
            ("RUSTFLAGS".to_string(), rustflags),
        ];
        if let Some(makepad_env) = makepad_env {
            env.push(("MAKEPAD".to_string(), makepad_env));
        }
        let env_refs = env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();

        shell_env(&env_refs, &cargo_cwd, "rustup", &args_out)?;
    }

    Ok(())
}

fn compose_android_rustflags(
    existing: Option<&str>,
    cfg_flag: &str,
    prefer_dynamic: bool,
) -> String {
    let mut rustflags = existing.unwrap_or_default().trim().to_string();
    if prefer_dynamic {
        let has_prefer_dynamic = rustflags
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["-C", "prefer-dynamic"])
            || rustflags
                .split_whitespace()
                .any(|token| token == "-Cprefer-dynamic");

        if !has_prefer_dynamic {
            if !rustflags.is_empty() {
                rustflags.push(' ');
            }
            rustflags.push_str("-C prefer-dynamic");
        }
    }
    if !cfg_flag.trim().is_empty() {
        if !rustflags.is_empty() {
            rustflags.push(' ');
        }
        rustflags.push_str(cfg_flag.trim());
    }
    rustflags
}

/// Resolve the cargo target directory for android builds.
/// Defaults to `target/android` to avoid invalidating desktop build caches.
fn cargo_target_root(cwd: &Path) -> PathBuf {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let target_dir = PathBuf::from(target_dir);
        if target_dir.is_absolute() {
            target_dir
        } else {
            cwd.join(target_dir)
        }
    } else {
        cwd.join("target")
    }
}

pub(super) fn cargo_target_dir(cwd: &Path) -> PathBuf {
    if std::env::var_os("CARGO_TARGET_DIR").is_some() {
        cargo_target_root(cwd)
    } else {
        cargo_target_root(cwd).join("android")
    }
}

#[cfg(test)]
mod tests {
    use super::compose_android_rustflags;

    #[test]
    fn compose_android_rustflags_adds_prefer_dynamic() {
        assert_eq!(
            compose_android_rustflags(None, "--cfg android_target=\"aarch64\"", true),
            "-C prefer-dynamic --cfg android_target=\"aarch64\""
        );
    }

    #[test]
    fn compose_android_rustflags_preserves_existing_flags() {
        assert_eq!(
            compose_android_rustflags(
                Some("-C debuginfo=1"),
                "--cfg android_target=\"aarch64\"",
                true,
            ),
            "-C debuginfo=1 -C prefer-dynamic --cfg android_target=\"aarch64\""
        );
    }

    #[test]
    fn compose_android_rustflags_does_not_duplicate_prefer_dynamic() {
        assert_eq!(
            compose_android_rustflags(
                Some("-C prefer-dynamic -C debuginfo=1"),
                "--cfg android_target=\"aarch64\"",
                true,
            ),
            "-C prefer-dynamic -C debuginfo=1 --cfg android_target=\"aarch64\""
        );
    }

    #[test]
    fn compose_android_rustflags_can_skip_prefer_dynamic() {
        assert_eq!(
            compose_android_rustflags(
                Some("-C debuginfo=1"),
                "--cfg android_target=\"aarch64\"",
                false
            ),
            "-C debuginfo=1 --cfg android_target=\"aarch64\""
        );
    }
}
