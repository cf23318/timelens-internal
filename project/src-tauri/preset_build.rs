//! Shared, non-network validation for an administrator-supplied build profile.
use serde_json::Value;
use std::{fs, path::Path};
#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StrictActivation {format:String,issuer_url:String,activation_code:String,batch_id:String,scope:String,bucket:String,archive_end_at:String}

pub fn validate(raw: &[u8], now_ms: i64) -> Result<String, String> {
    if raw.len() > 65536 {return Err("预置配置超过64KB".into());}
    // Struct deserialization also rejects duplicate fields, including ignored
    // earlier values which must never be copied into a compiled credential.
    let _:StrictActivation=serde_json::from_slice(raw).map_err(|_|"预置配置字段无效，禁止重复字段或长期密钥")?;
    let v:Value=serde_json::from_slice(raw).map_err(|_|"预置配置不是有效JSON")?;
    let obj=v.as_object().ok_or("预置配置须为对象")?;
    let keys=["format","issuerUrl","activationCode","batchId","scope","bucket","archiveEndAt"];
    if obj.len()!=keys.len() || keys.iter().any(|k|!obj.get(*k).is_some_and(Value::is_string)) || obj.keys().any(|k|!keys.contains(&k.as_str())) {return Err("预置配置字段无效；只接受批次激活配置，禁止OSS长期密钥".into());}
    let field=|key:&str|obj[key].as_str().unwrap();
    let valid_id=|s:&str|!s.is_empty() && s.len()<=64 && s.as_bytes()[0].is_ascii_alphanumeric() && s.bytes().all(|b|b.is_ascii_alphanumeric()||b"._-".contains(&b));
    let bucket=field("bucket");
    if field("format")!="timelens-cloudflare-activation-v1" || !["device","shared"].contains(&field("scope")) || !valid_id(field("batchId")) || bucket.len()<3 || bucket.len()>63 || !bucket.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-') || !bucket.as_bytes()[0].is_ascii_alphanumeric() || !bucket.as_bytes()[bucket.len()-1].is_ascii_alphanumeric() {return Err("预置批次或Bucket无效".into());}
    let secret=field("activationCode");
    if secret.len()!=64 || !secret.bytes().all(|b|b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {return Err("预置激活凭据无效".into());}
    let u=url::Url::parse(field("issuerUrl")).map_err(|_|"预置签发地址无效")?;
    if u.scheme()!="https" || u.host_str().is_none() || !u.username().is_empty() || u.password().is_some() || u.path()!="/" || u.query().is_some() || u.fragment().is_some() {return Err("预置签发地址须为HTTPS根地址".into());}
    let end=chrono::DateTime::parse_from_rfc3339(field("archiveEndAt")).map_err(|_|"预置批准结束须包含时区")?.timestamp_millis();
    if end<=now_ms || end>4102444800000 {return Err("预置批次已结束或截止无效；不能构建新员工包".into());}
    String::from_utf8(raw.to_vec()).map_err(|_|"预置配置编码失败".into())
}

pub fn load(path:Option<&Path>, release:bool, allow_generic:bool, now:i64)->Result<String,String> {
    if let Some(path)=path {
        let meta=fs::symlink_metadata(path).map_err(|_|"无法读取预置配置文件")?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>65536 {return Err("预置配置须为普通小文件，不能是符号链接".into());}
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;if meta.permissions().mode()&0o077!=0 {return Err("预置配置须仅当前用户可读写（0600）".into());}}
        validate(&fs::read(path).map_err(|_|"无法读取预置配置")?,now)
    } else if release && !allow_generic {
        Err("员工构建缺少预置配置：设置TIMELENS_ACTIVATION_CONFIG指向私有激活JSON；通用诊断包须显式设置TIMELENS_ALLOW_UNCONFIGURED_BUILD=1".into())
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
}
