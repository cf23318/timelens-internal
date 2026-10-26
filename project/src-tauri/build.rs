mod preset_build;
fn main() {
    println!("cargo:rerun-if-env-changed=TIMELENS_ACTIVATION_CONFIG");
    println!("cargo:rerun-if-env-changed=TIMELENS_ALLOW_UNCONFIGURED_BUILD");
    println!("cargo:rerun-if-changed=preset_build.rs");
    let file=std::env::var_os("TIMELENS_ACTIVATION_CONFIG").filter(|v|!v.is_empty()).map(std::path::PathBuf::from);
    if let Some(p)=&file {println!("cargo:rerun-if-changed={}",p.display());}
    let raw=preset_build::load(file.as_deref(),std::env::var("PROFILE").as_deref()==Ok("release"),std::env::var("TIMELENS_ALLOW_UNCONFIGURED_BUILD").as_deref()==Ok("1"),chrono::Utc::now().timestamp_millis()).unwrap_or_else(|e|panic!("{e}"));
    let out=std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("timelens-activation-preset.json");
    // OUT_DIR is reused by Cargo. Always overwrite, including when removing a preset.
    use std::io::Write;
    let mut options=std::fs::OpenOptions::new();options.write(true).create(true).truncate(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut output=options.open(out).expect("无法写入构建预置配置");
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;output.set_permissions(std::fs::Permissions::from_mode(0o600)).expect("无法保护构建预置配置");}
    output.write_all(raw.as_bytes()).expect("无法写入构建预置配置");
    tauri_build::build()
}
