use crate::makepad_shell::*;
use makepad_toml_parser::{parse_toml, Toml};
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub enum VersionCodeStrategy {
    Explicit(u32),
    Auto,
}

impl VersionCodeStrategy {
    pub fn resolve(&self) -> u32 {
        match self {
            Self::Explicit(v) => *v,
            Self::Auto => generate_auto_version_code(),
        }
    }
}

pub fn parse_version_code_flag(value: &str) -> Result<VersionCodeStrategy, String> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(VersionCodeStrategy::Auto);
    }
    value
        .parse::<u32>()
        .map(VersionCodeStrategy::Explicit)
        .map_err(|_| {
            format!("--version-code must be a non-negative integer or `auto`, got {value:?}")
        })
}

pub fn generate_auto_version_code() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour) = unix_to_utc_ymdh(secs);
    let value =
        (year as u64) * 1_000_000 + (month as u64) * 10_000 + (day as u64) * 100 + (hour as u64);
    value as u32
}

fn unix_to_utc_ymdh(secs: u64) -> (u32, u32, u32, u32) {
    let days_since_epoch = (secs / 86_400) as i64;
    let hour = ((secs % 86_400) / 3_600) as u32;

    let z = days_since_epoch + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 {
        mp as u32 + 3
    } else {
        mp as u32 - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year as u32, month, day, hour)
}

pub fn extract_dependency_paths(line: &str) -> Option<(String, Option<PathBuf>)> {
    let dependency_output_start = line.find(|c: char| c.is_alphanumeric())?;
    let dependency_output = &line[dependency_output_start..];

    let mut tokens = dependency_output.split(' ');
    if let Some(name) = tokens.next() {
        for token in tokens.collect::<Vec<&str>>() {
            if token == "(*)" || token == "(proc-macro)" {
                continue;
            }
            if token.starts_with('(') {
                let path = token[1..token.len() - 1].to_owned();
                let path = Path::new(&path);
                if path.is_dir() {
                    return Some((name.to_string(), Some(path.into())));
                }
            }
        }
        return Some((name.to_string(), None));
    }
    None
}

pub fn get_crate_dir(build_crate: &str) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().unwrap();
    if let Ok(output) = shell_env_cap(&[], &cwd, "cargo", &["pkgid", "-p", build_crate]) {
        #[cfg(target_os = "windows")]
        {
            let output = output.strip_prefix("file:///").unwrap_or(&output);
            let output = output.strip_prefix("path+file:///").unwrap_or(output);
            return Ok(output.split('#').next().unwrap().into());
        }
        #[cfg(not(target_os = "windows"))]
        {
            let output = output.strip_prefix("file://").unwrap_or(&output);
            let output = output.strip_prefix("path+file://").unwrap_or(output);
            return Ok(output.split('#').next().unwrap().into());
        }
    } else {
        Err(format!("Failed to get crate dir for: {}", build_crate))
    }
}

pub fn get_crate_dep_dirs(
    build_crate: &str,
    build_dir: &Path,
    target: &str,
) -> HashMap<String, PathBuf> {
    let mut dependencies = HashMap::new();
    let cwd = std::env::current_dir().unwrap();
    let target = format!("--target={target}");
    if let Ok(cargo_tree_output) = shell_env_cap(
        &[],
        &cwd,
        "cargo",
        &["tree", "--color", "never", "-p", build_crate, &target],
    ) {
        for line in cargo_tree_output.lines().skip(1) {
            if let Some((name, path)) = extract_dependency_paths(line) {
                if let Some(path) = path {
                    dependencies.insert(name, path);
                } else {
                    // check in the build dir for .path files, used to find the crate dir of a crates.io crate
                    let dir_file = build_dir.join(format!("{}.path", name));
                    if let Ok(path) = std::fs::read_to_string(&dir_file) {
                        dependencies.insert(name, Path::new(&path).into());
                    }
                }
            }
        }
    }
    dependencies
}

pub fn get_package_binary_name(build_crate: &str) -> Option<String> {
    let crate_dir = get_crate_dir(build_crate).ok()?;
    let cargo_toml = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;

    let mut in_bin = false;
    for raw in cargo_toml.lines() {
        let line = raw.trim();
        if line.starts_with("[[bin]]") {
            in_bin = true;
            continue;
        }
        if line.starts_with('[') {
            in_bin = false;
        }
        if in_bin && line.starts_with("name") {
            if let Some(eq) = line.find('=') {
                let value = line[eq + 1..].trim().trim_matches('"').to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }

    let toml = parse_toml(&cargo_toml).ok()?;
    if let Some(Toml::Str(pkg_name, _)) = toml.get("package.name") {
        return Some(pkg_name.clone());
    }
    None
}

pub fn get_build_crate_from_args(args: &[String]) -> Result<&str, String> {
    if args.is_empty() {
        return Err(
            "Not enough arguments to determine crate. Pass -p <crate> or --package <crate>.".into(),
        );
    }

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-p" || arg == "--package" {
            if i + 1 >= args.len() {
                return Err("Missing crate name after -p/--package".into());
            }
            return Ok(&args[i + 1]);
        }
        if let Some(pkg) = arg.strip_prefix("--package=") {
            if pkg.is_empty() {
                return Err("Missing crate name in --package=<crate>".into());
            }
            return Ok(pkg);
        }
        i += 1;
    }

    if let Some(first_positional) = args.iter().find(|a| !a.starts_with('-')) {
        return Ok(first_positional);
    }

    Err("No build crate specified. Pass -p <crate> or --package <crate>.".into())
}

pub fn get_target_from_args(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--target" {
            if i + 1 < args.len() {
                return Some(args[i + 1].clone());
            }
            return None;
        }
        if let Some(t) = arg.strip_prefix("--target=") {
            if !t.is_empty() {
                return Some(t.to_string());
            }
            return None;
        }
        i += 1;
    }
    None
}

pub fn get_profile_from_args(args: &[String]) -> String {
    for arg in args {
        if let Some(opt) = arg.strip_prefix("--profile=") {
            return opt.to_string();
        }
        if arg == "--release" {
            return "release".to_string();
        }
    }
    return "debug".to_string();
}

#[derive(Debug, Default, Clone)]
pub struct AndroidPackageMetadata {
    pub identifier: Option<String>,
    pub product_name: Option<String>,
    pub package_version: Option<String>,
    pub version_code: Option<VersionCodeStrategy>,
    pub version_name_override: Option<String>,
    pub min_sdk_version: Option<usize>,
}

pub fn read_android_package_metadata(build_crate: &str) -> AndroidPackageMetadata {
    let mut out = AndroidPackageMetadata::default();
    let Ok(crate_dir) = get_crate_dir(build_crate) else {
        return out;
    };
    let Ok(cargo_toml) = std::fs::read_to_string(crate_dir.join("Cargo.toml")) else {
        return out;
    };
    let Ok(toml) = parse_toml(&cargo_toml) else {
        return out;
    };

    if let Some(Toml::Str(value, _)) = toml.get("package.version") {
        out.package_version = Some(value.clone());
    }
    if let Some(Toml::Str(value, _)) = toml.get("package.metadata.packager.identifier") {
        out.identifier = Some(value.clone());
    }
    if let Some(Toml::Str(value, _)) = toml.get("package.metadata.packager.product_name") {
        out.product_name = Some(value.clone());
    }
    match toml.get("package.metadata.makepad.android.version_code") {
        Some(Toml::Num(value, _)) if *value >= 0.0 && *value <= u32::MAX as f64 => {
            out.version_code = Some(VersionCodeStrategy::Explicit(*value as u32));
        }
        Some(Toml::Str(value, _)) if value.eq_ignore_ascii_case("auto") => {
            out.version_code = Some(VersionCodeStrategy::Auto);
        }
        Some(Toml::Str(value, _)) => {
            eprintln!(
                "warning: ignoring [package.metadata.makepad.android].version_code = \"{value}\"; expected a non-negative integer or \"auto\""
            );
        }
        _ => {}
    }
    if let Some(Toml::Str(value, _)) = toml.get("package.metadata.makepad.android.version_name") {
        out.version_name_override = Some(value.clone());
    }
    if let Some(Toml::Num(value, _)) = toml.get("package.metadata.makepad.android.min_sdk_version")
    {
        if *value >= 1.0 && *value <= 100.0 && value.fract() == 0.0 {
            out.min_sdk_version = Some(*value as usize);
        } else {
            eprintln!(
                "warning: ignoring [package.metadata.makepad.android].min_sdk_version = {value}; expected a positive integer API level"
            );
        }
    }
    out
}

#[cfg(test)]
mod android_package_metadata_tests {
    use super::*;

    #[test]
    fn version_code_flag_accepts_int_and_auto() {
        assert!(matches!(
            parse_version_code_flag("42"),
            Ok(VersionCodeStrategy::Explicit(42))
        ));
        assert!(matches!(
            parse_version_code_flag("auto"),
            Ok(VersionCodeStrategy::Auto)
        ));
        assert!(matches!(
            parse_version_code_flag("AUTO"),
            Ok(VersionCodeStrategy::Auto)
        ));
        assert!(parse_version_code_flag("nope").is_err());
    }

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(unix_to_utc_ymdh(0), (1970, 1, 1, 0));
    }

    #[test]
    fn auto_version_code_fits_play_store_cap() {
        let value = generate_auto_version_code();
        assert!(value <= 2_100_000_000);
        assert!(value > 1_900_000_000);
    }

    #[test]
    fn package_metadata_shape_parses_expected_paths() {
        let toml = parse_toml(
            r#"
[package]
name = "demo"
version = "1.2.3"

[package.metadata.packager]
identifier = "dev.makepad.demo"
product_name = "Demo"

[package.metadata.makepad.android]
version_code = "auto"
version_name = "1.2.3-test"
min_sdk_version = 33
"#,
        )
        .expect("parse");

        assert!(matches!(
            toml.get("package.metadata.packager.identifier"),
            Some(Toml::Str(value, _)) if value == "dev.makepad.demo"
        ));
        assert!(matches!(
            toml.get("package.metadata.packager.product_name"),
            Some(Toml::Str(value, _)) if value == "Demo"
        ));
        assert!(matches!(
            toml.get("package.metadata.makepad.android.version_code"),
            Some(Toml::Str(value, _)) if value.eq_ignore_ascii_case("auto")
        ));
        assert!(matches!(
            toml.get("package.metadata.makepad.android.version_name"),
            Some(Toml::Str(value, _)) if value == "1.2.3-test"
        ));
        assert!(matches!(
            toml.get("package.metadata.makepad.android.min_sdk_version"),
            Some(Toml::Num(value, _)) if (*value - 33.0).abs() < f64::EPSILON
        ));
    }
}

pub const APP_ICON_COUNT: usize = 7;
pub const APP_ICON_IDX_512: usize = 4;
pub const APP_ICON_IDX_1024: usize = 5;
pub const APP_ICON_IDX_ICO: usize = 6;

pub type AppIconEnv = [String; APP_ICON_COUNT];

pub const APP_ICON_ENV_VARS: [&str; APP_ICON_COUNT] = [
    "MAKEPAD_APP_ICON_32",
    "MAKEPAD_APP_ICON_64",
    "MAKEPAD_APP_ICON_128",
    "MAKEPAD_APP_ICON_256",
    "MAKEPAD_APP_ICON_512",
    "MAKEPAD_APP_ICON_1024",
    "MAKEPAD_APP_ICON_ICO",
];

pub fn no_icon_requested() -> bool {
    env::var_os("MAKEPAD_NO_ICON").is_some()
}

pub fn set_no_icon_requested(no_icon: bool) {
    if no_icon {
        env::set_var("MAKEPAD_NO_ICON", "1");
    } else {
        env::remove_var("MAKEPAD_NO_ICON");
    }
}

pub fn resolve_app_icon_env(build_crate: &str) -> Result<Option<AppIconEnv>, String> {
    if no_icon_requested() {
        return Ok(None);
    }

    let resources_dir = get_crate_dir(build_crate)?.join("resources");
    let required_paths = [
        resources_dir.join("icon_32.png"),
        resources_dir.join("icon_64.png"),
        resources_dir.join("icon_128.png"),
        resources_dir.join("icon.ico"),
    ];

    if !required_paths.iter().all(|p| p.is_file()) {
        eprintln!(
            "warning: missing custom app icons in {}. Add icon_32.png, icon_64.png, icon_128.png, and icon.ico, or pass --no-icon to suppress this check.",
            resources_dir.display()
        );
        return Ok(None);
    }

    let optional = |name: &str, fallback: &Path| {
        let path = resources_dir.join(name);
        if path.is_file() {
            path
        } else {
            fallback.to_path_buf()
        }
    };

    let icon_256 = optional("icon_256.png", &required_paths[2]);
    let icon_512 = optional("icon_512.png", &icon_256);
    let icon_1024 = optional("icon_1024.png", &icon_512);

    Ok(Some([
        required_paths[0].to_string_lossy().to_string(),
        required_paths[1].to_string_lossy().to_string(),
        required_paths[2].to_string_lossy().to_string(),
        icon_256.to_string_lossy().to_string(),
        icon_512.to_string_lossy().to_string(),
        icon_1024.to_string_lossy().to_string(),
        required_paths[3].to_string_lossy().to_string(),
    ]))
}
