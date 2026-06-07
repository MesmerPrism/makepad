use crate::makepad_shell::mkdir;
use crate::utils::get_crate_dir;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

fn has_explicit_lib_target(cargo_toml: &str, crate_dir: &Path) -> bool {
    crate_dir.join("src/lib.rs").is_file()
        || cargo_toml
            .lines()
            .any(|line| line.trim_start().starts_with("[lib]"))
}

pub(super) fn normalize_toml_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    path.strip_prefix("//?/").unwrap_or(&path).to_string()
}

fn absolutize_manifest_path(crate_dir: &Path, value: &str) -> String {
    if value.contains("://") || Path::new(value).is_absolute() {
        return value.to_string();
    }
    let joined = crate_dir.join(value);
    normalize_toml_path(&joined.canonicalize().unwrap_or(joined))
}

fn rewrite_relative_toml_value(line: &mut String, key: &str, crate_dir: &Path) {
    for needle in [format!("{key} ="), format!("{key}=")] {
        let mut search_from = 0;
        loop {
            let Some(rel_pos) = line[search_from..].find(&needle) else {
                break;
            };
            let value_key_start = search_from + rel_pos;
            let mut quote_pos = value_key_start + needle.len();
            while line
                .as_bytes()
                .get(quote_pos)
                .is_some_and(|v| v.is_ascii_whitespace())
            {
                quote_pos += 1;
            }
            let Some(&quote) = line.as_bytes().get(quote_pos) else {
                break;
            };
            if quote != b'"' && quote != b'\'' {
                search_from = quote_pos.saturating_add(1);
                continue;
            }
            let mut value_end = quote_pos + 1;
            while let Some(&ch) = line.as_bytes().get(value_end) {
                if ch == quote && line.as_bytes().get(value_end.saturating_sub(1)) != Some(&b'\\') {
                    break;
                }
                value_end += 1;
            }
            if value_end >= line.len() {
                break;
            }

            let value = line[quote_pos + 1..value_end].to_string();
            let replacement = absolutize_manifest_path(crate_dir, &value);
            line.replace_range(quote_pos + 1..value_end, &replacement);
            search_from = quote_pos + replacement.len() + 2;
        }
    }
}

fn rewrite_wrapper_manifest_paths(cargo_toml: &str, crate_dir: &Path) -> String {
    let mut out = String::with_capacity(cargo_toml.len() + 256);
    for raw_line in cargo_toml.lines() {
        let mut line = raw_line.to_string();
        for key in ["path", "build", "readme", "license-file"] {
            rewrite_relative_toml_value(&mut line, key, crate_dir);
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn extract_workspace_patch_sections(workspace_manifest: &str) -> String {
    let mut out = String::new();
    let mut current_section: Option<String> = None;
    let mut current_body = Vec::new();

    let flush_section =
        |out: &mut String, current_section: &mut Option<String>, current_body: &mut Vec<String>| {
            let Some(section) = current_section.take() else {
                current_body.clear();
                return;
            };
            if !section.starts_with("[patch.") {
                current_body.clear();
                return;
            }

            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&section);
            out.push('\n');
            for line in current_body.iter() {
                out.push_str(line);
                out.push('\n');
            }
            current_body.clear();
        };

    for raw_line in workspace_manifest.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !raw_line.starts_with(' ') {
            flush_section(&mut out, &mut current_section, &mut current_body);
            current_section = Some(trimmed.to_string());
            continue;
        }

        if current_section.is_some() {
            current_body.push(raw_line.to_string());
        }
    }

    flush_section(&mut out, &mut current_section, &mut current_body);
    out
}

pub(super) fn strip_generated_wrapper_args(args: &[String], build_crate: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    let mut removed_positional = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }

        let skip_arg = matches!(
            arg.as_str(),
            "-p" | "--package"
                | "--manifest-path"
                | "--exclude"
                | "--bin"
                | "--example"
                | "--test"
                | "--bench"
        );
        if skip_arg {
            skip_next = true;
            continue;
        }

        let skip_prefixed = arg.starts_with("--package=")
            || arg.starts_with("--manifest-path=")
            || arg.starts_with("--exclude=")
            || arg.starts_with("--bin=")
            || arg.starts_with("--example=")
            || arg.starts_with("--test=")
            || arg.starts_with("--bench=");
        if skip_prefixed
            || matches!(
                arg.as_str(),
                "--workspace"
                    | "--all-targets"
                    | "--bins"
                    | "--examples"
                    | "--tests"
                    | "--benches"
                    | "--lib"
            )
        {
            continue;
        }

        if !removed_positional && !arg.starts_with('-') && arg == build_crate {
            removed_positional = true;
            continue;
        }

        out.push(arg.clone());
    }

    out
}

fn write_file_if_changed(path: &Path, data: &[u8]) -> Result<(), String> {
    if fs::read(path)
        .map(|existing| existing == data)
        .unwrap_or(false)
    {
        return Ok(());
    }
    fs::write(path, data).map_err(|e| format!("Can't write {:?}: {:?}", path, e))
}

pub(super) fn generate_android_wrapper_manifest(
    build_crate: &str,
    target_root: &Path,
) -> Result<Option<PathBuf>, String> {
    let workspace_root = std::env::current_dir().unwrap();
    let crate_dir = get_crate_dir(build_crate)?;
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let cargo_toml = fs::read_to_string(&cargo_toml_path)
        .map_err(|e| format!("Can't read {:?}: {:?}", cargo_toml_path, e))?;

    if has_explicit_lib_target(&cargo_toml, &crate_dir) {
        return Ok(None);
    }

    let main_rs = crate_dir.join("src/main.rs");
    if !main_rs.is_file() {
        return Err(format!(
            "Package {build_crate} has no library target and no src/main.rs to wrap for Android"
        ));
    }

    let wrapper_dir = target_root
        .join("makepad-android-wrapper")
        .join(build_crate.replace('-', "_"));
    mkdir(&wrapper_dir)?;

    let mut wrapper_manifest = rewrite_wrapper_manifest_paths(&cargo_toml, &crate_dir);
    wrapper_manifest.push_str("\n[lib]\n");
    wrapper_manifest.push_str(&format!("path = \"{}\"\n", normalize_toml_path(&main_rs)));
    wrapper_manifest.push_str("\n[workspace]\n");
    wrapper_manifest.push_str("resolver = \"2\"\n");

    let workspace_manifest_path = workspace_root.join("Cargo.toml");
    if let Ok(workspace_manifest) = fs::read_to_string(&workspace_manifest_path) {
        let workspace_patches = extract_workspace_patch_sections(&workspace_manifest);
        if !workspace_patches.trim().is_empty() {
            wrapper_manifest.push('\n');
            wrapper_manifest.push_str(&rewrite_wrapper_manifest_paths(
                &workspace_patches,
                &workspace_root,
            ));
        }
    }

    let wrapper_manifest_path = wrapper_dir.join("Cargo.toml");
    write_file_if_changed(&wrapper_manifest_path, wrapper_manifest.as_bytes())?;

    if let Ok(lock_data) = fs::read(crate_dir.join("Cargo.lock"))
        .or_else(|_| fs::read(std::env::current_dir().unwrap().join("Cargo.lock")))
    {
        let mut hasher = DefaultHasher::new();
        lock_data.hash(&mut hasher);
        let source_lock_hash = format!("{:016x}", hasher.finish());
        let source_lock_hash_path = wrapper_dir.join(".makepad-source-lock.hash");
        let wrapper_lock_path = wrapper_dir.join("Cargo.lock");
        let source_lock_changed = fs::read_to_string(&source_lock_hash_path)
            .map(|cached| cached.trim() != source_lock_hash)
            .unwrap_or(true);

        if source_lock_changed || !wrapper_lock_path.is_file() {
            write_file_if_changed(&wrapper_lock_path, &lock_data)?;
            write_file_if_changed(&source_lock_hash_path, source_lock_hash.as_bytes())?;
        }
    }

    Ok(Some(wrapper_manifest_path))
}
