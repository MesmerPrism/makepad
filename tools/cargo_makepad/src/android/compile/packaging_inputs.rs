use super::super::sdk::AndroidSDKUrls;
use super::{cargo_target_dir, to_snakecase, BuildPaths};
use crate::android::{AndroidConfig, AndroidVariant, ManifestArgs};
use crate::makepad_shell::{cp_all, mkdir, rm, rmdir, write_text};
use crate::utils::{
    get_crate_dir, no_icon_requested, read_android_package_metadata, VersionCodeStrategy,
};
use std::{fs, path::Path};

pub(super) struct ResolvedPackagingInputs {
    pub(super) java_url: String,
    pub(super) app_label: String,
    pub(super) version_code: u32,
    pub(super) version_name: String,
    pub(super) min_sdk_version_override: Option<usize>,
}

pub(super) fn resolve_packaging_inputs(
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

pub(super) struct PrepareBuildOpts<'a> {
    pub(super) build_crate: &'a str,
    pub(super) java_url: &'a str,
    pub(super) app_label: &'a str,
    pub(super) variant: &'a AndroidVariant,
    pub(super) config: &'a AndroidConfig,
    pub(super) urls: &'a AndroidSDKUrls,
    pub(super) version_code: u32,
    pub(super) version_name: &'a str,
    pub(super) debuggable: bool,
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

pub(super) fn prepare_build(opts: &PrepareBuildOpts<'_>) -> Result<BuildPaths, String> {
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
