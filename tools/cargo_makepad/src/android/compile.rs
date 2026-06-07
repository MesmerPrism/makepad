use super::sdk::AndroidSDKUrls;
use crate::android::{AndroidConfig, AndroidTarget, AndroidVariant, HostOs};
use crate::makepad_shell::*;
use crate::utils::*;
use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod aab_assembly;
mod apk_assembly;
mod assets;
mod java_build;
mod keystore;
mod packaging_inputs;
mod rust_build;
mod shared_libs;
mod toolchain;
mod wrapper_manifest;
pub use aab_assembly::AabSigningOpts;
use aab_assembly::{
    aapt2_compile_resources, aapt2_link_proto_apk, assemble_aab_base_module, prepare_aab_paths,
    run_bundletool_build_bundle, sign_aab,
};
use apk_assembly::{add_rust_library, build_unaligned_apk, build_zipaligned_apk, sign_apk};
use assets::{add_resources, stage_aab_assets};
use java_build::{build_dex, build_r_class, compile_java};
#[allow(unused_imports)]
pub use keystore::{
    keystore_create, keystore_sidecar_path, read_keystore_sidecar, KeystoreCreateOpts,
    KeystoreSidecar,
};
use packaging_inputs::{prepare_build, resolve_packaging_inputs, PrepareBuildOpts};
use rust_build::rust_build;
use shared_libs::stage_aab_native_libs;
use toolchain::{java_tool_path, preflight_android_sdk, resolve_java_home};

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
    use super::{parse_adb_devices, parse_ip_addr_show, parse_ip_route};

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
