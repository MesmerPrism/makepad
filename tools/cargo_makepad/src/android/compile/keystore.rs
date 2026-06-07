use super::toolchain::{keytool_path, resolve_java_home};
use crate::android::HostOs;
use crate::makepad_shell::{mkdir, shell_env};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn keystore_sidecar_path(keystore: &Path) -> PathBuf {
    let mut name = keystore
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".makepad");
    keystore.with_file_name(name)
}

#[derive(Debug, Default)]
pub struct KeystoreSidecar {
    pub alias: Option<String>,
    pub store_type: Option<String>,
}

pub fn read_keystore_sidecar(keystore: &Path) -> Option<KeystoreSidecar> {
    let path = keystore_sidecar_path(keystore);
    let text = fs::read_to_string(&path).ok()?;
    let mut sidecar = KeystoreSidecar::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "alias" => sidecar.alias = Some(value),
            "store_type" => sidecar.store_type = Some(value),
            _ => {}
        }
    }
    Some(sidecar)
}

fn write_keystore_sidecar(keystore: &Path, sidecar: &KeystoreSidecar) -> Result<(), String> {
    let path = keystore_sidecar_path(keystore);
    let mut body = String::from(
        "# Keystore metadata written by `cargo makepad android keystore-create`.\n\
         # Safe to commit: contains no passwords. `cargo makepad android build-aab` reads this\n\
         # next to the keystore so you only need to pass --keystore + password on each build.\n",
    );
    if let Some(alias) = &sidecar.alias {
        body.push_str(&format!("alias = {alias}\n"));
    }
    if let Some(store_type) = &sidecar.store_type {
        body.push_str(&format!("store_type = {store_type}\n"));
    }
    fs::write(&path, body).map_err(|e| format!("Cant write {:?}: {e}", path))
}

pub struct KeystoreCreateOpts {
    pub keystore_path: PathBuf,
    pub alias: String,
    pub validity_days: u32,
    pub key_size: u32,
    pub key_alg: String,
    pub store_type: String,
    pub dname: Option<String>,
}

pub fn keystore_create(
    sdk_dir: &Path,
    host_os: HostOs,
    opts: &KeystoreCreateOpts,
) -> Result<(), String> {
    if opts.keystore_path.exists() {
        return Err(format!(
            "Refusing to overwrite existing file {:?}. Pick a new path or delete the existing keystore first.",
            opts.keystore_path
        ));
    }
    let keytool = keytool_path(sdk_dir, host_os);
    if !keytool.is_file() {
        return Err(format!(
            "keytool not found at {:?}. Run `cargo makepad android install-toolchain` or set JAVA_HOME to a full JDK.",
            keytool
        ));
    }
    if let Some(parent) = opts.keystore_path.parent() {
        if !parent.as_os_str().is_empty() {
            mkdir(parent)?;
        }
    }

    println!("================================================================================");
    println!("CREATING ANDROID UPLOAD KEYSTORE");
    println!("================================================================================");
    println!();
    println!("This keystore is your upload key. Back up the file and password.");
    println!("Do not commit the keystore to a public repository.");
    println!();

    let java_home = resolve_java_home(sdk_dir, host_os);
    let cwd = std::env::current_dir().unwrap();
    let validity = opts.validity_days.to_string();
    let keysize = opts.key_size.to_string();
    let keystore_str = opts.keystore_path.to_string_lossy().to_string();

    let mut args: Vec<String> = vec![
        "-genkeypair".to_string(),
        "-v".to_string(),
        "-keystore".to_string(),
        keystore_str,
        "-alias".to_string(),
        opts.alias.clone(),
        "-keyalg".to_string(),
        opts.key_alg.clone(),
        "-keysize".to_string(),
        keysize,
        "-validity".to_string(),
        validity,
        "-storetype".to_string(),
        opts.store_type.clone(),
    ];
    if let Some(dname) = &opts.dname {
        args.push("-dname".to_string());
        args.push(dname.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    shell_env(
        &[("JAVA_HOME", java_home.to_str().unwrap())],
        &cwd,
        keytool.to_str().unwrap(),
        &arg_refs,
    )?;

    write_keystore_sidecar(
        &opts.keystore_path,
        &KeystoreSidecar {
            alias: Some(opts.alias.clone()),
            store_type: Some(opts.store_type.clone()),
        },
    )?;

    println!("Keystore created: {}", opts.keystore_path.display());
    println!(
        "Metadata sidecar: {}",
        keystore_sidecar_path(&opts.keystore_path).display()
    );
    Ok(())
}
