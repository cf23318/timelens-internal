//! Shared, non-network validation for an administrator-supplied build profile.
use serde_json::Value;
use std::{fs, path::Path};
#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StrictActivation {format:String,issuer_url:String,activation_code:String,batch_id:String,scope:String,bucket:String,archive_end_at:String}
#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StrictRegistration {format:String,issuer_url:String,batch_id:String,scope:String,bucket:String,study_window_days:u32}

pub fn validate(raw: &[u8], now_ms: i64) -> Result<String, String> {
    if raw.len() > 65536 {return Err("预置配置超过64KB".into());}
    // Struct deserialization also rejects duplicate fields, including ignored
    // earlier values which must never be copied into a compiled credential.
    let v:Value=serde_json::from_slice(raw).map_err(|_|"预置配置不是有效JSON")?;
    let obj=v.as_object().ok_or("预置配置须为对象")?;
    let registration=obj.get("format").and_then(Value::as_str)==Some("timelens-cloudflare-registration-v1");
    let keys:&[&str]=if registration {&["format","issuerUrl","batchId","scope","bucket","studyWindowDays"]} else {&["format","issuerUrl","activationCode","batchId","scope","bucket","archiveEndAt"]};
    if registration {let _:StrictRegistration=serde_json::from_slice(raw).map_err(|_|"公开注册配置字段无效，禁止凭据或重复字段")?;}
    else {let _:StrictActivation=serde_json::from_slice(raw).map_err(|_|"预置配置字段无效，禁止重复字段或长期密钥")?;}
    if obj.len()!=keys.len() || keys.iter().filter(|k|**k!="studyWindowDays").any(|k|!obj.get(*k).is_some_and(Value::is_string)) || obj.keys().any(|k|!keys.contains(&k.as_str())) {return Err("预置配置字段无效，禁止OSS长期密钥".into());}
    let field=|key:&str|obj[key].as_str().unwrap();
    let valid_id=|s:&str|!s.is_empty() && s.len()<=64 && s.as_bytes()[0].is_ascii_alphanumeric() && s.bytes().all(|b|b.is_ascii_alphanumeric()||b"._-".contains(&b));
    let bucket=field("bucket");
    if !registration && field("format")!="timelens-cloudflare-activation-v1" || !["device","shared"].contains(&field("scope")) || registration && field("scope")!="device" || !valid_id(field("batchId")) || bucket.len()<3 || bucket.len()>63 || !bucket.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-') || !bucket.as_bytes()[0].is_ascii_alphanumeric() || !bucket.as_bytes()[bucket.len()-1].is_ascii_alphanumeric() {return Err("预置批次或Bucket无效".into());}
    let u=url::Url::parse(field("issuerUrl")).map_err(|_|"预置签发地址无效")?;
    if u.scheme()!="https" || u.host_str().is_none() || !u.username().is_empty() || u.password().is_some() || u.path()!="/" || u.query().is_some() || u.fragment().is_some() {return Err("预置签发地址须为HTTPS根地址".into());}
    if registration {
        if !obj["studyWindowDays"].as_u64().is_some_and(|n|(1..=30).contains(&n)) {return Err("设备调研窗口须为1至30天".into());}
    } else {
        let secret=field("activationCode");
        if secret.len()!=64 || !secret.bytes().all(|b|b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {return Err("预置激活凭据无效".into());}
        let end=chrono::DateTime::parse_from_rfc3339(field("archiveEndAt")).map_err(|_|"预置批准结束须包含时区")?.timestamp_millis();
        if end<=now_ms || end>4102444800000 {return Err("预置批次已结束或截止无效；不能构建新员工包".into());}
    }
    String::from_utf8(raw.to_vec()).map_err(|_|"预置配置编码失败".into())
}

pub fn load(path:Option<&Path>, release:bool, allow_generic:bool, now:i64)->Result<String,String> {
    if let Some(path)=path {
        let meta=fs::symlink_metadata(path).map_err(|_|"无法读取预置配置文件")?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>65536 {return Err("预置配置须为普通小文件，不能是符号链接".into());}
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;if meta.permissions().mode()&0o077!=0 {return Err("预置配置须仅当前用户可读写（0600）".into());}}
        validate(&fs::read(path).map_err(|_|"无法读取预置配置")?,now)
    } else if release && !allow_generic {
        Err("员工构建缺少预置配置：设置TIMELENS_ACTIVATION_CONFIG指向部署配置JSON；通用诊断包须显式设置TIMELENS_ALLOW_UNCONFIGURED_BUILD=1".into())
    } else {Ok("null".into())}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw()->Value {serde_json::json!({"format":"timelens-cloudflare-activation-v1","issuerUrl":"https://issuer.example.com/","activationCode":"a".repeat(64),"batchId":"pilot","scope":"device","bucket":"test-bucket","archiveEndAt":"2099-01-01T00:00:00Z"})}
    #[test] fn strict_profile_rejects_secrets_invalid_scope_and_expired_batch() {
        let v=raw();assert!(validate(&serde_json::to_vec(&v).unwrap(),1).is_ok());
        for (key,value) in [("issuerUrl","http://example.com/"),("issuerUrl","https://u:p@example.com/"),("issuerUrl","https://example.com/?token=x"),("scope","any"),("activationCode","bad"),("archiveEndAt","2020-01-01T00:00:00Z")] {let mut bad=v.clone();bad[key]=value.into();assert!(validate(&serde_json::to_vec(&bad).unwrap(),1792979363000).is_err());}
        let mut bad=v;bad["accessKeySecret"]="must-not-be-embedded".into();let err=validate(&serde_json::to_vec(&bad).unwrap(),1).unwrap_err();assert!(!err.contains("must-not-be-embedded"));
        let normal=raw().to_string();let duplicate=format!("{{\"activationCode\":\"must-not-be-embedded\",{}",&normal[1..]);assert!(validate(duplicate.as_bytes(),1).is_err());
    }
    #[test] fn release_needs_profile_and_builds_do_not_reuse_old_content() {
        assert!(load(None,true,false,1).is_err());assert_eq!(load(None,true,true,1).unwrap(),"null");assert_eq!(load(None,false,false,1).unwrap(),"null");
        let t=tempfile::tempdir().unwrap();let p=t.path().join("preset.private.json");
        fs::write(&p,serde_json::to_vec(&raw()).unwrap()).unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;fs::set_permissions(&p,fs::Permissions::from_mode(0o600)).unwrap();}
        let first=load(Some(&p),true,false,1).unwrap();let mut v=raw();v["activationCode"]="b".repeat(64).into();fs::write(&p,serde_json::to_vec(&v).unwrap()).unwrap();
        assert_ne!(load(Some(&p),true,false,1).unwrap(),first);assert_eq!(load(None,false,false,1).unwrap(),"null");
    }
    #[test] fn public_registration_profile_needs_no_credential_or_absolute_batch_end() {
        let v=serde_json::json!({"format":"timelens-cloudflare-registration-v1","issuerUrl":"https://issuer.example.com/","batchId":"employees","scope":"device","bucket":"test-bucket","studyWindowDays":7});
        assert!(validate(v.to_string().as_bytes(),4102440000000).is_ok());
        for (k,value) in [("activationCode",serde_json::json!("a".repeat(64))),("studyWindowDays",serde_json::json!(0)),("studyWindowDays",serde_json::json!(31)),("scope",serde_json::json!("shared"))] {let mut bad=v.clone();bad[k]=value;assert!(validate(bad.to_string().as_bytes(),1).is_err());}
        let duplicate=format!("{{\"studyWindowDays\":7,{}",&v.to_string()[1..]);assert!(validate(duplicate.as_bytes(),1).is_err());
    }
}
