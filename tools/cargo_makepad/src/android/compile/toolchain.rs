use super::super::sdk::{AndroidSDKUrls, BUILD_TOOLS_DIR, BUNDLETOOL_JAR_REL, PLATFORMS_DIR};
use crate::android::{AndroidTarget, HostOs};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn aapt_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(BUILD_TOOLS_DIR)
        .join(resolve_build_tools_version(sdk_dir, urls))
        .join(host_executable_name("aapt"))
}

pub(super) fn aapt2_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(BUILD_TOOLS_DIR)
        .join(resolve_build_tools_version(sdk_dir, urls))
        .join(host_executable_name("aapt2"))
}

pub(super) fn bundletool_jar_path(sdk_dir: &Path) -> PathBuf {
    sdk_dir.join(BUNDLETOOL_JAR_REL)
}

pub(super) fn keytool_path(sdk_dir: &Path, host_os: HostOs) -> PathBuf {
    let java_home = resolve_java_home(sdk_dir, host_os);
    java_tool_path(&java_home, "keytool")
}

pub(super) fn d8_jar_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(BUILD_TOOLS_DIR)
        .join(resolve_build_tools_version(sdk_dir, urls))
        .join("lib/d8.jar")
}

pub(super) fn apksigner_jar_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(BUILD_TOOLS_DIR)
        .join(resolve_build_tools_version(sdk_dir, urls))
        .join("lib/apksigner.jar")
}

pub(super) fn zipalign_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(BUILD_TOOLS_DIR)
        .join(resolve_build_tools_version(sdk_dir, urls))
        .join(host_executable_name("zipalign"))
}

pub(super) fn android_jar_path(sdk_dir: &Path, urls: &AndroidSDKUrls) -> PathBuf {
    sdk_dir
        .join(PLATFORMS_DIR)
        .join(resolve_android_platform(sdk_dir, urls))
        .join("android.jar")
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedAndroidSdk {
    pub(super) platform: String,
    pub(super) platform_api: usize,
    pub(super) build_tools_version: String,
    pub(super) compiler_api: usize,
    pub(super) java_home: PathBuf,
    pub(super) ndk_prebuilt_root: PathBuf,
}

fn host_executable_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub(super) fn java_tool_path(java_home: &Path, name: &str) -> PathBuf {
    java_home.join("bin").join(host_executable_name(name))
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn dotted_version_sort_key(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn android_platform_api(platform: &str) -> Option<usize> {
    platform.strip_prefix("android-").and_then(|tail| {
        tail.chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .parse::<usize>()
            .ok()
    })
}

fn installed_android_platforms(sdk_dir: &Path) -> Vec<(usize, String)> {
    let platforms_dir = sdk_dir.join(PLATFORMS_DIR);
    let Ok(entries) = fs::read_dir(platforms_dir) else {
        return Vec::new();
    };
    let mut platforms = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("android.jar").is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            android_platform_api(&name).map(|api| (api, name))
        })
        .collect::<Vec<_>>();
    platforms.sort_by(|(api_a, name_a), (api_b, name_b)| {
        api_b.cmp(api_a).then_with(|| name_b.cmp(name_a))
    });
    platforms
}

fn installed_build_tools_versions(sdk_dir: &Path) -> Vec<String> {
    let build_tools_dir = sdk_dir.join(BUILD_TOOLS_DIR);
    let Ok(entries) = fs::read_dir(build_tools_dir) else {
        return Vec::new();
    };
    let mut versions = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    versions.sort_by(|a, b| dotted_version_sort_key(b).cmp(&dotted_version_sort_key(a)));
    versions
}

pub(super) fn resolve_android_platform(sdk_dir: &Path, urls: &AndroidSDKUrls) -> String {
    env_non_empty("ANDROID_PLATFORM")
        .or_else(|| {
            env_non_empty("ANDROID_SDK_VERSION").map(|version| format!("android-{version}"))
        })
        .or_else(|| {
            installed_android_platforms(sdk_dir)
                .into_iter()
                .next()
                .map(|(_, name)| name)
        })
        .unwrap_or_else(|| urls.platform.to_string())
}

pub(super) fn resolve_platform_api(sdk_dir: &Path, urls: &AndroidSDKUrls) -> usize {
    android_platform_api(&resolve_android_platform(sdk_dir, urls)).unwrap_or(urls.sdk_version)
}

pub(super) fn resolve_build_tools_version(sdk_dir: &Path, urls: &AndroidSDKUrls) -> String {
    env_non_empty("ANDROID_BUILD_TOOLS_VERSION")
        .or_else(|| installed_build_tools_versions(sdk_dir).into_iter().next())
        .unwrap_or_else(|| urls.build_tools_version.to_string())
}

pub(super) fn resolve_java_home(sdk_dir: &Path, _host_os: HostOs) -> PathBuf {
    if let Some(java_home) = env_non_empty("JAVA_HOME").map(PathBuf::from) {
        if java_tool_path(&java_home, "java").is_file()
            && java_tool_path(&java_home, "javac").is_file()
        {
            return java_home;
        }
    }
    sdk_dir.join("openjdk")
}

fn available_clang_api_levels(
    ndk_prebuilt_root: &Path,
    host_os: HostOs,
    android_target: &AndroidTarget,
) -> Vec<usize> {
    let bin_dir = ndk_prebuilt_root.join("bin");
    let Ok(entries) = fs::read_dir(bin_dir) else {
        return Vec::new();
    };
    let prefix = android_target.clang();
    let suffix = match host_os {
        HostOs::WindowsX64 => "-clang.cmd",
        HostOs::MacosX64 | HostOs::MacosAarch64 | HostOs::LinuxX64 => "-clang",
        _ => "-clang",
    };
    let mut levels = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter_map(|name| {
            let tail = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            tail.parse::<usize>().ok()
        })
        .collect::<Vec<_>>();
    levels.sort_by(|a, b| b.cmp(a));
    levels.dedup();
    levels
}

pub(super) fn resolve_compiler_api_level(
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    ndk_prebuilt_root: &Path,
    android_target: &AndroidTarget,
) -> Result<usize, String> {
    let levels = available_clang_api_levels(ndk_prebuilt_root, host_os, android_target);
    if levels.is_empty() {
        return Err(format!(
            "No API-level clang wrappers found in {:?} for {}",
            ndk_prebuilt_root.join("bin"),
            android_target.clang()
        ));
    }
    if let Some(api) = env_non_empty("ANDROID_API_LEVEL")
        .or_else(|| env_non_empty("ANDROID_SDK_VERSION"))
        .and_then(|value| value.parse::<usize>().ok())
    {
        if levels.contains(&api) {
            return Ok(api);
        }
        return Err(format!(
            "Requested Android API level {api} has no {}{api}-clang wrapper in {:?}; available API levels: {:?}",
            android_target.clang(),
            ndk_prebuilt_root.join("bin"),
            levels
        ));
    }
    if levels.contains(&urls.sdk_version) {
        return Ok(urls.sdk_version);
    }
    Err(format!(
        "Configured Android min SDK {} has no {}{}-clang wrapper in {:?}; available API levels: {:?}",
        urls.sdk_version,
        android_target.clang(),
        urls.sdk_version,
        ndk_prebuilt_root.join("bin"),
        levels
    ))
}

pub(super) fn preflight_android_sdk(
    sdk_dir: &Path,
    host_os: HostOs,
    urls: &AndroidSDKUrls,
    android_targets: &[AndroidTarget],
) -> Result<ResolvedAndroidSdk, String> {
    let platform = resolve_android_platform(sdk_dir, urls);
    let platform_api = android_platform_api(&platform).ok_or_else(|| {
        format!("Android platform name does not contain an API level: {platform}")
    })?;
    let android_jar = sdk_dir
        .join(PLATFORMS_DIR)
        .join(&platform)
        .join("android.jar");
    if !android_jar.is_file() {
        return Err(format!(
            "Android platform {platform} was selected, but android.jar is missing at {:?}",
            android_jar
        ));
    }

    let build_tools_version = resolve_build_tools_version(sdk_dir, urls);
    let build_tools_dir = sdk_dir.join(BUILD_TOOLS_DIR).join(&build_tools_version);
    for tool in ["aapt", "zipalign"] {
        let path = build_tools_dir.join(host_executable_name(tool));
        if !path.is_file() {
            return Err(format!(
                "Android build-tools {build_tools_version} was selected, but {tool} is missing at {:?}",
                path
            ));
        }
    }
    for jar in ["lib/d8.jar", "lib/apksigner.jar"] {
        let path = build_tools_dir.join(jar);
        if !path.is_file() {
            return Err(format!(
                "Android build-tools {build_tools_version} was selected, but {jar} is missing at {:?}",
                path
            ));
        }
    }

    let java_home = resolve_java_home(sdk_dir, host_os);
    for tool in ["java", "javac"] {
        let path = java_tool_path(&java_home, tool);
        if !path.is_file() {
            return Err(format!(
                "Java tool {tool} not found at {:?}. Set JAVA_HOME or install Makepad-managed openjdk under the selected SDK.",
                path
            ));
        }
    }

    let (_ndk_version, ndk_prebuilt_root) =
        resolve_ndk_prebuilt_root(sdk_dir, host_os, urls.ndk_version_full)?;
    let Some(first_target) = android_targets.first() else {
        return Err("No Android targets selected".to_string());
    };
    let compiler_api = resolve_compiler_api_level(host_os, urls, &ndk_prebuilt_root, first_target)?;
    for target in android_targets {
        let path = ndk_prebuilt_root.join("bin").join(clang_tool_name(
            target,
            compiler_api,
            host_os,
            false,
        ));
        if !path.is_file() {
            return Err(format!(
                "Android compiler for target {} API {} not found at {:?}",
                target.toolchain(),
                compiler_api,
                path
            ));
        }
    }

    println!(
        "Resolved Android SDK: platform={} platformApi={} buildTools={} compilerApi={} javaHome={} ndkPrebuilt={}",
        platform,
        platform_api,
        build_tools_version,
        compiler_api,
        java_home.display(),
        ndk_prebuilt_root.display()
    );

    Ok(ResolvedAndroidSdk {
        platform,
        platform_api,
        build_tools_version,
        compiler_api,
        java_home,
        ndk_prebuilt_root,
    })
}

pub(super) fn clang_tool_name(
    android_target: &AndroidTarget,
    api_level: usize,
    host_os: HostOs,
    cxx: bool,
) -> String {
    let suffix = if cxx { "clang++" } else { "clang" };
    let base = format!("{}{}-{suffix}", android_target.clang(), api_level);
    match host_os {
        HostOs::WindowsX64 => format!("{base}.cmd"),
        HostOs::MacosX64 | HostOs::MacosAarch64 | HostOs::LinuxX64 => base,
        _ => base,
    }
}

fn ndk_prebuilt_dir_candidates(host_os: HostOs) -> &'static [&'static str] {
    match host_os {
        HostOs::MacosX64 => &["darwin-x86_64"],
        // On Apple Silicon, older NDKs only ship darwin-x86_64 prebuilts.
        HostOs::MacosAarch64 => &["darwin-aarch64", "darwin-x86_64"],
        HostOs::WindowsX64 => &["windows-x86_64"],
        HostOs::LinuxX64 => &["linux-x86_64"],
        _ => panic!("Unsupported host OS"),
    }
}

fn ndk_version_sort_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

pub(super) fn resolve_ndk_prebuilt_root(
    sdk_dir: &Path,
    host_os: HostOs,
    preferred_version: &str,
) -> Result<(String, PathBuf), String> {
    let prebuilt_candidates = ndk_prebuilt_dir_candidates(host_os);
    let ndk_root = sdk_dir.join("ndk");
    if !ndk_root.is_dir() {
        return Err(format!(
            "Android NDK directory not found: {:?}. Run `cargo makepad android install-toolchain` or copy an NDK into `ndk/<version>`.",
            ndk_root
        ));
    }

    let mut versions = Vec::new();
    for entry in
        std::fs::read_dir(&ndk_root).map_err(|e| format!("failed to read {:?}: {e}", ndk_root))?
    {
        let entry =
            entry.map_err(|e| format!("failed to read NDK entry in {:?}: {e}", ndk_root))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(version) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        for prebuilt in prebuilt_candidates {
            let prebuilt_root = path.join("toolchains/llvm/prebuilt").join(prebuilt);
            if prebuilt_root.is_dir() {
                versions.push((version.to_string(), prebuilt_root));
                break;
            }
        }
    }

    if versions.is_empty() {
        return Err(format!(
            "No compatible NDK toolchain found under {:?} for host prebuilts {:?}",
            ndk_root, prebuilt_candidates
        ));
    }

    if let Some((version, root)) = versions
        .iter()
        .find(|(version, _)| version == preferred_version)
    {
        return Ok((version.clone(), root.clone()));
    }

    versions.sort_by(|(a, _), (b, _)| ndk_version_sort_key(b).cmp(&ndk_version_sort_key(a)));
    Ok(versions.remove(0))
}

pub(super) fn ndk_bin_path(ndk_prebuilt_root: &Path, host_os: HostOs, name: &str) -> PathBuf {
    let file_name = match host_os {
        HostOs::WindowsX64 => format!("{name}.exe"),
        _ => name.to_string(),
    };
    ndk_prebuilt_root.join("bin").join(file_name)
}
