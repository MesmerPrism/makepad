use super::super::sdk::AndroidSDKUrls;
use super::{
    rust_build::cargo_target_dir,
    to_snakecase,
    toolchain::{
        aapt2_path, android_jar_path, bundletool_jar_path, java_tool_path, resolve_java_home,
    },
};
use crate::android::HostOs;
use crate::makepad_shell::{cp, cp_all, ls, mkdir, rm, rmdir, shell_env_cap};
use makepad_zip_file::*;
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) struct AabPaths {
    pub(super) aab_dir: PathBuf,
    pub(super) staged_assets_dir: PathBuf,
    pub(super) staged_libs_dir: PathBuf,
    pub(super) compiled_res_zip: PathBuf,
    pub(super) proto_apk: PathBuf,
    pub(super) base_module_dir: PathBuf,
    pub(super) base_module_zip: PathBuf,
    pub(super) dst_aab: PathBuf,
}

pub(super) fn prepare_aab_paths(build_crate: &str, app_label: &str) -> Result<AabPaths, String> {
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

pub(super) fn aapt2_compile_resources(
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

pub(super) fn aapt2_link_proto_apk(
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

pub(super) fn assemble_aab_base_module(
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

pub(super) fn run_bundletool_build_bundle(
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

pub(super) fn sign_aab(
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
