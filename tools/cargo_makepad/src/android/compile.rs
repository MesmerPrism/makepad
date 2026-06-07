use super::sdk::AndroidSDKUrls;
use crate::android::{AndroidConfig, AndroidTarget, AndroidVariant, HostOs, ManifestArgs};
use crate::makepad_shell::*;
use crate::utils::*;
use makepad_zip_file::*;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    fs::File,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod keystore;
mod shared_libs;
mod toolchain;
mod wrapper_manifest;
#[allow(unused_imports)]
pub use keystore::{
    keystore_create, keystore_sidecar_path, read_keystore_sidecar, KeystoreCreateOpts,
    KeystoreSidecar,
};
use shared_libs::{bundle_local_shared_deps, bundle_ndk_shared_deps, stage_aab_native_libs};
use toolchain::{
    aapt2_path, aapt_path, android_jar_path, apksigner_jar_path, bundletool_jar_path,
    clang_tool_name, d8_jar_path, java_tool_path, preflight_android_sdk, resolve_android_platform,
    resolve_build_tools_version, resolve_compiler_api_level, resolve_java_home,
    resolve_ndk_prebuilt_root, resolve_platform_api, zipalign_path,
};
use wrapper_manifest::{
    generate_android_wrapper_manifest, normalize_toml_path, strip_generated_wrapper_args,
};

#[derive(Debug)]
struct BuildPaths {
    tmp_dir: PathBuf,
    out_dir: PathBuf,
    java_out_dir: PathBuf,
    res_dir: PathBuf,
    manifest_file: PathBuf,
    java_file: PathBuf,
    xr_file: PathBuf,
    dst_unaligned_apk: PathBuf,
    dst_apk: PathBuf,
}

pub struct BuildResult {
    dst_apk: PathBuf,
    java_url: String,
}

const SMALL_FONT_REPLACEMENTS: [(&str, &str); 5] = [
    ("GoNotoKurrent-Bold.ttf", "IBMPlexSans-SemiBold.ttf"),
    ("GoNotoKurrent-Regular.ttf", "IBMPlexSans-Text.ttf"),
    ("LXGWWenKaiBold.ttf", "IBMPlexSans-Text.ttf"),
    ("LXGWWenKaiRegular.ttf", "IBMPlexSans-Text.ttf"),
    ("NotoColorEmoji.ttf", "IBMPlexSans-Text.ttf"),
];

fn main_java(url: &str) -> String {
    format!(
        r#"
        package {url};
        import dev.makepad.android.MakepadActivity;
        public class MakepadApp extends MakepadActivity{{
            public boolean isXrActivity(){{
                return false;
            }}
            public void switchActivity(){{
                switchActivityClass(MakepadAppXr.class);
            }}
            public void startXrActivity(){{
                switchActivityClass(MakepadAppXr.class);
            }}
            public void stopXrActivity(){{
            }}
        }}
    "#
    )
}

fn xr_java(url: &str) -> String {
    format!(
        r#"
        package {url};
        import dev.makepad.android.MakepadActivity;
        public class MakepadAppXr extends MakepadActivity{{
            public boolean isXrActivity(){{
                return true;
            }}
            public void switchActivity(){{
                switchActivityClass(MakepadApp.class);
            }}
            public void startXrActivity(){{
            }}
            public void stopXrActivity(){{
                switchActivityClass(MakepadApp.class);
            }}
        }}
    "#
    )
}

fn android_phase_timings_enabled() -> bool {
    std::env::var("MAKEPAD_ANDROID_TIMINGS")
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn timed_android_phase<T, F>(phase: &str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    if !android_phase_timings_enabled() {
        return f();
    }

    let started_at = Instant::now();
    let result = f();
    let status = if result.is_ok() { "ok" } else { "failed" };
    eprintln!(
        "MAKEPAD_ANDROID_TIMING phase={} status={} duration_ms={}",
        phase,
        status,
        started_at.elapsed().as_millis()
    );
    result
}

fn rust_build(
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

fn cargo_target_dir(cwd: &Path) -> PathBuf {
    if std::env::var_os("CARGO_TARGET_DIR").is_some() {
        cargo_target_root(cwd)
    } else {
        cargo_target_root(cwd).join("android")
    }
}

struct ResolvedPackagingInputs {
    java_url: String,
    app_label: String,
    version_code: u32,
    version_name: String,
    min_sdk_version_override: Option<usize>,
}

fn resolve_packaging_inputs(
    build_crate: &str,
    binary_name: &str,
    package_name_flag: Option<String>,
    app_label_flag: Option<String>,
    version_code_flag: Option<VersionCodeStrategy>,
    version_name_flag: Option<String>,
    min_sdk_version_flag: Option<usize>,
    urls: &AndroidSDKUrls,
) -> Result<ResolvedPackagingInputs, String> {
    let underscore_binary_name = binary_name.replace('-', "_");
    let metadata = read_android_package_metadata(build_crate);

    let java_url = package_name_flag
        .or(metadata.identifier.clone())
        .unwrap_or_else(|| format!("dev.makepad.{underscore_binary_name}"));
    let app_label = app_label_flag
        .or(metadata.product_name.clone())
        .unwrap_or_else(|| {
            let mut chars = underscore_binary_name.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        });

    let version_code = version_code_flag
        .or(metadata.version_code)
        .unwrap_or(VersionCodeStrategy::Explicit(1))
        .resolve();
    let version_name = version_name_flag
        .or(metadata.version_name_override.clone())
        .or(metadata.package_version.clone())
        .unwrap_or_else(|| "1.0".to_string());

    let min_sdk_version_override = min_sdk_version_flag.or(metadata.min_sdk_version);
    if let Some(min_sdk_version) = min_sdk_version_override {
        if min_sdk_version < urls.sdk_version {
            return Err(format!(
                "min_sdk_version = {min_sdk_version} is below cargo-makepad's current Android floor of {}",
                urls.sdk_version
            ));
        }
        if min_sdk_version > urls.target_sdk_version {
            return Err(format!(
                "min_sdk_version = {min_sdk_version} cannot exceed targetSdkVersion = {}",
                urls.target_sdk_version
            ));
        }
    }

    Ok(ResolvedPackagingInputs {
        java_url,
        app_label,
        version_code,
        version_name,
        min_sdk_version_override,
    })
}

struct PrepareBuildOpts<'a> {
    build_crate: &'a str,
    java_url: &'a str,
    app_label: &'a str,
    variant: &'a AndroidVariant,
    config: &'a AndroidConfig,
    urls: &'a AndroidSDKUrls,
    version_code: u32,
    version_name: &'a str,
    debuggable: bool,
}

fn substitute_manifest_template(template: &str, args: &ManifestArgs<'_>) -> String {
    let debuggable = if args.debuggable { "true" } else { "false" };
    let screen_orientation = args.screen_orientation.unwrap_or("");
    let resizeable_activity = args
        .resizeable_activity
        .map(|value| if value { "true" } else { "false" })
        .unwrap_or("");
    template
        .replace("{label}", args.label)
        .replace("{class_name}", args.class_name)
        .replace("{package_id}", args.url)
        .replace("{min_sdk_version}", &args.sdk_version.to_string())
        .replace("{target_sdk_version}", &args.target_sdk_version.to_string())
        .replace("{version_code}", &args.version_code.to_string())
        .replace("{version_name}", args.version_name)
        .replace("{debuggable}", debuggable)
        .replace("{screen_orientation}", screen_orientation)
        .replace("{resizeable_activity}", resizeable_activity)
}

fn prepare_build(opts: &PrepareBuildOpts<'_>) -> Result<BuildPaths, String> {
    let cwd = std::env::current_dir().unwrap();
    let target_dir = cargo_target_dir(&cwd);
    let underscore_build_crate = opts.build_crate.replace('-', "_");

    let tmp_dir = target_dir
        .join("makepad-android-apk")
        .join(&underscore_build_crate)
        .join("tmp");
    let out_dir = target_dir
        .join("makepad-android-apk")
        .join(&underscore_build_crate)
        .join("apk");
    let java_out_dir = target_dir
        .join("makepad-android-apk")
        .join(&underscore_build_crate)
        .join("java");
    let res_dir = tmp_dir.join("res");

    let _ = rmdir(&tmp_dir);
    let _ = rmdir(&out_dir);
    mkdir(&tmp_dir)?;
    mkdir(&out_dir)?;
    mkdir(&java_out_dir)?;

    let cargo_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    cp_all(&cargo_manifest_dir.join("src/android/res"), &res_dir, false)?;

    let build_crate_dir = get_crate_dir(opts.build_crate)?;
    let app_android_res = build_crate_dir.join("resources/android/res");
    if app_android_res.is_dir() {
        cp_all(&app_android_res, &res_dir, false)?;
    }

    let android_icon_targets = [
        "mipmap-mdpi",
        "mipmap-hdpi",
        "mipmap-xhdpi",
        "mipmap-xxhdpi",
        "mipmap-xxxhdpi",
    ];
    let has_android_icon = android_icon_targets
        .iter()
        .all(|d| res_dir.join(d).join("ic_launcher.png").is_file());
    if !has_android_icon && !no_icon_requested() {
        eprintln!(
            "warning: missing Android launcher icons under {}. Add mipmap-*/ic_launcher.png files, or pass --no-icon to suppress this check.",
            res_dir.display()
        );
    }

    let manifest_args = ManifestArgs {
        label: opts.app_label,
        class_name: "MakepadApp",
        url: opts.java_url,
        sdk_version: opts.urls.sdk_version,
        target_sdk_version: opts.urls.target_sdk_version,
        has_icon: has_android_icon,
        version_code: opts.version_code,
        version_name: opts.version_name,
        debuggable: opts.debuggable,
        screen_orientation: opts.config.screen_orientation.as_deref(),
        resizeable_activity: opts.config.resizeable_activity,
    };
    let custom_template = build_crate_dir.join("resources/android/AndroidManifest.xml.template");
    let manifest_xml = if custom_template.is_file() {
        let template = fs::read_to_string(&custom_template)
            .map_err(|e| format!("Can't read {:?}: {e}", custom_template))?;
        println!(
            "Using custom AndroidManifest template: {}",
            custom_template.display()
        );
        substitute_manifest_template(&template, &manifest_args)
    } else {
        opts.variant.manifest_xml(&manifest_args)
    };
    let manifest_file = tmp_dir.join("AndroidManifest.xml");
    write_text(&manifest_file, &manifest_xml)?;

    let main_java = main_java(opts.java_url);
    let java_path = opts.java_url.replace('.', "/");
    let java_file = tmp_dir.join(&java_path).join("MakepadApp.java");
    write_text(&java_file, &main_java)?;

    let xr_java = xr_java(opts.java_url);
    let xr_file = tmp_dir.join(&java_path).join("MakepadAppXr.java");
    write_text(&xr_file, &xr_java)?;

    let apk_filename = to_snakecase(opts.app_label);
    let dst_unaligned_apk = out_dir.join(format!("{apk_filename}.unaligned.apk"));
    let dst_apk = out_dir.join(format!("{apk_filename}.apk"));

    let _ = rm(&dst_unaligned_apk);
    let _ = rm(&dst_apk);

    Ok(BuildPaths {
        tmp_dir,
        out_dir,
        java_out_dir,
        res_dir,
        manifest_file,
        java_file,
        xr_file,
        dst_unaligned_apk,
        dst_apk,
    })
}

fn build_r_class(
    sdk_dir: &Path,
    host_os: HostOs,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let java_home = resolve_java_home(sdk_dir, host_os);
    let cwd = std::env::current_dir().unwrap();

    shell_env(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        &aapt_path(sdk_dir, urls).to_str().unwrap(),
        &[
            "package",
            "-f",
            "-m",
            "-I",
            (android_jar_path(sdk_dir, urls).to_str().unwrap()),
            "-S",
            (build_paths.res_dir.to_str().unwrap()),
            "-M",
            (build_paths.manifest_file.to_str().unwrap()),
            "-J",
            (build_paths.tmp_dir.to_str().unwrap()),
            "--custom-package",
            "dev.makepad.android",
            (build_paths.out_dir.to_str().unwrap()),
        ],
    )?;

    Ok(())
}

fn compile_java(
    sdk_dir: &Path,
    host_os: HostOs,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let makepad_package_path = "dev/makepad/android";
    let cargo_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let java_home = resolve_java_home(sdk_dir, host_os);
    let cwd = std::env::current_dir().unwrap();
    let javac_stamp = build_paths.java_out_dir.join("javac.inputs");

    let r_class_path = build_paths
        .tmp_dir
        .join(makepad_package_path)
        .join("R.java");
    let makepad_java_classes_dir = &cargo_manifest_dir
        .join("src/android/java/")
        .join(makepad_package_path);
    let java_sources = vec![
        r_class_path.clone(),
        makepad_java_classes_dir.join("MakepadNative.java"),
        makepad_java_classes_dir.join("MakepadActivity.java"),
        makepad_java_classes_dir.join("MakepadInputConnection.java"),
        makepad_java_classes_dir.join("MakepadNetwork.java"),
        makepad_java_classes_dir.join("MakepadSocketStream.java"),
        makepad_java_classes_dir.join("MakepadWebSocket.java"),
        makepad_java_classes_dir.join("MakepadWebSocketReader.java"),
        makepad_java_classes_dir.join("MediaProjectionStreamService.java"),
        makepad_java_classes_dir.join("ByteArrayMediaDataSource.java"),
        makepad_java_classes_dir.join("VideoPlayer.java"),
        makepad_java_classes_dir.join("BrokerH264VideoPlayer.java"),
        makepad_java_classes_dir.join("VideoPlayerRunnable.java"),
        makepad_java_classes_dir.join("H264Encoder.java"),
        build_paths.java_file.clone(),
        build_paths.xr_file.clone(),
    ];

    let mut hasher = DefaultHasher::new();
    for source in &java_sources {
        source.to_string_lossy().hash(&mut hasher);
        fs::read(source)
            .map_err(|e| format!("failed to read Java source {:?}: {e}", source))?
            .hash(&mut hasher);
    }
    let java_inputs_hash = format!("{:016x}", hasher.finish());

    let app_class_dir = build_paths
        .java_file
        .parent()
        .and_then(|path| path.strip_prefix(&build_paths.tmp_dir).ok())
        .ok_or_else(|| {
            format!(
                "failed to resolve Java output package for {:?}",
                build_paths.java_file
            )
        })?;
    let expected_outputs = [
        build_paths.java_out_dir.join("dev/makepad/android/R.class"),
        build_paths
            .java_out_dir
            .join("dev/makepad/android/MakepadActivity.class"),
        build_paths
            .java_out_dir
            .join(app_class_dir)
            .join("MakepadApp.class"),
        build_paths
            .java_out_dir
            .join(app_class_dir)
            .join("MakepadAppXr.class"),
    ];

    if fs::read_to_string(&javac_stamp)
        .map(|cached| cached.trim() == java_inputs_hash)
        .unwrap_or(false)
        && expected_outputs.iter().all(|path| path.is_file())
    {
        return Ok(());
    }

    let android_jar = android_jar_path(sdk_dir, urls);
    let _ = rmdir(&build_paths.java_out_dir);
    mkdir(&build_paths.java_out_dir)?;
    let mut javac_args = vec![
        "-source",
        "1.8",
        "-target",
        "1.8",
        "-Xlint:-options",
        "-classpath",
        android_jar.to_str().unwrap(),
        "-Xlint:deprecation",
        "-d",
        build_paths.java_out_dir.to_str().unwrap(),
    ];
    for source in &java_sources {
        javac_args.push(source.to_str().unwrap());
    }

    shell_env(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        java_tool_path(&java_home, "javac").to_str().unwrap(),
        &javac_args,
    )?;
    write_text(&javac_stamp, &java_inputs_hash)?;

    Ok(())
}

fn build_dex(
    sdk_dir: &Path,
    host_os: HostOs,
    build_paths: &BuildPaths,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let java_home = resolve_java_home(sdk_dir, host_os);
    let cwd = std::env::current_dir().unwrap();

    let mut class_files: Vec<PathBuf> = ls(&build_paths.java_out_dir)?
        .into_iter()
        .filter(|rel| rel.extension().and_then(|ext| ext.to_str()) == Some("class"))
        .map(|rel| build_paths.java_out_dir.join(rel))
        .collect();

    class_files.sort();

    if class_files.is_empty() {
        return Err(format!(
            "No compiled Java class files found in {:?}",
            build_paths.java_out_dir
        ));
    }

    let _ = rmdir(&build_paths.out_dir);
    mkdir(&build_paths.out_dir)?;

    let d8_jar = d8_jar_path(sdk_dir, urls);
    let android_jar = android_jar_path(sdk_dir, urls);

    let mut args: Vec<&str> = vec![
        "-cp",
        d8_jar.to_str().unwrap(),
        "com.android.tools.r8.D8",
        "--classpath",
        android_jar.to_str().unwrap(),
        "--output",
        build_paths.out_dir.to_str().unwrap(),
    ];

    for class_file in &class_files {
        args.push(class_file.to_str().unwrap());
    }

    shell_env_cap(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        java_tool_path(&java_home, "java").to_str().unwrap(),
        &args,
    )?;

    Ok(())
}

fn build_unaligned_apk(
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

fn add_rust_library(
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

fn add_resources(
    sdk_dir: &Path,
    build_crate: &str,
    build_paths: &BuildPaths,
    build_dir: &Path,
    android_targets: &[AndroidTarget],
    variant: &AndroidVariant,
    config: &AndroidConfig,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let mut assets_to_add: Vec<String> = Vec::new();

    let build_crate_dir = get_crate_dir(build_crate)?;
    add_assets_dir_to_apk(
        &build_paths.out_dir,
        &mut assets_to_add,
        build_crate,
        &build_crate_dir.join("resources"),
        "resources",
        config,
    )?;
    add_font_assets_dir_to_apk(
        &build_paths.out_dir,
        &mut assets_to_add,
        build_crate,
        &build_crate_dir.join("fonts"),
        &build_crate_dir.join("resources"),
        config,
    )?;

    let deps = get_crate_dep_dirs(build_crate, &build_dir, &android_targets[0].toolchain());
    for (name, dep_dir) in deps.iter() {
        add_assets_dir_to_apk(
            &build_paths.out_dir,
            &mut assets_to_add,
            name,
            &dep_dir.join("resources"),
            "resources",
            config,
        )?;
        add_font_assets_dir_to_apk(
            &build_paths.out_dir,
            &mut assets_to_add,
            name,
            &dep_dir.join("fonts"),
            &dep_dir.join("resources"),
            config,
        )?;
    }
    // FIX THIS PROPER
    // On quest remove most of the widget resourcse
    if let AndroidVariant::Quest = variant {
        let dst_dir = build_paths
            .out_dir
            .join(format!("assets/makepad/makepad_widgets/resources"));
        let remove = [
            "fa-solid-900.ttf",
            //"LXGWWenKaiBold.ttf",
            "LiberationMono-Regular.ttf",
            //"GoNotoKurrent-Bold.ttf",
            // "NotoColorEmoji.ttf",
            //"IBMPlexSans-SemiBold.ttf",
            "NotoSans-Regular.ttf",
        ];
        for remove in remove {
            assets_to_add.retain(|v| !v.contains(remove));
            let remove_path = dst_dir.join(remove);
            if remove_path.is_file() {
                rm(&remove_path)?;
            }
        }
    }

    if !assets_to_add.is_empty() {
        let mut aapt_args = vec!["add", build_paths.dst_unaligned_apk.to_str().unwrap()];
        for asset in &assets_to_add {
            aapt_args.push(asset);
        }

        shell_env_cap(
            &[],
            &build_paths.out_dir,
            aapt_path(sdk_dir, urls).to_str().unwrap(),
            &aapt_args,
        )?;
    }

    Ok(())
}

fn add_assets_dir_to_apk(
    out_dir: &Path,
    assets_to_add: &mut Vec<String>,
    crate_name: &str,
    source_dir: &Path,
    asset_subdir: &str,
    config: &AndroidConfig,
) -> Result<(), String> {
    if !source_dir.is_dir() {
        return Ok(());
    }

    let crate_name = crate_name.replace('-', "_");
    let dst_dir = out_dir.join(format!("assets/makepad/{crate_name}/{asset_subdir}"));
    mkdir(&dst_dir)?;
    cp_all(source_dir, &dst_dir, false)?;
    if config.small_fonts && asset_subdir == "resources" {
        for (target_name, replacement_name) in SMALL_FONT_REPLACEMENTS {
            let replacement = source_dir.join(replacement_name);
            let target = dst_dir.join(target_name);
            if replacement.is_file() && target.is_file() {
                cp(&replacement, &target, false)?;
            }
        }
    }

    let assets = ls(&dst_dir)?;
    for path in &assets {
        let path = path.display().to_string().replace("\\", "/");
        assets_to_add.push(format!("assets/makepad/{crate_name}/{asset_subdir}/{path}"));
    }
    Ok(())
}

fn add_font_assets_dir_to_apk(
    out_dir: &Path,
    assets_to_add: &mut Vec<String>,
    crate_name: &str,
    source_dir: &Path,
    resource_dir: &Path,
    config: &AndroidConfig,
) -> Result<(), String> {
    if !source_dir.is_dir() {
        return Ok(());
    }

    let crate_name = crate_name.replace('-', "_");
    let dst_dir = out_dir.join(format!("assets/makepad/{crate_name}/fonts"));
    let assets = ls(source_dir)?;
    for path in &assets {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        if !matches!(
            ext.as_deref(),
            Some("ttf" | "otf" | "ttc" | "woff" | "woff2")
        ) {
            continue;
        }
        // Skip files that already ship from the sibling `resources/` dir —
        // otherwise the same TTF lands in the APK twice. The widgets crate
        // for instance keeps LXGWWenKai*.ttf and NotoColorEmoji.ttf in both.
        if resource_dir.join(path).is_file() {
            continue;
        }
        cp(&source_dir.join(path), &dst_dir.join(path), false)?;
        let path = path.display().to_string().replace("\\", "/");
        assets_to_add.push(format!("assets/makepad/{crate_name}/fonts/{path}"));
    }
    if config.small_fonts {
        for (target_name, replacement_name) in SMALL_FONT_REPLACEMENTS {
            let replacement = source_dir
                .join(replacement_name)
                .is_file()
                .then(|| source_dir.join(replacement_name))
                .or_else(|| {
                    resource_dir
                        .join(replacement_name)
                        .is_file()
                        .then(|| resource_dir.join(replacement_name))
                });
            let target = dst_dir.join(target_name);
            if let Some(replacement) = replacement {
                if target.is_file() {
                    cp(&replacement, &target, false)?;
                }
            }
        }
    }
    Ok(())
}

fn build_zipaligned_apk(
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

fn sign_apk(
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

struct AabPaths {
    aab_dir: PathBuf,
    staged_assets_dir: PathBuf,
    staged_libs_dir: PathBuf,
    compiled_res_zip: PathBuf,
    proto_apk: PathBuf,
    base_module_dir: PathBuf,
    base_module_zip: PathBuf,
    dst_aab: PathBuf,
}

fn prepare_aab_paths(build_crate: &str, app_label: &str) -> Result<AabPaths, String> {
    let cwd = std::env::current_dir().unwrap();
    let target_dir = cargo_target_dir(&cwd);
    let underscore_build_crate = build_crate.replace('-', "_");
    let aab_dir = target_dir
        .join("makepad-android-aab")
        .join(&underscore_build_crate);
    let _ = rmdir(&aab_dir);
    mkdir(&aab_dir)?;

    let staged_assets_dir = aab_dir.join("assets");
    let staged_libs_dir = aab_dir.join("lib");
    let compiled_res_zip = aab_dir.join("compiled_res.zip");
    let proto_apk = aab_dir.join("base_proto.apk");
    let base_module_dir = aab_dir.join("base");
    let base_module_zip = aab_dir.join("base.zip");
    let dst_aab = aab_dir.join(format!("{}.aab", to_snakecase(app_label)));

    mkdir(&staged_assets_dir)?;
    mkdir(&staged_libs_dir)?;

    Ok(AabPaths {
        aab_dir,
        staged_assets_dir,
        staged_libs_dir,
        compiled_res_zip,
        proto_apk,
        base_module_dir,
        base_module_zip,
        dst_aab,
    })
}

fn stage_aab_assets(
    build_crate: &str,
    aab_dir: &Path,
    build_dir: &Path,
    android_targets: &[AndroidTarget],
    variant: &AndroidVariant,
    config: &AndroidConfig,
) -> Result<(), String> {
    let mut ignored_assets_to_add = Vec::new();
    let build_crate_dir = get_crate_dir(build_crate)?;
    add_assets_dir_to_apk(
        aab_dir,
        &mut ignored_assets_to_add,
        build_crate,
        &build_crate_dir.join("resources"),
        "resources",
        config,
    )?;
    add_font_assets_dir_to_apk(
        aab_dir,
        &mut ignored_assets_to_add,
        build_crate,
        &build_crate_dir.join("fonts"),
        &build_crate_dir.join("resources"),
        config,
    )?;

    let deps = get_crate_dep_dirs(build_crate, build_dir, &android_targets[0].toolchain());
    for (name, dep_dir) in deps.iter() {
        add_assets_dir_to_apk(
            aab_dir,
            &mut ignored_assets_to_add,
            name,
            &dep_dir.join("resources"),
            "resources",
            config,
        )?;
        add_font_assets_dir_to_apk(
            aab_dir,
            &mut ignored_assets_to_add,
            name,
            &dep_dir.join("fonts"),
            &dep_dir.join("resources"),
            config,
        )?;
    }

    if let AndroidVariant::Quest = variant {
        let dst_dir = aab_dir.join("assets/makepad/makepad_widgets/resources");
        for remove in [
            "fa-solid-900.ttf",
            "LiberationMono-Regular.ttf",
            "NotoSans-Regular.ttf",
        ] {
            let remove_path = dst_dir.join(remove);
            if remove_path.is_file() {
                rm(&remove_path)?;
            }
        }
    }

    Ok(())
}

fn aapt2_compile_resources(
    sdk_dir: &Path,
    res_dir: &Path,
    out_zip: &Path,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    shell_env_cap(
        &[],
        &cwd,
        aapt2_path(sdk_dir, urls).to_str().unwrap(),
        &[
            "compile",
            "--dir",
            res_dir.to_str().unwrap(),
            "-o",
            out_zip.to_str().unwrap(),
        ],
    )?;
    Ok(())
}

fn aapt2_link_proto_apk(
    sdk_dir: &Path,
    manifest_xml: &Path,
    compiled_res_zip: &Path,
    assets_dir: &Path,
    out_apk: &Path,
    urls: &AndroidSDKUrls,
) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    let android_jar = android_jar_path(sdk_dir, urls);
    let android_jar_str = android_jar.to_str().unwrap().to_string();
    let manifest_str = manifest_xml.to_str().unwrap().to_string();
    let out_str = out_apk.to_str().unwrap().to_string();
    let res_zip_str = compiled_res_zip.to_str().unwrap().to_string();
    let assets_str = assets_dir.to_str().unwrap().to_string();
    let has_assets = assets_dir.is_dir() && ls(assets_dir).map(|v| !v.is_empty()).unwrap_or(false);

    let mut args: Vec<&str> = vec![
        "link",
        "--proto-format",
        "--auto-add-overlay",
        "-I",
        &android_jar_str,
        "--manifest",
        &manifest_str,
        "-o",
        &out_str,
    ];
    if has_assets {
        args.push("-A");
        args.push(&assets_str);
    }
    args.push(&res_zip_str);

    shell_env_cap(
        &[],
        &cwd,
        aapt2_path(sdk_dir, urls).to_str().unwrap(),
        &args,
    )?;
    Ok(())
}

fn assemble_aab_base_module(
    sdk_dir: &Path,
    host_os: HostOs,
    proto_apk: &Path,
    classes_dex: &Path,
    libs_root: &Path,
    base_dir: &Path,
    base_zip: &Path,
) -> Result<(), String> {
    let _ = rmdir(base_dir);
    mkdir(base_dir)?;

    let mut zip_file =
        File::open(proto_apk).map_err(|e| format!("Cant open proto APK {:?}: {e}", proto_apk))?;
    let directory = zip_read_central_directory(&mut zip_file)
        .map_err(|e| format!("Cant read proto APK {:?}: {:?}", proto_apk, e))?;

    for header in &directory.file_headers {
        let entry_name = &header.file_name;
        if entry_name.ends_with('/') {
            continue;
        }
        let data = header
            .extract(&mut zip_file)
            .map_err(|e| format!("Failed to extract {entry_name} from proto APK: {:?}", e))?;
        let dst_rel = if entry_name == "AndroidManifest.xml" {
            "manifest/AndroidManifest.xml".to_string()
        } else {
            entry_name.clone()
        };
        let dst_path = base_dir.join(&dst_rel);
        mkdir(dst_path.parent().unwrap())?;
        let mut f =
            File::create(&dst_path).map_err(|e| format!("Cant write {:?}: {e}", dst_path))?;
        f.write_all(&data)
            .map_err(|e| format!("Cant write to {:?}: {e}", dst_path))?;
    }

    cp(classes_dex, &base_dir.join("dex/classes.dex"), false)?;

    if libs_root.is_dir() && ls(libs_root).map(|v| !v.is_empty()).unwrap_or(false) {
        cp_all(libs_root, &base_dir.join("lib"), false)?;
    }

    let java_home = resolve_java_home(sdk_dir, host_os);
    if base_zip.is_file() {
        rm(base_zip)?;
    }
    shell_env_cap(
        &[("JAVA_HOME", java_home.to_str().unwrap())],
        base_dir,
        java_tool_path(&java_home, "jar").to_str().unwrap(),
        &["cMf", base_zip.to_str().unwrap(), "."],
    )?;

    Ok(())
}

fn run_bundletool_build_bundle(
    sdk_dir: &Path,
    host_os: HostOs,
    base_zip: &Path,
    aab_path: &Path,
) -> Result<(), String> {
    let java_home = resolve_java_home(sdk_dir, host_os);
    let bundletool = bundletool_jar_path(sdk_dir);
    if !bundletool.is_file() {
        return Err(format!(
            "bundletool jar not found at {:?}. Re-run `cargo makepad android install-toolchain` to download it.",
            bundletool
        ));
    }
    if aab_path.is_file() {
        rm(aab_path)?;
    }
    let cwd = std::env::current_dir().unwrap();
    let modules_arg = format!("--modules={}", base_zip.display());
    let output_arg = format!("--output={}", aab_path.display());
    shell_env_cap(
        &[("JAVA_HOME", java_home.to_str().unwrap())],
        &cwd,
        java_tool_path(&java_home, "java").to_str().unwrap(),
        &[
            "-jar",
            bundletool.to_str().unwrap(),
            "build-bundle",
            &modules_arg,
            &output_arg,
        ],
    )?;
    Ok(())
}

fn resolve_jarsigner(sdk_dir: &Path, host_os: HostOs) -> Option<PathBuf> {
    let java_home = resolve_java_home(sdk_dir, host_os);
    let from_home = java_tool_path(&java_home, "jarsigner");
    if from_home.is_file() {
        return Some(from_home);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["jarsigner", "jarsigner.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct AabSigningOpts {
    pub keystore: PathBuf,
    pub storepass: String,
    pub key_alias: String,
    pub keypass: String,
}

fn sign_aab(
    sdk_dir: &Path,
    host_os: HostOs,
    aab_path: &Path,
    opts: &AabSigningOpts,
) -> Result<(), String> {
    let jarsigner = resolve_jarsigner(sdk_dir, host_os).ok_or_else(|| {
        "jarsigner not found. Re-run `cargo makepad android install-toolchain`, or set JAVA_HOME to a full JDK install."
            .to_string()
    })?;
    let java_home = resolve_java_home(sdk_dir, host_os);
    let cwd = std::env::current_dir().unwrap();
    shell_env_cap(
        &[("JAVA_HOME", java_home.to_str().unwrap())],
        &cwd,
        jarsigner.to_str().unwrap(),
        &[
            "-keystore",
            opts.keystore.to_str().unwrap(),
            "-storepass",
            &opts.storepass,
            "-keypass",
            &opts.keypass,
            aab_path.to_str().unwrap(),
            &opts.key_alias,
        ],
    )?;
    Ok(())
}

pub struct BuildAabResult {
    #[allow(dead_code)]
    pub dst_aab: PathBuf,
}

pub fn build_aab(
    sdk_dir: &Path,
    host_os: HostOs,
    package_name: Option<String>,
    app_label: Option<String>,
    version_code: Option<VersionCodeStrategy>,
    version_name: Option<String>,
    min_sdk_version: Option<usize>,
    args: &[String],
    android_targets: &[AndroidTarget],
    variant: &AndroidVariant,
    config: &AndroidConfig,
    urls: &AndroidSDKUrls,
    signing: Option<AabSigningOpts>,
) -> Result<BuildAabResult, String> {
    let build_crate = get_build_crate_from_args(args)?;
    let binary_name =
        get_package_binary_name(build_crate).unwrap_or_else(|| build_crate.to_string());
    let underscore_build_crate = build_crate.replace('-', "_");

    let resolved = resolve_packaging_inputs(
        build_crate,
        &binary_name,
        package_name,
        app_label,
        version_code,
        version_name,
        min_sdk_version,
        urls,
    )?;
    let mut effective_urls = *urls;
    if let Some(min_sdk_version) = resolved.min_sdk_version_override {
        effective_urls.sdk_version = min_sdk_version;
    }
    let urls = &effective_urls;

    if let Some(icon) = resolve_app_icon_env(build_crate)? {
        for (var, value) in APP_ICON_ENV_VARS.iter().zip(icon.iter()) {
            std::env::set_var(var, value);
        }
    }

    let resolved_sdk = preflight_android_sdk(sdk_dir, host_os, urls, android_targets)?;
    std::env::set_var("ANDROID_PLATFORM", &resolved_sdk.platform);
    std::env::set_var("ANDROID_SDK_VERSION", resolved_sdk.platform_api.to_string());
    std::env::set_var("ANDROID_API_LEVEL", resolved_sdk.compiler_api.to_string());
    std::env::set_var(
        "ANDROID_BUILD_TOOLS_VERSION",
        &resolved_sdk.build_tools_version,
    );
    std::env::set_var("JAVA_HOME", &resolved_sdk.java_home);
    std::env::set_var("ANDROID_NDK_PREBUILT_ROOT", &resolved_sdk.ndk_prebuilt_root);

    rust_build(
        sdk_dir,
        host_os,
        build_crate,
        args,
        android_targets,
        variant,
        urls,
        false,
    )?;

    let prep_opts = PrepareBuildOpts {
        build_crate,
        java_url: &resolved.java_url,
        app_label: &resolved.app_label,
        variant,
        config,
        urls,
        version_code: resolved.version_code,
        version_name: &resolved.version_name,
        debuggable: false,
    };
    let build_paths = prepare_build(&prep_opts)?;
    let aab_paths = prepare_aab_paths(build_crate, &resolved.app_label)?;

    println!(
        "Building AAB (package={}, label={}, versionCode={}, versionName={}, minSdkVersion={}, targetSdkVersion={})",
        resolved.java_url,
        resolved.app_label,
        resolved.version_code,
        resolved.version_name,
        urls.sdk_version,
        urls.target_sdk_version
    );

    build_r_class(sdk_dir, host_os, &build_paths, urls)?;
    compile_java(sdk_dir, host_os, &build_paths, urls)?;
    build_dex(sdk_dir, host_os, &build_paths, urls)?;
    let classes_dex = build_paths.out_dir.join("classes.dex");
    if !classes_dex.is_file() {
        return Err(format!(
            "d8 did not produce classes.dex at {:?}",
            classes_dex
        ));
    }

    let build_dir = stage_aab_native_libs(
        sdk_dir,
        host_os,
        &underscore_build_crate,
        &aab_paths.staged_libs_dir,
        android_targets,
        args,
        variant,
        urls,
    )?;

    stage_aab_assets(
        build_crate,
        &aab_paths.aab_dir,
        &build_dir,
        android_targets,
        variant,
        config,
    )?;

    aapt2_compile_resources(
        sdk_dir,
        &build_paths.res_dir,
        &aab_paths.compiled_res_zip,
        urls,
    )?;
    aapt2_link_proto_apk(
        sdk_dir,
        &build_paths.manifest_file,
        &aab_paths.compiled_res_zip,
        &aab_paths.staged_assets_dir,
        &aab_paths.proto_apk,
        urls,
    )?;

    assemble_aab_base_module(
        sdk_dir,
        host_os,
        &aab_paths.proto_apk,
        &classes_dex,
        &aab_paths.staged_libs_dir,
        &aab_paths.base_module_dir,
        &aab_paths.base_module_zip,
    )?;

    run_bundletool_build_bundle(
        sdk_dir,
        host_os,
        &aab_paths.base_module_zip,
        &aab_paths.dst_aab,
    )?;

    if let Some(opts) = &signing {
        sign_aab(sdk_dir, host_os, &aab_paths.dst_aab, opts)?;
    } else {
        println!("Skipping signing (--no-sign). The AAB is unsigned and will not be accepted by the Play Store as-is.");
    }

    println!("AAB Build completed: {}", aab_paths.dst_aab.display());
    Ok(BuildAabResult {
        dst_aab: aab_paths.dst_aab,
    })
}

pub fn build(
    sdk_dir: &Path,
    host_os: HostOs,
    package_name: Option<String>,
    app_label: Option<String>,
    version_code: Option<VersionCodeStrategy>,
    version_name: Option<String>,
    min_sdk_version: Option<usize>,
    args: &[String],
    android_targets: &[AndroidTarget],
    variant: &AndroidVariant,
    config: &AndroidConfig,
    urls: &AndroidSDKUrls,
) -> Result<BuildResult, String> {
    let build_crate = get_build_crate_from_args(args)?;
    let binary_name =
        get_package_binary_name(build_crate).unwrap_or_else(|| build_crate.to_string());
    let underscore_build_crate = build_crate.replace('-', "_");

    let resolved = resolve_packaging_inputs(
        build_crate,
        &binary_name,
        package_name,
        app_label,
        version_code,
        version_name,
        min_sdk_version,
        urls,
    )?;
    let mut effective_urls = *urls;
    if let Some(min_sdk_version) = resolved.min_sdk_version_override {
        effective_urls.sdk_version = min_sdk_version;
    }
    let urls = &effective_urls;

    if let Some(icon) = resolve_app_icon_env(build_crate)? {
        for (var, value) in APP_ICON_ENV_VARS.iter().zip(icon.iter()) {
            std::env::set_var(var, value);
        }
    }

    let resolved_sdk = preflight_android_sdk(sdk_dir, host_os, urls, android_targets)?;
    std::env::set_var("ANDROID_PLATFORM", &resolved_sdk.platform);
    std::env::set_var("ANDROID_SDK_VERSION", resolved_sdk.platform_api.to_string());
    std::env::set_var("ANDROID_API_LEVEL", resolved_sdk.compiler_api.to_string());
    std::env::set_var(
        "ANDROID_BUILD_TOOLS_VERSION",
        &resolved_sdk.build_tools_version,
    );
    std::env::set_var("JAVA_HOME", &resolved_sdk.java_home);
    std::env::set_var("ANDROID_NDK_PREBUILT_ROOT", &resolved_sdk.ndk_prebuilt_root);

    timed_android_phase("rust_build", || {
        rust_build(
            sdk_dir,
            host_os,
            build_crate,
            args,
            android_targets,
            variant,
            urls,
            true,
        )
    })?;
    let debuggable = get_profile_from_args(args) != "release";
    let prep_opts = PrepareBuildOpts {
        build_crate,
        java_url: &resolved.java_url,
        app_label: &resolved.app_label,
        variant,
        config,
        urls,
        version_code: resolved.version_code,
        version_name: &resolved.version_name,
        debuggable,
    };
    let build_paths = timed_android_phase("prepare_build", || prepare_build(&prep_opts))?;

    eprintln!(
        "Building APK (package={}, label={}, versionCode={}, versionName={}, minSdkVersion={}, targetSdkVersion={}, debuggable={})",
        resolved.java_url,
        resolved.app_label,
        resolved.version_code,
        resolved.version_name,
        urls.sdk_version,
        urls.target_sdk_version,
        debuggable
    );
    timed_android_phase("build_r_class", || {
        build_r_class(sdk_dir, host_os, &build_paths, urls)
    })?;
    timed_android_phase("compile_java", || {
        compile_java(sdk_dir, host_os, &build_paths, urls)
    })?;
    timed_android_phase("build_dex", || {
        build_dex(sdk_dir, host_os, &build_paths, urls)
    })?;
    timed_android_phase("build_unaligned_apk", || {
        build_unaligned_apk(sdk_dir, host_os, &build_paths, urls)
    })?;
    let build_dir = timed_android_phase("add_rust_library", || {
        add_rust_library(
            sdk_dir,
            host_os,
            &underscore_build_crate,
            &build_paths,
            android_targets,
            args,
            variant,
            urls,
        )
    })?;
    timed_android_phase("add_resources", || {
        add_resources(
            sdk_dir,
            build_crate,
            &build_paths,
            &build_dir,
            android_targets,
            variant,
            config,
            urls,
        )
    })?;
    timed_android_phase("build_zipaligned_apk", || {
        build_zipaligned_apk(sdk_dir, &build_paths, urls)
    })?;
    timed_android_phase("sign_apk", || {
        sign_apk(sdk_dir, host_os, &build_paths, urls)
    })?;

    eprintln!("APK Build completed");
    Ok(BuildResult {
        dst_apk: build_paths.dst_apk,
        java_url: resolved.java_url,
    })
}

pub fn run(
    sdk_dir: &Path,
    host_os: HostOs,
    package_name: Option<String>,
    app_label: Option<String>,
    version_code: Option<VersionCodeStrategy>,
    version_name: Option<String>,
    min_sdk_version: Option<usize>,
    args: &[String],
    targets: &[AndroidTarget],
    android_variant: &AndroidVariant,
    config: &AndroidConfig,
    urls: &AndroidSDKUrls,
    devices: Vec<String>,
) -> Result<(), String> {
    let build_crate = get_build_crate_from_args(args)?;
    let result = build(
        sdk_dir,
        host_os,
        package_name,
        app_label,
        version_code,
        version_name,
        min_sdk_version,
        args,
        targets,
        android_variant,
        config,
        urls,
    )?;

    let cwd = std::env::current_dir().unwrap();
    // alright so how will we do multiple targets eh

    fn android_start_args(java_url: &str, build_crate: &str) -> Vec<String> {
        let mut args = vec![
            "shell".to_string(),
            "am".to_string(),
            "start".to_string(),
            "-S".to_string(),
            "-n".to_string(),
            format!("{0}/{0}.MakepadApp", java_url),
        ];
        if let Ok(studio_host) = std::env::var("STUDIO_HOST") {
            if !studio_host.trim().is_empty() {
                println!("Android launch intent makepad.STUDIO_HOST={}", studio_host);
                args.push("--es".to_string());
                args.push("makepad.STUDIO_HOST".to_string());
                args.push(studio_host);
                args.push("--es".to_string());
                args.push("makepad.STUDIO_CRATE".to_string());
                args.push(build_crate.to_string());
            }
        } else {
            println!("Android launch intent makepad.STUDIO_HOST is not set");
        }
        args
    }

    if devices.len() == 0 {
        println!("Uploading android application");
        shell_env_cap(
            &[],
            &cwd,
            sdk_dir.join("platform-tools/adb").to_str().unwrap(),
            &[
                "install",
                "--no-incremental",
                "-r",
                (result.dst_apk.to_str().unwrap()),
            ],
        )?;
        println!("Starting android application");
        let start_args = android_start_args(&result.java_url, build_crate);
        let start_args_refs = start_args
            .iter()
            .map(|arg| arg.as_str())
            .collect::<Vec<_>>();
        shell_env_cap(
            &[],
            &cwd,
            sdk_dir.join("platform-tools/adb").to_str().unwrap(),
            &start_args_refs,
        )?;
        #[allow(unused_assignments)]
        let mut pid = None;
        loop {
            if let Ok(thing) = shell_env_cap(
                &[],
                &cwd,
                sdk_dir.join("platform-tools/adb").to_str().unwrap(),
                &["shell", "pidof", &result.java_url],
            ) {
                pid = Some(thing.trim().to_string());
                break;
            }
        }
        shell_env(
            &[],
            &cwd,
            sdk_dir.join("platform-tools/adb").to_str().unwrap(),
            &["logcat", "--pid", &pid.unwrap(), "Makepad:D *:S"],
        )?;
    } else {
        let mut children = Vec::new();
        println!("Uploading android application");
        for device in &devices {
            children.push(shell_child_create(
                &[],
                &cwd,
                sdk_dir.join("platform-tools/adb").to_str().unwrap(),
                &[
                    "-s",
                    &device,
                    "install",
                    "--no-incremental",
                    "-r",
                    (result.dst_apk.to_str().unwrap()),
                ],
            )?);
        }
        for child in children {
            shell_child_wait(child)?;
        }
        let mut children = Vec::new();
        println!("Starting android application");
        for device in &devices {
            let start_args = android_start_args(&result.java_url, build_crate);
            let mut device_args = vec!["-s".to_string(), device.clone()];
            device_args.extend(start_args);
            let device_args_refs = device_args
                .iter()
                .map(|arg| arg.as_str())
                .collect::<Vec<_>>();
            children.push(shell_child_create(
                &[],
                &cwd,
                sdk_dir.join("platform-tools/adb").to_str().unwrap(),
                &device_args_refs,
            )?);
        }
        for child in children {
            shell_child_wait(child)?;
        }
    }
    Ok(())
}

pub fn adb(sdk_dir: &Path, _host_os: HostOs, args: &[String]) -> Result<(), String> {
    let mut args_out = Vec::new();
    for arg in args {
        args_out.push(arg.as_ref());
    }
    let cwd = std::env::current_dir().unwrap();
    shell_env(
        &[],
        &cwd,
        sdk_dir.join("platform-tools/adb").to_str().unwrap(),
        &args_out,
    )?;
    Ok(())
}

fn adb_path(sdk_dir: &Path) -> PathBuf {
    sdk_dir.join("platform-tools/adb")
}

fn push_serial_args<'a>(serial: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(args.len() + 2);
    if let Some(serial) = serial {
        out.push("-s");
        out.push(serial);
    }
    out.extend_from_slice(args);
    out
}

fn adb_cap(sdk_dir: &Path, serial: Option<&str>, args: &[&str]) -> Result<String, String> {
    let cwd = std::env::current_dir().unwrap();
    let args_out = push_serial_args(serial, args);
    shell_env_cap(&[], &cwd, adb_path(sdk_dir).to_str().unwrap(), &args_out)
}

fn adb_run(sdk_dir: &Path, serial: Option<&str>, args: &[&str]) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap();
    let args_out = push_serial_args(serial, args);
    shell_env(&[], &cwd, adb_path(sdk_dir).to_str().unwrap(), &args_out)
}

fn parse_adb_devices(output: &str) -> Vec<String> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices attached") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else {
            continue;
        };
        let Some(state) = parts.next() else {
            continue;
        };
        if state == "device" {
            devices.push(serial.to_string());
        }
    }
    devices
}

pub fn list_connected_devices(sdk_dir: &Path) -> Result<Vec<String>, String> {
    let output = adb_cap(sdk_dir, None, &["devices"])?;
    Ok(parse_adb_devices(&output))
}

fn parse_ipv4_token(token: &str) -> Option<&str> {
    let candidate = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
    let mut parts = candidate.split('.');
    let mut count = 0usize;
    while let Some(part) = parts.next() {
        if part.is_empty() || part.len() > 3 {
            return None;
        }
        if part.parse::<u8>().is_err() {
            return None;
        }
        count += 1;
    }
    if count == 4 && candidate != "0.0.0.0" && candidate != "127.0.0.1" {
        Some(candidate)
    } else {
        None
    }
}

fn parse_ip_addr_show(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        while let Some(part) = parts.next() {
            if part == "inet" {
                if let Some(addr) = parts.next() {
                    if let Some(ip) = parse_ipv4_token(addr.split('/').next().unwrap_or(addr)) {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }
    None
}

fn parse_ip_route(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        while let Some(part) = parts.next() {
            if part == "src" {
                if let Some(ip) = parts.next().and_then(parse_ipv4_token) {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

fn detect_device_ip(sdk_dir: &Path, serial: Option<&str>) -> Result<String, String> {
    let addr_show = adb_cap(
        sdk_dir,
        serial,
        &[
            "shell", "ip", "-f", "inet", "addr", "show", "scope", "global",
        ],
    )?;
    if let Some(ip) = parse_ip_addr_show(&addr_show) {
        return Ok(ip);
    }

    let route = adb_cap(sdk_dir, serial, &["shell", "ip", "route"])?;
    if let Some(ip) = parse_ip_route(&route) {
        return Ok(ip);
    }

    Err(format!(
        "Could not determine device IP address over adb. `ip -f inet addr show scope global` output:\n{}\n`ip route` output:\n{}",
        addr_show.trim(),
        route.trim()
    ))
}

pub fn adb_tcp(
    sdk_dir: &Path,
    _host_os: HostOs,
    devices: &[String],
    args: &[String],
) -> Result<(), String> {
    let port = match args {
        [] => 5555u16,
        [port] => port
            .parse::<u16>()
            .map_err(|_| format!("Invalid adb-tcp port `{port}`"))?,
        _ => {
            return Err(
                "adb-tcp accepts at most one optional argument: the tcp port (default 5555)"
                    .to_string(),
            )
        }
    };
    let port_string = port.to_string();

    if devices.is_empty() {
        let ip = detect_device_ip(sdk_dir, None)?;
        println!("Detected device IP: {ip}");
        adb_run(sdk_dir, None, &["tcpip", &port_string])?;
        thread::sleep(Duration::from_secs(1));
        let output = adb_cap(sdk_dir, None, &["connect", &format!("{ip}:{port}")])?;
        print!("{output}");
        return Ok(());
    }

    for device in devices {
        let ip = detect_device_ip(sdk_dir, Some(device))?;
        println!("Detected device IP for {device}: {ip}");
        adb_run(sdk_dir, Some(device), &["tcpip", &port_string])?;
        thread::sleep(Duration::from_secs(1));
        let output = adb_cap(sdk_dir, None, &["connect", &format!("{ip}:{port}")])?;
        print!("{output}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{compose_android_rustflags, parse_adb_devices, parse_ip_addr_show, parse_ip_route};

    #[test]
    fn parse_adb_devices_filters_ready_targets() {
        let output = "\
List of devices attached\n\
emulator-5554          device product:sdk_gphone64 model:sdk_gphone64 device:emu64 transport_id:1\n\
quest-offline          offline transport_id:2\n\
quest-unauthorized     unauthorized usb:3\n\
10.0.0.151:5555        device product:eureka model:Quest_3 device:eureka transport_id:4\n\
\n";
        assert_eq!(
            parse_adb_devices(output),
            vec!["emulator-5554".to_string(), "10.0.0.151:5555".to_string()]
        );
    }

    #[test]
    fn parse_ip_addr_show_extracts_ipv4() {
        let output = "\
2: wlan0    inet 192.168.0.42/24 brd 192.168.0.255 scope global wlan0\n\
   valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_ip_addr_show(output), Some("192.168.0.42".to_string()));
    }

    #[test]
    fn parse_ip_route_prefers_src_ipv4() {
        let output = "\
default via 192.168.0.1 dev wlan0 proto dhcp src 192.168.0.42 metric 303\n\
192.168.0.0/24 dev wlan0 proto kernel scope link src 192.168.0.42\n";
        assert_eq!(parse_ip_route(output), Some("192.168.0.42".to_string()));
    }

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

pub fn java(sdk_dir: &Path, host_os: HostOs, args: &[String]) -> Result<(), String> {
    let mut args_out = Vec::new();
    for arg in args {
        args_out.push(arg.as_ref());
    }
    let cwd = std::env::current_dir().unwrap();
    let java_home = resolve_java_home(sdk_dir, host_os);
    shell_env(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        java_tool_path(&java_home, "java").to_str().unwrap(),
        &args_out,
    )?;
    Ok(())
}

pub fn javac(sdk_dir: &Path, host_os: HostOs, args: &[String]) -> Result<(), String> {
    let mut args_out = Vec::new();
    for arg in args {
        args_out.push(arg.as_ref());
    }
    let cwd = std::env::current_dir().unwrap();
    let java_home = resolve_java_home(sdk_dir, host_os);
    shell_env(
        &[("JAVA_HOME", (java_home.to_str().unwrap()))],
        &cwd,
        java_tool_path(&java_home, "javac").to_str().unwrap(),
        &args_out,
    )?;
    Ok(())
}

fn to_snakecase(label: &str) -> String {
    let mut snakecase = String::new();
    let mut previous_was_underscore = false;

    for c in label.chars() {
        if c.is_whitespace() {
            previous_was_underscore = true;
        } else if c.is_uppercase() {
            if !previous_was_underscore && !snakecase.is_empty() {
                snakecase.push('_');
            }
            snakecase.extend(c.to_lowercase());
            previous_was_underscore = false;
        } else {
            snakecase.push(c);
            previous_was_underscore = false;
        }
    }
    snakecase
}
