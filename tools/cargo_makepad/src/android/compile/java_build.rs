use super::super::sdk::AndroidSDKUrls;
use super::{
    toolchain::{aapt_path, android_jar_path, d8_jar_path, java_tool_path, resolve_java_home},
    BuildPaths,
};
use crate::android::HostOs;
use crate::makepad_shell::{ls, mkdir, rmdir, shell_env, shell_env_cap, write_text};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

pub(super) fn build_r_class(
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

pub(super) fn compile_java(
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
    let mut makepad_java_sources = fs::read_dir(makepad_java_classes_dir)
        .map_err(|e| {
            format!(
                "failed to list Makepad Android Java sources {:?}: {e}",
                makepad_java_classes_dir
            )
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|e| format!("failed to read Makepad Android Java source entry: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    makepad_java_sources
        .retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("java"));
    makepad_java_sources.sort();

    let mut java_sources = Vec::with_capacity(makepad_java_sources.len() + 3);
    java_sources.push(r_class_path.clone());
    java_sources.extend(makepad_java_sources);
    java_sources.push(build_paths.java_file.clone());
    java_sources.push(build_paths.xr_file.clone());

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

pub(super) fn build_dex(
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
