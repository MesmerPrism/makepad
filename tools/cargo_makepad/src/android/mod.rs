mod compile;
mod sdk;

#[derive(Clone, Copy, PartialEq)]
pub enum HostOs {
    WindowsX64,
    MacosX64,
    MacosAarch64,
    LinuxX64,
    Unsupported,
}

#[derive(Clone, Copy, PartialEq)]
pub enum AndroidVariant {
    Default,
    Quest,
}

#[derive(Clone, Debug, Default)]
pub struct AndroidConfig {
    pub small_fonts: bool,
    pub screen_orientation: Option<String>,
    pub resizeable_activity: Option<bool>,
    pub quest_camera_permissions: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ManifestArgs<'a> {
    pub label: &'a str,
    pub class_name: &'a str,
    pub url: &'a str,
    pub sdk_version: usize,
    pub target_sdk_version: usize,
    pub has_icon: bool,
    pub version_code: u32,
    pub version_name: &'a str,
    pub debuggable: bool,
    pub screen_orientation: Option<&'a str>,
    pub resizeable_activity: Option<bool>,
    pub quest_camera_permissions: bool,
}

fn parse_android_bool_flag(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("{name} must be true or false, got {value:?}")),
    }
}

fn validate_manifest_attr_value(name: &str, value: &str) -> Result<String, String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(format!(
            "{name} must be a non-empty Android manifest token, got {value:?}"
        ));
    }
    Ok(value.to_string())
}

impl AndroidVariant {
    fn from_str(opt: &str) -> Result<Self, String> {
        for opt in opt.split(",") {
            match opt {
                "default" => return Ok(AndroidVariant::Default),
                "quest" => return Ok(AndroidVariant::Quest),
                _ => (),
            }
        }
        return Err(format!(
            "please provide a valid android variant: default, quest"
        ));
    }

    fn manifest_xml(&self, args: &ManifestArgs<'_>) -> String {
        let ManifestArgs {
            label,
            class_name,
            url,
            sdk_version,
            target_sdk_version,
            has_icon,
            version_code,
            version_name,
            debuggable,
            screen_orientation,
            resizeable_activity,
            quest_camera_permissions,
        } = args;
        let icon_attr = if *has_icon {
            "\n                    android:icon=\"@mipmap/ic_launcher\""
        } else {
            ""
        };
        let debuggable_str = if *debuggable { "true" } else { "false" };
        let default_screen_orientation_attr = screen_orientation
            .map(|value| format!("\n                    android:screenOrientation=\"{value}\""))
            .unwrap_or_default();
        let default_resizeable_activity_attr = resizeable_activity
            .map(|value| {
                format!(
                    "\n                    android:resizeableActivity=\"{}\"",
                    if value { "true" } else { "false" }
                )
            })
            .unwrap_or_default();
        let quest_screen_orientation = screen_orientation.unwrap_or("landscape");
        let quest_resizeable_activity = if resizeable_activity.unwrap_or(false) {
            "true"
        } else {
            "false"
        };
        let quest_camera_declarations = if *quest_camera_permissions {
            r#"
                <uses-feature android:name="android.hardware.camera" android:required="false"/>
                <uses-feature android:name="android.hardware.camera2.full" android:required="false"/>
                <uses-permission android:name="android.permission.CAMERA"/>
                <uses-permission android:name="horizonos.permission.HEADSET_CAMERA" />
                <uses-permission android:name="horizonos.permission.SPATIAL_CAMERA" />"#
        } else {
            ""
        };

        match self {
            Self::Default => format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
                <manifest xmlns:android="http://schemas.android.com/apk/res/android"
                xmlns:tools="http://schemas.android.com/tools"
                package="{url}"
                android:versionCode="{version_code}"
                android:versionName="{version_name}">
                <application
                    android:label="{label}"{icon_attr}
                    android:theme="@style/MakepadAppTheme"
                    android:allowBackup="true"
                    android:supportsRtl="true"
                    android:debuggable="{debuggable_str}"
                    android:largeHeap="true"
                    tools:targetApi="{target_sdk_version}">
                    <meta-data android:name="android.max_aspect" android:value="2.1" />
                    <activity
                    android:name=".{class_name}"
                    android:configChanges="orientation|screenSize|keyboardHidden"
                    android:exported="true"
                    android:launchMode="singleTask"{default_resizeable_activity_attr}{default_screen_orientation_attr}
                    android:windowSoftInputMode="adjustNothing|stateUnchanged"
                    android:theme="@style/MakepadLaunchTheme">
                    <intent-filter>
                        <action android:name="android.intent.action.MAIN" />
                        <category android:name="android.intent.category.LAUNCHER" />
                    </intent-filter>
                    </activity>
                </application>
                <uses-sdk android:minSdkVersion="{sdk_version}" android:targetSdkVersion="{target_sdk_version}" />
                <uses-feature android:glEsVersion="0x00020000" android:required="true"/>
                <uses-feature android:name="android.hardware.bluetooth_le" android:required="true"/>
                <uses-feature android:name="android.software.midi" android:required="true"/>
                <uses-permission android:name="android.permission.READ_EXTERNAL_STORAGE" />
                <uses-permission android:name="android.permission.READ_MEDIA_VIDEO"  />
                <uses-permission android:name="android.permission.READ_MEDIA_IMAGES"  />
                <uses-permission android:name="android.permission.INTERNET" />
                <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
                <uses-permission android:name="android.permission.BLUETOOTH"/>
                <uses-permission android:name="android.permission.BLUETOOTH_CONNECT"/>
                <uses-permission android:name="android.permission.CAMERA"/>
                <uses-permission android:name="android.permission.RECORD_AUDIO"/>
                <uses-permission android:name="android.permission.MODIFY_AUDIO_SETTINGS"/>
                <uses-permission android:name="android.permission.ACCESS_COARSE_LOCATION"/>
                <uses-permission android:name="android.permission.ACCESS_FINE_LOCATION"/>
                <uses-permission android:name="android.permission.USE_BIOMETRIC" />
                <uses-permission android:name="android.permission.QUERY_ALL_PACKAGES" tools:ignore="QueryAllPackagesPermission" />
                            
                <queries>
                <intent>
                <action android:name="android.intent.action.MAIN" />
                </intent>
                </queries>
                </manifest>
                "#
            ),
            Self::Quest => format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
                <manifest
                    xmlns:android="http://schemas.android.com/apk/res/android"
                    xmlns:tools="http://schemas.android.com/tools"
                    package="{url}"
                    android:versionCode="{version_code}"
                    android:versionName="{version_name}"
                    android:installLocation="auto"
                >      
                                                                
                <uses-sdk android:minSdkVersion="{sdk_version}" android:targetSdkVersion="{target_sdk_version}" />
                <uses-feature android:glEsVersion="0x00030001" android:required="true"/>
                <uses-feature android:name="android.hardware.vr.headtracking" android:version="1" android:required="true"/>
                <uses-feature android:name="com.oculus.feature.PASSTHROUGH" android:required="true"/>
                <uses-feature android:name="com.oculus.feature.CONTEXTUAL_BOUNDARYLESS_APP" android:required="false"/>
                {quest_camera_declarations}
                <uses-permission android:name="com.oculus.permission.USE_SCENE" />
                <!-- Request hand and keyboard tracking for keyboard hand presence testing -->
                <uses-feature android:name="oculus.software.handtracking" android:required="false"/>
                <uses-permission android:name="com.oculus.permission.HAND_TRACKING" />
                <uses-permission android:name="android.permission.INTERNET" />
                <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
                <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
                <uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION" />
                <uses-permission android:name="android.permission.RECORD_AUDIO"/>
                <uses-permission android:name="android.permission.MODIFY_AUDIO_SETTINGS"/>
                <uses-permission android:name="org.khronos.openxr.permission.OPENXR" />
                <uses-permission android:name="org.khronos.openxr.permission.OPENXR_SYSTEM" />
                <uses-permission android:name="com.oculus.permission.USE_ANCHOR_API" />
                <!-- Grants access to Shared Spatial Anchors. -->
                <uses-permission android:name="com.oculus.permission.IMPORT_EXPORT_IOT_MAP_DATA" />
                <uses-permission android:name="com.oculus.permission.USE_COLOCATION_DISCOVERY_API" />
                
                <application
                    android:label="{label}"{icon_attr}
                    android:theme="@style/MakepadAppTheme"
                    android:allowBackup="false"
                    android:extractNativeLibs="true"
                    android:supportsRtl="true"
                    android:debuggable="{debuggable_str}"
                    android:largeHeap="true"
                    tools:targetApi="{target_sdk_version}">
                    <meta-data
                        android:name="com.samsung.android.vr.application.mode"
                        android:value="vr_only" />
                    <!-- Quest 3-only CPU/GPU trade: prefer one extra CPU level over one GPU level. -->
                    <meta-data
                        android:name="com.oculus.trade_cpu_for_gpu_amount"
                        android:value="-1" />
                    <activity
                        android:name=".{class_name}"
                        android:configChanges="screenSize|screenLayout|orientation|keyboardHidden|keyboard|navigation|uiMode"
                        android:excludeFromRecents="false"
                        android:exported="true"
                        android:hardwareAccelerated="false"
                        android:launchMode="singleTask"
                        android:resizeableActivity="{quest_resizeable_activity}"
                        android:screenOrientation="{quest_screen_orientation}"
                        android:windowSoftInputMode="adjustNothing|stateUnchanged"
                        android:theme="@style/MakepadLaunchTheme" 
                        >
                        <intent-filter>
                            <action android:name="android.intent.action.MAIN" />
                            <category android:name="android.intent.category.LAUNCHER" />
                        </intent-filter>
                        </activity>
                                                
                    <activity
                        android:name="{class_name}Xr"
                        android:configChanges="screenSize|screenLayout|orientation|keyboardHidden|keyboard|navigation|uiMode"
                        android:excludeFromRecents="true"
                        android:exported="true"
                        android:hardwareAccelerated="false"
                        android:launchMode="singleTask"
                        android:resizeableActivity="{quest_resizeable_activity}"
                        android:screenOrientation="{quest_screen_orientation}"
                        android:windowSoftInputMode="adjustNothing|stateUnchanged"
                        android:theme="@style/MakepadLaunchTheme" 
                        >
                        <meta-data
                            android:name="com.oculus.vr.focusaware"
                            android:value="true" />
                        <meta-data
                            android:name="com.oculus.intent.category.VR"
                            android:value="vr_only" />
                        <intent-filter>
                            <action android:name="android.intent.action.MAIN" />
                            <category android:name="com.oculus.intent.category.VR" />
                        </intent-filter>
                    </activity>

                    <service
                        android:name="dev.makepad.android.MediaProjectionStreamService"
                        android:exported="false"
                        android:foregroundServiceType="mediaProjection" />
                </application>
                                                                                    
                <queries>
                <!-- to talk to the broker -->
                    <provider 
                    android:name="x" android:authorities="org.khronos.openxr.runtime_broker;org.khronos.openxr.system_runtime_broker" />
                                                                                                
                <!-- so client-side code of runtime/layers can talk to their service sides -->
                <intent>
                <action android:name="org.khronos.openxr.OpenXRRuntimeService" />
                </intent>
                <intent>
                <action android:name="org.khronos.openxr.OpenXRApiLayerService" />
                </intent>
                <intent>
                <action android:name="android.intent.action.MAIN" />
                </intent>
                </queries>
                                            
                </manifest>
                "#
            ),
        }
    }
}

/*
Self::Quest=>format!(r#"<?xml version="1.0" encoding="utf-8"?>
    <manifest
    xmlns:android="http://schemas.android.com/apk/res/android"
    package="{url}"
    android:versionCode="1"
    android:versionName="1.0"
    android:installLocation="auto"
    >


    <uses-sdk android:targetSdkVersion="{sdk_version}" />
    <uses-feature android:glEsVersion="0x00030001" android:required="true"/>
    <uses-feature android:name="android.hardware.vr.headtracking" android:required="false"/>
    <uses-feature android:name="com.oculus.feature.PASSTHROUGH" android:required="true"/>
    <uses-permission android:name="com.oculus.permission.USE_SCENE" />
    <!-- Request hand and keyboard tracking for keyboard hand presence testing -->
    <uses-feature android:name="oculus.software.handtracking" android:required="false"/>
    <uses-permission android:name="com.oculus.permission.HAND_TRACKING" />
    <uses-permission android:name="android.permission.INTERNET" />
    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
    <uses-permission android:name="org.khronos.openxr.permission.OPENXR" />
    <uses-permission android:name="org.khronos.openxr.permission.OPENXR_SYSTEM" />

    <application
    android:label="{label}"
    android:allowBackup="false"
    android:debuggable="true"
    >
    //
    <activity
    android:name="{class_name}"
    android:theme="@android:style/Theme.Black.NoTitleBar.Fullscreen"
    android:launchMode="singleTask"
    android:screenOrientation="landscape"
    android:excludeFromRecents="false"
    android:configChanges="screenSize|screenLayout|orientation|keyboardHidden|keyboard|navigation|uiMode"
    android:exported="true"
    >
    <intent-filter>
    <action android:name="android.intent.action.MAIN" />
    <action android:name="android.intent.action.LAUNCHER" />
    <action android:name="android.intent.action.VR" />
    </intent-filter>
    </activity>

    <activity
    android:name="{class_name}.MakepadAppXr"
    android:configChanges="screenSize|screenLayout|orientation|keyboardHidden|keyboard|navigation|uiMode"
    android:excludeFromRecents="false"
    android:exported="true"
    android:launchMode="singleTask"
    android:screenOrientation="landscape"
    android:theme="@android:style/Theme.Black.NoTitleBar.Fullscreen"
    >
    <intent-filter>
    <action android:name="android.intent.action.MAIN" />
    <category android:name="com.oculus.intent.category.VR" />
    </intent-filter>
    </activity>
    </application>

    <queries>
    <!-- to talk to the broker -->
    <provider
    android:name="x" android:authorities="org.khronos.openxr.runtime_broker;org.khronos.openxr.system_runtime_broker" />

    <!-- so client-side code of runtime/layers can talk to their service sides -->
    <intent>
    <action android:name="org.khronos.openxr.OpenXRRuntimeService" />
    </intent>
    <intent>
    <action android:name="org.khronos.openxr.OpenXRApiLayerService" />
    </intent>
    <intent>
    <action android:name="android.intent.action.MAIN" />
    </intent>
    </queries>

    </manifest>
    "#)*/

#[allow(non_camel_case_types)]
pub enum AndroidTarget {
    aarch64,
    x86_64,
    armv7,
    i686,
}

impl AndroidTarget {
    fn from_str(opt: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for opt in opt.split(",") {
            match opt {
                "all" => {
                    return Ok(vec![
                        AndroidTarget::aarch64,
                        AndroidTarget::x86_64,
                        AndroidTarget::armv7,
                        AndroidTarget::i686,
                    ])
                }
                "aarch64" => out.push(AndroidTarget::aarch64),
                "x86_64" => out.push(AndroidTarget::x86_64),
                "armv7" => out.push(AndroidTarget::armv7),
                "i686" => out.push(AndroidTarget::i686),
                x => {
                    return Err(format!(
                        "{:?} please provide a valid ABI: aarch64, x86_64, armv7, i686",
                        x
                    ))
                }
            }
        }
        return Ok(out);
    }
    fn _sys_dir(&self) -> &'static str {
        match self {
            Self::aarch64 => "aarch64-linux-android",
            Self::x86_64 => "x86_64-linux-android",
            Self::armv7 => "arm-linux-androideabi",
            Self::i686 => "i686-linux-android",
        }
    }
    fn _unwind_dir(&self) -> &'static str {
        match self {
            Self::aarch64 => "aarch64",
            Self::x86_64 => "x86_64",
            Self::armv7 => "arm",
            Self::i686 => "i386",
        }
    }

    fn clang(&self) -> &'static str {
        match self {
            Self::aarch64 => "aarch64-linux-android",
            Self::x86_64 => "x86_64-linux-android",
            Self::armv7 => "armv7a-linux-androideabi",
            Self::i686 => "i686-linux-android",
        }
    }
    fn toolchain(&self) -> &'static str {
        match self {
            Self::aarch64 => "aarch64-linux-android",
            Self::x86_64 => "x86_64-linux-android",
            Self::armv7 => "armv7-linux-androideabi",
            Self::i686 => "i686-linux-android",
        }
    }
    fn to_str(&self) -> &'static str {
        match self {
            Self::aarch64 => "aarch64",
            Self::x86_64 => "x86_64",
            Self::armv7 => "armv7",
            Self::i686 => "i686",
        }
    }
    fn abi_identifier(&self) -> &'static str {
        match self {
            Self::aarch64 => "arm64-v8a",
            Self::x86_64 => "x86_64",
            Self::armv7 => "armeabi-v7a",
            Self::i686 => "x86",
        }
    }
    fn linker_env_var(&self) -> &'static str {
        match self {
            Self::aarch64 => "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER",
            Self::x86_64 => "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER",
            Self::armv7 => "CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER",
            Self::i686 => "CARGO_TARGET_I686_LINUX_ANDROID_LINKER",
        }
    }
}

impl HostOs {
    fn from_str(opt: &str) -> Result<Self, String> {
        match opt {
            "windows-x64" => Ok(HostOs::WindowsX64),
            "macos-x64" => Ok(HostOs::MacosX64),
            "macos-aarch64" => Ok(HostOs::MacosAarch64),
            "linux-x64" => Ok(HostOs::LinuxX64),
            x => {
                Err(format!("{:?} please provide a valid host-os: windows-x64,macos-x64,macos-aarch64,linux-x64", x))
            }
        }
    }

    fn default_path(&self) -> &'static str {
        match self {
            Self::WindowsX64 => "./android_33_windows_x64",
            Self::MacosX64 => "./android_33_macos_x64",
            Self::MacosAarch64 => "./android_33_macos_aarch64",
            Self::LinuxX64 => "./android_33_linux_x64",
            Self::Unsupported => panic!(),
        }
    }
}

fn android_help() -> &'static str {
    "Android commands:\n\
  cargo makepad android [options] install-toolchain\n\
  cargo makepad android [options] build <cargo args>\n\
  cargo makepad android [options] build-aab <cargo args>\n\
  cargo makepad android keystore-create <path> [keytool options]\n\
  cargo makepad android [options] run <cargo args>\n\
  cargo makepad android [options] adb <adb args>\n\
  cargo makepad android [options] adb-tcp [port]\n\
\n\
Common options:\n\
  --abi=aarch64|x86_64|armv7|i686|all   (default: aarch64)\n\
  --package-name=<id>\n\
  --app-label=<label>\n\
  --version-code=<int|auto>\n\
  --version-name=<str>\n\
  --min-sdk-version=<api>\n\
  --small-fonts\n\
  --screen-orientation=<value>            Adds android:screenOrientation to the generated launcher activity\n\
  --resizeable-activity=true|false        Adds android:resizeableActivity to the generated launcher activity\n\
  --quest-camera-permissions=true|false   Quest only: include Android/headset/spatial camera declarations (default true)\n\
  --no-icon\n\
  --sdk-path=<path>\n\
  --host-os=linux-x64|windows-x64|macos-aarch64|macos-x64\n\
  --variant=default|quest\n\
  --devices=<serial1,serial2,...>|all    (for run and adb-tcp)\n\
  --keep-sdk-sources\n\
  --keystore=<path>                       build-aab signing keystore\n\
  --keystore-pass=<pass>                  build-aab keystore password, or MAKEPAD_KEYSTORE_PASS\n\
  --keystore-key-alias=<alias>            build-aab key alias\n\
  --keystore-key-pass=<pass>              build-aab key password, defaults to keystore password\n\
  --no-sign                               build-aab only: emit unsigned bundle\n\
\n\
Custom AndroidManifest:\n\
  Drop a template at `<crate>/resources/android/AndroidManifest.xml.template` to\n\
  override the built-in manifest. Tokens replaced: {package_id}, {label},\n\
  {class_name}, {min_sdk_version}, {target_sdk_version}, {version_code},\n\
  {version_name}, {debuggable}, {screen_orientation}, {resizeable_activity}.\n\
\n\
Examples:\n\
  cargo makepad android --abi=aarch64 build -p my-app --release\n\
  cargo makepad android --abi=aarch64 build-aab -p my-app --release\n\
  cargo makepad android keystore-create my-app.keystore\n\
  cargo makepad android --abi=aarch64 run -p my-app --release\n\
  cargo makepad android --devices=all --variant=quest run -p my-app --release\n\
  cargo makepad android adb devices -l\n\
  cargo makepad android adb-tcp\n\
  cargo makepad android --devices=<serial> adb-tcp 5555"
}

fn resolve_aab_signing_opts(
    keystore: Option<String>,
    keystore_pass: Option<String>,
    key_alias: Option<String>,
    key_pass: Option<String>,
) -> Result<compile::AabSigningOpts, String> {
    let cargo_manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let any_keystore_opt =
        keystore.is_some() || keystore_pass.is_some() || key_alias.is_some() || key_pass.is_some();

    if !any_keystore_opt {
        return Ok(compile::AabSigningOpts {
            keystore: cargo_manifest_dir.join("debug.keystore"),
            storepass: "android".to_string(),
            key_alias: "androiddebugkey".to_string(),
            keypass: "android".to_string(),
        });
    }

    let keystore_path = std::path::PathBuf::from(keystore.ok_or_else(|| {
        "--keystore=<path> is required when any other keystore option is set".to_string()
    })?);
    let sidecar = compile::read_keystore_sidecar(&keystore_path);
    let storepass = keystore_pass
        .or_else(|| std::env::var("MAKEPAD_KEYSTORE_PASS").ok())
        .ok_or_else(|| {
            "no keystore password provided. Pass --keystore-pass=<pw> or set MAKEPAD_KEYSTORE_PASS"
                .to_string()
        })?;
    let alias = key_alias
        .or_else(|| sidecar.as_ref().and_then(|s| s.alias.clone()))
        .ok_or_else(|| {
            format!(
                "no key alias found. Pass --keystore-key-alias=<alias>, or run `cargo makepad android keystore-create {}`.",
                keystore_path.display()
            )
        })?;
    let keypass = key_pass.unwrap_or_else(|| storepass.clone());

    Ok(compile::AabSigningOpts {
        keystore: keystore_path,
        storepass,
        key_alias: alias,
        keypass,
    })
}

fn parse_keystore_create_args(args: &[String]) -> Result<compile::KeystoreCreateOpts, String> {
    let mut keystore_path: Option<String> = None;
    let mut alias: String = "upload".to_string();
    let mut validity_days: u32 = 10_000;
    let mut key_size: u32 = 2048;
    let mut key_alg: String = "RSA".to_string();
    let mut store_type: String = "PKCS12".to_string();
    let mut dname: Option<String> = None;

    for arg in args {
        if let Some(v) = arg.strip_prefix("--alias=") {
            alias = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--validity=") {
            validity_days = v
                .parse::<u32>()
                .map_err(|_| format!("--validity must be a positive integer, got {v:?}"))?;
        } else if let Some(v) = arg.strip_prefix("--keysize=") {
            key_size = v
                .parse::<u32>()
                .map_err(|_| format!("--keysize must be a positive integer, got {v:?}"))?;
        } else if let Some(v) = arg.strip_prefix("--keyalg=") {
            key_alg = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--storetype=") {
            store_type = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--dname=") {
            dname = Some(v.to_string());
        } else if arg == "--help" || arg == "-h" {
            return Err(keystore_create_help().to_string());
        } else if arg.starts_with("--") {
            return Err(format!(
                "unknown option {arg:?}\n\n{}",
                keystore_create_help()
            ));
        } else if keystore_path.is_none() {
            keystore_path = Some(arg.clone());
        } else {
            return Err(format!(
                "unexpected positional argument {arg:?}\n\n{}",
                keystore_create_help()
            ));
        }
    }

    let keystore_path = keystore_path.ok_or_else(|| {
        format!(
            "missing required <keystore-path> argument\n\n{}",
            keystore_create_help()
        )
    })?;

    Ok(compile::KeystoreCreateOpts {
        keystore_path: std::path::PathBuf::from(keystore_path),
        alias,
        validity_days,
        key_size,
        key_alg,
        store_type,
        dname,
    })
}

fn keystore_create_help() -> &'static str {
    "Usage:\n\
  cargo makepad android keystore-create <keystore-path> [options]\n\
\n\
Options:\n\
  --alias=<alias>       key alias inside the keystore (default: upload)\n\
  --validity=<days>     certificate validity in days (default: 10000)\n\
  --keyalg=<alg>        signing algorithm (default: RSA)\n\
  --keysize=<bits>      key size in bits (default: 2048)\n\
  --storetype=<type>    keystore type (default: PKCS12)\n\
  --dname=<dn>          non-interactive certificate subject"
}

fn resolve_devices_arg(
    sdk_dir: &std::path::Path,
    devices: &[String],
) -> Result<Vec<String>, String> {
    if !devices.iter().any(|device| device == "all") {
        return Ok(devices.to_vec());
    }
    if devices.len() != 1 {
        return Err("`--devices=all` cannot be combined with explicit device serials".to_string());
    }
    let devices = compile::list_connected_devices(sdk_dir)?;
    if devices.is_empty() {
        return Err("`--devices=all` found no connected adb devices".to_string());
    }
    Ok(devices)
}

pub fn handle_android(mut args: &[String]) -> Result<(), String> {
    #[allow(unused)]
    let mut host_os = HostOs::Unsupported;
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let mut host_os = HostOs::WindowsX64;
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    let mut host_os = HostOs::MacosX64;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let mut host_os = HostOs::MacosAarch64;
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let mut host_os = HostOs::LinuxX64;
    let mut sdk_path = None;
    let mut package_name = None;
    let mut app_label = None;
    let mut version_code = None;
    let mut version_name = None;
    let mut min_sdk_version = None;
    let mut devices = Vec::new();
    let mut variant = AndroidVariant::Default;
    let mut targets = vec![AndroidTarget::aarch64];
    let mut keep_sdk_sources = false;
    let mut no_icon = false;
    let mut config = AndroidConfig::default();
    let mut keystore: Option<String> = None;
    let mut keystore_pass: Option<String> = None;
    let mut keystore_key_alias: Option<String> = None;
    let mut keystore_key_pass: Option<String> = None;
    let mut no_sign = false;

    let urls = sdk::ANDROID_SDK_URLS_33;

    // pull out options
    for i in 0..args.len() {
        let v = &args[i];
        if let Some(opt) = v.strip_prefix("--host-os=") {
            host_os = HostOs::from_str(opt)?;
        } else if let Some(opt) = v.strip_prefix("--sdk-path=") {
            sdk_path = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--package-name=") {
            package_name = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--app-label=") {
            app_label = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--version-code=") {
            version_code = Some(crate::utils::parse_version_code_flag(opt)?);
        } else if let Some(opt) = v.strip_prefix("--version-name=") {
            version_name = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--min-sdk-version=") {
            min_sdk_version = Some(opt.parse::<usize>().map_err(|_| {
                format!("--min-sdk-version must be a positive integer API level, got {opt:?}")
            })?);
        } else if let Some(opt) = v.strip_prefix("--abi=") {
            targets = AndroidTarget::from_str(opt)?;
        } else if let Some(d) = v.strip_prefix("--devices=") {
            devices = d.split(",").map(|v| v.to_string()).collect()
        } else if let Some(opt) = v.strip_prefix("--variant=") {
            variant = AndroidVariant::from_str(opt)?;
        } else if v.trim() == "--small-fonts" {
            config.small_fonts = true;
        } else if let Some(opt) = v.strip_prefix("--screen-orientation=") {
            config.screen_orientation =
                Some(validate_manifest_attr_value("--screen-orientation", opt)?);
        } else if let Some(opt) = v.strip_prefix("--resizeable-activity=") {
            config.resizeable_activity =
                Some(parse_android_bool_flag("--resizeable-activity", opt)?);
        } else if let Some(opt) = v.strip_prefix("--quest-camera-permissions=") {
            config.quest_camera_permissions =
                Some(parse_android_bool_flag("--quest-camera-permissions", opt)?);
        } else if v.trim() == "--no-icon" {
            no_icon = true;
        } else if v.trim() == "--keep-sdk-sources" {
            keep_sdk_sources = true;
        } else if let Some(opt) = v.strip_prefix("--keystore=") {
            keystore = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--keystore-pass=") {
            keystore_pass = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--keystore-key-alias=") {
            keystore_key_alias = Some(opt.to_string());
        } else if let Some(opt) = v.strip_prefix("--keystore-key-pass=") {
            keystore_key_pass = Some(opt.to_string());
        } else if v.trim() == "--no-sign" {
            no_sign = true;
        } else {
            args = &args[i..];
            break;
        }
    }

    if args.is_empty() {
        return Err(format!(
            "missing android subcommand. use one of: install-toolchain, build, build-aab, keystore-create, run, adb, adb-tcp\n\n{}",
            android_help()
        ));
    }

    if args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        println!("{}", android_help());
        return Ok(());
    }

    if sdk_path.is_none() {
        sdk_path = Some(format!(
            "{}/{}",
            env!("CARGO_MANIFEST_DIR"),
            host_os.default_path().to_string()
        ));
    }

    let cwd = std::env::current_dir().unwrap();
    let sdk_dir = cwd.join(sdk_path.unwrap());
    crate::utils::set_no_icon_requested(no_icon);

    match args[0].as_ref() {
        "adb" => compile::adb(&sdk_dir, host_os, &args[1..]),
        "adb-tcp" => {
            let devices = resolve_devices_arg(&sdk_dir, &devices)?;
            compile::adb_tcp(&sdk_dir, host_os, &devices, &args[1..])
        }
        "java" => compile::java(&sdk_dir, host_os, &args[1..]),
        "javac" => compile::javac(&sdk_dir, host_os, &args[1..]),
        "rustup-toolchain-install" | "rustup-install-toolchain" => {
            sdk::rustup_toolchain_install(&targets)
        }
        "download-sdk" => sdk::download_sdk(&sdk_dir, host_os, &args[1..], &urls),
        "expand-sdk" => sdk::expand_sdk(&sdk_dir, host_os, &args[1..], &targets, &urls),
        "remove-sdk-sources" => sdk::remove_sdk_sources(&sdk_dir, host_os, &args[1..]),
        "toolchain-install" | "install-toolchain" => {
            println!("Installing Android toolchain\n");
            sdk::rustup_toolchain_install(&targets)?;
            sdk::download_sdk(&sdk_dir, host_os, &args[1..], &urls)?;
            sdk::expand_sdk(&sdk_dir, host_os, &args[1..], &targets, &urls)?;
            if !keep_sdk_sources {
                sdk::remove_sdk_sources(&sdk_dir, host_os, &args[1..])?;
            }
            println!("\nAndroid toolchain has been installed\n");
            Ok(())
        }
        "keystore-create" => {
            let opts = parse_keystore_create_args(&args[1..])?;
            compile::keystore_create(&sdk_dir, host_os, &opts)
        }
        /*"base-apk"=>{
            compile::base_apk(&sdk_dir, host_os, &args[1..])
        }*/
        "build" => {
            compile::build(
                &sdk_dir,
                host_os,
                package_name,
                app_label,
                version_code,
                version_name,
                min_sdk_version,
                &args[1..],
                &targets,
                &variant,
                &config,
                &urls,
            )?;
            Ok(())
        }
        "build-aab" => {
            let signing = if no_sign {
                None
            } else {
                Some(resolve_aab_signing_opts(
                    keystore,
                    keystore_pass,
                    keystore_key_alias,
                    keystore_key_pass,
                )?)
            };
            compile::build_aab(
                &sdk_dir,
                host_os,
                package_name,
                app_label,
                version_code,
                version_name,
                min_sdk_version,
                &args[1..],
                &targets,
                &variant,
                &config,
                &urls,
                signing,
            )?;
            Ok(())
        }
        "run" => {
            let devices = resolve_devices_arg(&sdk_dir, &devices)?;
            compile::run(
                &sdk_dir,
                host_os,
                package_name,
                app_label,
                version_code,
                version_name,
                min_sdk_version,
                &args[1..],
                &targets,
                &variant,
                &config,
                &urls,
                devices,
            )
        }
        _ => Err(format!(
            "{} is not a valid android subcommand\n\n{}",
            args[0],
            android_help()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{AndroidVariant, ManifestArgs};

    fn test_manifest_args<'a>(label: &'a str, url: &'a str) -> ManifestArgs<'a> {
        ManifestArgs {
            label,
            class_name: "MakepadApp",
            url,
            sdk_version: 26,
            target_sdk_version: 35,
            has_icon: true,
            version_code: 7,
            version_name: "1.2.3",
            debuggable: true,
            screen_orientation: None,
            resizeable_activity: None,
            quest_camera_permissions: true,
        }
    }

    #[test]
    fn default_manifest_uses_splash_and_app_themes() {
        let xml =
            AndroidVariant::Default.manifest_xml(&test_manifest_args("App", "dev.makepad.app"));
        assert!(xml.contains("android:theme=\"@style/MakepadAppTheme\""));
        assert!(xml.contains("android:theme=\"@style/MakepadLaunchTheme\""));
        assert!(xml.contains("android.permission.CAMERA"));
        assert!(xml.contains("android:launchMode=\"singleTask\""));
        assert!(xml.contains("android:minSdkVersion=\"26\""));
        assert!(xml.contains("android:targetSdkVersion=\"35\""));
        assert!(xml.contains("android:versionCode=\"7\""));
        assert!(xml.contains("android:versionName=\"1.2.3\""));
    }

    #[test]
    fn quest_manifest_uses_splash_and_app_themes() {
        let xml = AndroidVariant::Quest.manifest_xml(&test_manifest_args("App", "dev.makepad.app"));
        assert!(xml.contains("android:theme=\"@style/MakepadAppTheme\""));
        assert!(xml.contains("android:theme=\"@style/MakepadLaunchTheme\""));
        assert!(xml.contains("android.permission.CAMERA"));
        assert!(xml.contains("horizonos.permission.HEADSET_CAMERA"));
        assert!(xml.contains("horizonos.permission.SPATIAL_CAMERA"));
        assert!(xml.contains("android.hardware.camera"));
        assert!(xml.contains("android.hardware.camera2.full"));
        assert!(xml.contains("android.hardware.vr.headtracking"));
        assert!(xml.contains("android:version=\"1\""));
        assert!(xml.contains("android:required=\"true\""));
        assert!(xml.contains("android:allowBackup=\"false\""));
        assert!(xml.contains("android:extractNativeLibs=\"true\""));
        assert!(xml.contains("com.samsung.android.vr.application.mode"));
        assert!(xml.contains("android:hardwareAccelerated=\"false\""));
        assert!(xml.contains("android:resizeableActivity=\"false\""));
        assert!(xml.contains("android:excludeFromRecents=\"true\""));
        assert!(xml.contains("com.oculus.vr.focusaware"));
        assert!(xml.contains("com.oculus.intent.category.VR"));
        assert!(xml.contains("android:value=\"vr_only\""));
        assert!(xml.contains("android:minSdkVersion=\"26\""));
        assert!(xml.contains("android:targetSdkVersion=\"35\""));
        assert!(xml.contains("android:versionCode=\"7\""));
        assert!(xml.contains("android:versionName=\"1.2.3\""));
    }

    #[test]
    fn quest_manifest_can_omit_camera_permissions() {
        let mut args = test_manifest_args("App", "dev.makepad.app");
        args.quest_camera_permissions = false;
        let xml = AndroidVariant::Quest.manifest_xml(&args);
        assert!(!xml.contains("android.permission.CAMERA"));
        assert!(!xml.contains("horizonos.permission.HEADSET_CAMERA"));
        assert!(!xml.contains("horizonos.permission.SPATIAL_CAMERA"));
        assert!(!xml.contains("android.hardware.camera"));
        assert!(!xml.contains("android.hardware.camera2.full"));
        assert!(xml.contains("org.khronos.openxr.permission.OPENXR"));
        assert!(xml.contains("org.khronos.openxr.permission.OPENXR_SYSTEM"));
        assert!(xml.contains("MakepadAppXr"));
        assert!(xml.contains("com.oculus.intent.category.VR"));
    }

    #[test]
    fn default_manifest_can_force_landscape_panel() {
        let mut args = test_manifest_args("App", "dev.makepad.app");
        args.screen_orientation = Some("landscape");
        args.resizeable_activity = Some(false);
        let xml = AndroidVariant::Default.manifest_xml(&args);
        assert!(xml.contains("android:screenOrientation=\"landscape\""));
        assert!(xml.contains("android:resizeableActivity=\"false\""));
        assert!(!xml.contains("com.oculus.intent.category.VR"));
    }
}
