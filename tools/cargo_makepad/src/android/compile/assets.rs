use super::super::sdk::AndroidSDKUrls;
use super::{toolchain::aapt_path, BuildPaths};
use crate::android::{AndroidConfig, AndroidTarget, AndroidVariant};
use crate::makepad_shell::{cp, cp_all, ls, mkdir, rm, shell_env_cap};
use crate::utils::{get_crate_dep_dirs, get_crate_dir};
use std::path::Path;

const SMALL_FONT_REPLACEMENTS: [(&str, &str); 5] = [
    ("GoNotoKurrent-Bold.ttf", "IBMPlexSans-SemiBold.ttf"),
    ("GoNotoKurrent-Regular.ttf", "IBMPlexSans-Text.ttf"),
    ("LXGWWenKaiBold.ttf", "IBMPlexSans-Text.ttf"),
    ("LXGWWenKaiRegular.ttf", "IBMPlexSans-Text.ttf"),
    ("NotoColorEmoji.ttf", "IBMPlexSans-Text.ttf"),
];

pub(super) fn add_resources(
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

pub(super) fn stage_aab_assets(
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
