//! Online grant renewal. Network I/O never holds the collection gate.
use std::{io::Read, path::Path, sync::atomic::Ordering, time::Duration};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use super::{internal::{InternalControl, InternalConfig, TaskProgress}, oss_direct::{self, Grant}};

const FILE: &str = "cloud-issuer-device.json";
const DAY: i64 = 86_400_000;
#[derive(Default, Clone, Serialize)]
#[serde(rename_all="camelCase")]
pub struct CloudStatus { pub configured: bool, pub issuer_url: String, pub last_checked_ms: i64, pub last_error: Option<String> }
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Enrollment { issuer_url: String, device_id: String, device_secret: String, activation_code: Option<String>, expected:Expected, #[serde(default)] force_refresh:bool, #[serde(default)] preset_managed:bool }
#[derive(Serialize,Deserialize,Clone)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Expected {batch_id:String,scope:String,bucket:String,archive_end_at:String}
impl Expected {
    fn matches(&self,g:&Grant)->bool {g.user_id==self.batch_id && g.bucket==self.bucket && g.archive_end_at==self.archive_end_at && g.is_shared()==(self.scope=="shared")}
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Activation { format: String, issuer_url: String, activation_code: String, batch_id:String, scope:String, bucket:String, archive_end_at:String }
fn is_secret(s: &str) -> bool { s.len()==64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }
fn url(value: &str) -> Result<reqwest::Url,String> {
    let u=reqwest::Url::parse(value).map_err(|_|"云端签发地址无效")?;
    if u.scheme()!="https" || u.host_str().is_none() || !u.username().is_empty() || u.password().is_some() || u.path()!="/" || u.query().is_some() || u.fragment().is_some() { return Err("云端签发地址须为 HTTPS 根地址".into()); }
    Ok(u)
}
fn enrollment(root:&Path)->Result<Enrollment,String> {
    let e:Enrollment=serde_json::from_slice(&oss_direct::private_read(&root.join(FILE))?).map_err(|_|"设备激活配置无效")?;
    url(&e.issuer_url)?;
    if e.device_id!=oss_direct::device_id(root)? || !is_secret(&e.device_secret) || e.activation_code.as_ref().is_some_and(|s|!is_secret(s)) { return Err("设备激活身份不匹配".into()); }
    Ok(e)
}
pub fn summary(root:&Path, control:&InternalControl)->CloudStatus {
    let mut s=control.cloud_status.lock().clone();
    if let Ok(e)=enrollment(root) {s.configured=true;s.issuer_url=e.issuer_url;} s
}
pub fn refresh_due(expires:&str, now:i64)->bool { DateTime::parse_from_rfc3339(expires).map_or(true,|t|t.timestamp_millis()<=now+DAY) }
fn same_scope(a:&Grant,b:&Grant)->bool {
    a.format==b.format && a.device_id==b.device_id && a.user_id==b.user_id && a.bucket==b.bucket && a.region==b.region && a.endpoint==b.endpoint && a.prefix==b.prefix && a.archive_end_at==b.archive_end_at
}
// Expired old grants are parsed for immutable identity, never used for sending.
pub fn install_renewal(root:&Path,control:&InternalControl,raw:&str)->Result<(),String> {
    let next=Grant::parse(raw,&oss_direct::device_id(root)?,Utc::now().timestamp_millis())?;
    let _gate=control.gate.write();
    let cfg=control.config.read().clone();
    let old:Grant=serde_json::from_slice(&oss_direct::private_read(&root.join(oss_direct::GRANT_FILE))?).map_err(|_|"原授权损坏，不能自动续期")?;
    if !same_scope(&old,&next) {return Err("云端续期改变了授权范围或调研截止，已拒绝".into());}
    next.matches_config(&cfg)?;
    if DateTime::parse_from_rfc3339(&next.issued_at).map_err(|_|"签发时间无效")? < DateTime::parse_from_rfc3339(&old.issued_at).map_err(|_|"原签发时间无效")? {return Err("拒绝过时的续期响应".into());}
    if let Some(b)=super::archive::load(root)? {super::archive::ensure_live(root,&b.task_id,Utc::now().timestamp_millis())?;}
    super::internal::write_json_atomic(&root.join(oss_direct::GRANT_FILE),&next)
}
fn request(client:&reqwest::blocking::Client,e:&Enrollment,force:bool)->Result<String,String> {
    let base=url(&e.issuer_url)?;
    let r=if let Some(code)=&e.activation_code {
        client.post(base.join("v1/activate").unwrap()).json(&serde_json::json!({"activationCode":code,"deviceId":e.device_id,"deviceSecret":e.device_secret}))
    } else {client.post(base.join("v1/renew").unwrap()).bearer_auth(&e.device_secret).json(&serde_json::json!({"deviceId":e.device_id,"force":force}))};
    let mut response=r.send().map_err(|_|"云端续期连接失败；已有授权及待传图片保留")?;
    if !response.status().is_success() {return Err(format!("云端授权返回 HTTP {}；请检查激活或设备状态",response.status().as_u16()));}
    let mut bytes=Vec::new(); response.by_ref().take(65537).read_to_end(&mut bytes).map_err(|_|"云端授权响应读取失败")?;
    if bytes.len()>65536 {return Err("云端授权响应过大".into());}
    let v:serde_json::Value=serde_json::from_slice(&bytes).map_err(|_|"云端授权响应无效")?;
    serde_json::to_string(v.get("grant").ok_or("云端授权缺失")?).map_err(|_|"云端授权无效".into())
}
fn client()->Result<reqwest::blocking::Client,String> {
    reqwest::blocking::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(25)).build().map_err(|_|"无法建立云端签发连接".into())
}
fn refresh_locked(root:&Path,control:&InternalControl,force:bool)->Result<(),String> {
    refresh_with_client(root,control,force,&client()?)
}
fn refresh_with_client(root:&Path,control:&InternalControl,force:bool,client:&reqwest::blocking::Client)->Result<(),String> {
    let mut e=enrollment(root)?;
    let cfg=control.config.read().clone();
    if !force && !e.force_refresh && e.activation_code.is_none() && oss_direct::load(root,&cfg).is_ok_and(|g|!refresh_due(&g.expires_at,Utc::now().timestamp_millis())) {return Ok(());}
    let raw=match request(client,&e,force||e.force_refresh) {
        Ok(raw)=>raw,
        Err(error)=>{
            if e.force_refresh && !retryable_renewal(&error) {e.force_refresh=false;super::internal::write_json_atomic(&root.join(FILE),&e)?;}
            return Err(error);
        }
    };
    let grant=Grant::parse(&raw,&e.device_id,Utc::now().timestamp_millis())?;
    if !e.expected.matches(&grant) {return Err("云端返回的授权与激活批次不符".into());}
    if root.join(oss_direct::GRANT_FILE).exists() {install_renewal(root,control,&raw)?;} else {install_initial(root,control,&grant,e.preset_managed)?;}
    e.activation_code=None;
    e.force_refresh=false;
    super::internal::write_json_atomic(&root.join(FILE),&e)
}
fn install_initial(root:&Path,control:&InternalControl,grant:&Grant,preset_managed:bool)->Result<(),String> {
    let _gate=control.gate.write();
    let old=control.config.read().clone();let p=control.progress.lock().clone();
    if root.join(oss_direct::GRANT_FILE).exists() || control.frame_upload_running.load(Ordering::SeqCst) || p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() || p.integrity_error.is_some() {return Err("首次授权落地状态不符，请人工核对".into());}
    if let Some(b)=super::archive::load(root)? {
        if !preset_managed {return Err("首次授权落地状态不符，请人工核对".into());}
        validate_binding(root,&old,&p,&b)?;
        grant.matches_config(&old)?;
    }
    if !old.upload_url.is_empty() {grant.matches_config(&old)?;}
    let mut cfg=old.clone();cfg.upload_mode="ossDirect".into();cfg.upload_url=grant.endpoint.clone();cfg.upload_survey_id=grant.user_id.clone();cfg.upload_device_id=grant.device_id.clone();cfg.archive_end_at=grant.archive_end_at.clone();cfg.domestic_server_confirmed=true;
    cfg.validate()?;cfg.require_participant_name()?;
    super::internal::write_json_atomic(&root.join("internal-config.json"),&cfg)?;
    if let Err(e)=super::internal::write_json_atomic(&root.join(oss_direct::GRANT_FILE),grant) {
        super::internal::write_json_atomic(&root.join("internal-config.json"),&old).map_err(|_|"首次授权失败且配置恢复失败，请停止上传并核对")?;return Err(e);
    }
    *control.config.write()=cfg;Ok(())
}
fn validate_binding(root:&Path,cfg:&InternalConfig,p:&TaskProgress,b:&super::archive::ArchiveBinding)->Result<(),String> {
    let owner:String=serde_json::from_slice(&oss_direct::private_read(&root.join("archive-profile-owner.json"))?).map_err(|_|"任务归属无效")?;
    if owner!=p.task_id || b.task_id!=p.task_id || b.upload_mode!=cfg.upload_mode || b.upload_url!=cfg.upload_url || b.survey_id!=cfg.upload_survey_id || b.device_id!=cfg.upload_device_id || b.planned_end_ms!=super::archive::end_ms(cfg)? {return Err("预置激活不能改写原任务归属或目的地".into());}
    super::archive::ensure_live(root,&p.task_id,Utc::now().timestamp_millis())
}
/// Network-free provisioning. The same function is used by startup, the task
/// clock and ALL native start entrypoints; no pasted JSON is required.
fn preset_already_prepared(root:&Path,control:&InternalControl)->Result<bool,String> {
    if !root.join(FILE).exists() {return Ok(false);}
    let e=enrollment(root)?;let _gate=control.gate.read();let cfg=control.config.read().clone();let p=control.progress.lock().clone();
    if cfg.participant_name.trim().is_empty() {return Err("请先填写姓名".into());}
    if p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() || p.integrity_error.is_some() {return Err("原任务需要管理员核对，不能自动激活".into());}
    let ready=cfg.upload_mode=="ossDirect" && cfg.upload_url==format!("https://{}.oss-cn-beijing.aliyuncs.com/",e.expected.bucket) && cfg.upload_survey_id==e.expected.batch_id && cfg.upload_device_id==e.device_id && cfg.archive_end_at==e.expected.archive_end_at && cfg.domestic_server_confirmed;
    if !ready {return Ok(false);}
    let has_grant=root.join(oss_direct::GRANT_FILE).exists();
    if has_grant {let old:Grant=serde_json::from_slice(&oss_direct::private_read(&root.join(oss_direct::GRANT_FILE))?).map_err(|_|"原授权损坏，请联系管理员")?;if !e.expected.matches(&old)||old.device_id!=e.device_id{return Err("原授权与设备不符，请联系管理员".into());}}
    if let Some(b)=super::archive::load(root)? {validate_binding(root,&cfg,&p,&b)?;if !has_grant && !e.preset_managed {return Err("原任务授权缺失，请联系管理员".into());}}
    Ok(e.preset_managed||has_grant)
}
pub fn prepare_preset(root:&Path,control:&InternalControl,raw:&str)->Result<(),String> {
    // A renewal may hold cloud_refresh through a network timeout. Already
    // provisioned starts validate immutable state without waiting for that I/O.
    let _single=loop {
        if preset_already_prepared(root,control)? {return Ok(());}
        if let Some(lock)=control.cloud_refresh.try_lock() {break lock;}
        std::thread::sleep(Duration::from_millis(10));
    };
    let existing=root.join(FILE).exists();
    let mut e=if existing {enrollment(root)?} else {
        let a:Activation=serde_json::from_str(raw).map_err(|_|"安装包预置配置无效，请联系管理员")?;
        if a.format!="timelens-cloudflare-activation-v1" || !is_secret(&a.activation_code) || !super::frame_upload::valid_id(&a.batch_id) || !["device","shared"].contains(&a.scope.as_str()) {return Err("安装包预置批次无效，请联系管理员".into());}
        Enrollment{issuer_url:url(&a.issuer_url)?.to_string(),device_id:oss_direct::device_id(root)?,device_secret:format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple()),activation_code:Some(a.activation_code),expected:Expected{batch_id:a.batch_id,scope:a.scope,bucket:a.bucket,archive_end_at:a.archive_end_at},force_refresh:false,preset_managed:false}
    };
    let _gate=control.gate.write();
    let old=control.config.read().clone();let p=control.progress.lock().clone();
    if old.participant_name.trim().is_empty() {return Err("请先填写姓名".into());}
    if p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() || p.integrity_error.is_some() {return Err("原任务需要管理员核对，不能自动激活".into());}
    let mut cfg=old.clone();cfg.upload_mode="ossDirect".into();cfg.upload_url=format!("https://{}.oss-cn-beijing.aliyuncs.com/",e.expected.bucket);cfg.upload_survey_id=e.expected.batch_id.clone();cfg.upload_device_id=e.device_id.clone();cfg.archive_end_at=e.expected.archive_end_at.clone();cfg.domestic_server_confirmed=true;
    cfg.validate()?;cfg.require_participant_name()?;
    if !old.upload_url.is_empty() && (old.upload_mode!=cfg.upload_mode || old.upload_url!=cfg.upload_url || old.upload_survey_id!=cfg.upload_survey_id || old.upload_device_id!=cfg.upload_device_id || old.archive_end_at!=cfg.archive_end_at) {return Err("预置配置与原任务不同，请联系管理员；原配置未改动".into());}
    let has_grant=root.join(oss_direct::GRANT_FILE).exists();
    if has_grant {
        let g:Grant=serde_json::from_slice(&oss_direct::private_read(&root.join(oss_direct::GRANT_FILE))?).map_err(|_|"原授权损坏，请联系管理员")?;
        if !e.expected.matches(&g) || g.device_id!=e.device_id {return Err("预置配置与原授权不同，请联系管理员".into());}
    }
    if let Some(b)=super::archive::load(root)? {
        validate_binding(root,&cfg,&p,&b)?;
        if !has_grant && !(existing&&e.preset_managed) {return Err("原任务授权缺失，不能作为新安装自动绑定".into());}
    }
    if !existing || !e.preset_managed {e.preset_managed=true;super::internal::write_json_atomic(&root.join(FILE),&e)?;}
    if serde_json::to_vec(&cfg).unwrap()!=serde_json::to_vec(&old).unwrap() {
        super::internal::write_json_atomic(&root.join("internal-config.json"),&cfg)?;*control.config.write()=cfg;
    }
    Ok(())
}
pub fn refresh(root:&Path,control:&InternalControl,force:bool)->Result<(),String> {
    let _single=control.cloud_refresh.lock();
    let result=refresh_locked(root,control,force);
    let mut s=control.cloud_status.lock();s.last_checked_ms=Utc::now().timestamp_millis();s.last_error=result.as_ref().err().cloned();
    result
}
pub enum Recovery { Unchanged,Updated,RetryLater }
fn retryable_renewal(error:&str)->bool {
    error.contains("连接失败") || error.contains("读取失败") || error.split("HTTP ").nth(1).and_then(|v|v.get(..3)).and_then(|v|v.parse::<u16>().ok()).is_some_and(|v|v==408||v==429||(500..600).contains(&v))
}
pub fn pending_refresh(root:&Path)->bool {enrollment(root).is_ok_and(|e|e.force_refresh)}
pub fn refresh_after_rejection(root:&Path,control:&InternalControl,grant:&Grant,error:&str)->Recovery {
    if !error.contains("HTTP 403") || !root.join(FILE).exists() {return Recovery::Unchanged;}
    match client() {Ok(client)=>recover_with_client(root,control,grant,&client),Err(_)=>Recovery::RetryLater}
}
fn recover_with_client(root:&Path,control:&InternalControl,grant:&Grant,client:&reqwest::blocking::Client)->Recovery {
    let _single=control.cloud_refresh.lock();
    let Ok(mut e)=enrollment(root) else{return Recovery::Unchanged;};
    e.force_refresh=true;
    if super::internal::write_json_atomic(&root.join(FILE),&e).is_err() {return Recovery::Unchanged;}
    let r=refresh_with_client(root,control,true,client);
    {let mut s=control.cloud_status.lock();s.last_checked_ms=Utc::now().timestamp_millis();s.last_error=r.as_ref().err().cloned();}
    match r {Err(error) if retryable_renewal(&error)=>Recovery::RetryLater,Ok(()) if oss_direct::load(root,&control.config.read()).is_ok_and(|next|next.fingerprint()!=grant.fingerprint())=>Recovery::Updated,_=>Recovery::Unchanged}
}
pub fn activate(root:&Path,control:&InternalControl,raw:&str)->Result<(),String> {
    let a:Activation=serde_json::from_str(raw).map_err(|_|"云端激活配置格式无效")?;
    if a.format!="timelens-cloudflare-activation-v1" || !is_secret(&a.activation_code) {return Err("云端激活配置无效".into());}
    let issuer=url(&a.issuer_url)?.to_string();
    let expected=Expected{batch_id:a.batch_id.clone(),scope:a.scope.clone(),bucket:a.bucket.clone(),archive_end_at:a.archive_end_at.clone()};
    let _single=control.cloud_refresh.lock();
    let cfg=control.config.read().clone();
    if cfg.participant_name.trim().is_empty() {return Err("请先填写并保存姓名，再激活".into());}
    if control.frame_upload_running.load(Ordering::SeqCst) || control.tracking.load(Ordering::SeqCst) {return Err("首次激活前请暂停采集并停止上传；续期无需暂停".into());}
    if !super::frame_upload::valid_id(&a.batch_id) || !["device","shared"].contains(&a.scope.as_str()) || DateTime::parse_from_rfc3339(&a.archive_end_at).is_err() {return Err("激活批次信息无效".into());}
    let p=control.progress.lock().clone();
    if p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() || p.integrity_error.is_some() || super::archive::load(root)?.is_some() && !root.join(oss_direct::GRANT_FILE).exists() {return Err("原任务完整性或授权缺失，请先人工核对".into());}
    if !cfg.upload_url.is_empty() && (cfg.upload_mode!="ossDirect" || cfg.upload_url!=format!("https://{}.oss-cn-beijing.aliyuncs.com/",a.bucket) || cfg.upload_survey_id!=a.batch_id || cfg.archive_end_at!=a.archive_end_at || cfg.upload_device_id!=oss_direct::device_id(root)?) {return Err("激活配置与已保存的目的地不符".into());}
    if root.join(oss_direct::GRANT_FILE).exists() {
        let old:Grant=serde_json::from_slice(&oss_direct::private_read(&root.join(oss_direct::GRANT_FILE))?).map_err(|_|"原授权损坏，不能激活")?;
        if old.user_id!=a.batch_id || old.bucket!=a.bucket || old.archive_end_at!=a.archive_end_at || old.is_shared()!=(a.scope=="shared") {return Err("激活配置与原任务范围不符；请使用原批次配置".into());}
    }
    let e=if root.join(FILE).exists() {
        let mut e=enrollment(root)?;
        if e.activation_code.is_none() && e.issuer_url!=issuer {return Err("此设备已绑定其他签发服务".into());}
        if e.activation_code.is_some() {
            if e.issuer_url!=issuer {e.device_secret=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());}
            e.issuer_url=issuer;e.activation_code=Some(a.activation_code);
            e.expected=expected;
        } e
    } else {Enrollment{issuer_url:issuer,device_id:oss_direct::device_id(root)?,device_secret:format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple()),activation_code:Some(a.activation_code),expected,force_refresh:false,preset_managed:false}};
    // Persist before activation so a lost successful reply is safely retryable.
    super::internal::write_json_atomic(&root.join(FILE),&e)?;
    let result=refresh_locked(root,control,true);
    let mut s=control.cloud_status.lock();s.last_checked_ms=Utc::now().timestamp_millis();s.last_error=result.as_ref().err().cloned(); result
}
pub fn set_auto(control:&InternalControl,enabled:bool)->Result<(),String> {
    let _gate=control.gate.write();
    let mut cfg=control.config.read().clone();cfg.auto_upload_enabled=enabled;
    super::internal::write_json_atomic(&control.root.join("internal-config.json"),&cfg)?;
    *control.config.write()=cfg;
    if enabled {*control.auto_failed_grant.lock()=None;}
    if !enabled {control.frame_upload_active.store(false,Ordering::SeqCst);} Ok(())
}
pub fn auto_ready(cfg:&InternalConfig,p:&TaskProgress)->bool {
    cfg.upload_mode=="ossDirect" && cfg.auto_upload_enabled && cfg.require_participant_name().is_ok() && p.integrity_error.is_none() && p.receipt.is_none() && p.stored_bundle.is_none() && p.bundle_path.is_none() && (!p.needs_flush || p.status==super::internal::TaskStatus::Recording)
}
pub fn spawn(app:tauri::AppHandle) {
    std::thread::spawn(move|| {
        let mut next_refresh=0i64;let mut failures=0u32;
        loop {
            let Some(state)=app.try_state::<crate::AppState>() else {break;};
            if !state.0.running.load(Ordering::SeqCst) || state.0.exiting.load(Ordering::SeqCst) {break;}
            let cfg=state.0.internal.config.read().clone();
            if cfg.upload_mode=="ossDirect" {
                if super::preset::employee_mode() && !cfg.participant_name.trim().is_empty() {
                    if let Err(error)=super::preset::prepare(&state.0.paths.root,&state.0.internal) {
                        state.0.internal.cloud_status.lock().last_error=Some(error);
                        std::thread::sleep(Duration::from_secs(5));continue;
                    }
                }
                if state.0.paths.root.join(FILE).exists() && Utc::now().timestamp_millis()>=next_refresh {
                    let r=refresh(&state.0.paths.root,&state.0.internal,false);
                    failures=if r.is_ok(){0}else{failures.saturating_add(1)};
                    next_refresh=Utc::now().timestamp_millis()+if failures==0{60000}else{super::frame_upload::retry_delay_ms(failures)};
                }
                if !state.0.internal.frame_upload_running.load(Ordering::SeqCst) {
                    if let Err(e)=super::oss_direct::begin_auto(&app) {
                        if state.0.internal.progress.lock().last_error.as_ref()!=Some(&e) {let _=state.0.internal.edit_progress(|p|{p.last_error=Some(e);Ok(())});}
                    }
                }
                let _=app.emit("internal_task_changed",state.0.internal.progress.lock().clone());
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc,atomic::AtomicBool};
    fn grant(device:&str,scope:&str,offset:i64,plan:i64)->String {
        let script=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cloudflare-issuer/test/grant-fixture.mjs");
        let r=std::process::Command::new(std::env::var("TIMELENS_TEST_NODE").unwrap_or("node".into())).arg(script).args([device,scope,&offset.to_string(),&plan.to_string()]).output().unwrap();
        assert!(r.status.success());String::from_utf8(r.stdout).unwrap()
    }
    fn preset_raw(batch:&str)->String {serde_json::json!({"format":"timelens-cloudflare-activation-v1","issuerUrl":"https://issuer.example.com/","activationCode":"a".repeat(64),"batchId":batch,"scope":"device","bucket":"test-bucket","archiveEndAt":"2099-01-01T00:00:00.000Z"}).to_string()}
    #[test] fn automatic_preset_preserves_identity_stop_choice_and_old_scope_after_update() {
        let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();
        assert!(prepare_preset(t.path(),&c,&preset_raw("pilot")).is_err());assert!(!t.path().join(FILE).exists());
        c.save_participant_name("  张三  ").unwrap();set_auto(&c,false).unwrap();
        prepare_preset(t.path(),&c,&preset_raw("pilot")).unwrap();
        let before=oss_direct::private_read(&t.path().join(FILE)).unwrap();let id=oss_direct::device_id(t.path()).unwrap();
        let restored=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();
        prepare_preset(t.path(),&restored,&preset_raw("different-batch-in-updated-installer")).unwrap();
        assert_eq!(before,oss_direct::private_read(&t.path().join(FILE)).unwrap());
        let cfg=restored.config.read();assert_eq!(cfg.participant_name,"张三");assert_eq!(cfg.upload_survey_id,"pilot");assert_eq!(cfg.upload_device_id,id);assert!(!cfg.auto_upload_enabled);
        assert!(!restored.tracking.load(Ordering::SeqCst));
    }
    #[test] fn concurrent_name_only_save_never_rolls_back_automatic_provisioning() {
        let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();c.save_participant_name("测试员工").unwrap();
        let copy=c.clone();let root=t.path().to_owned();let a=std::thread::spawn(move||{for _ in 0..20 {prepare_preset(&root,&copy,&preset_raw("pilot")).unwrap();}});
        let copy=c.clone();let b=std::thread::spawn(move||{for n in 0..20 {copy.save_participant_name(&format!("测试员工{n}")).unwrap();}});
        a.join().unwrap();b.join().unwrap();let cfg=c.config.read();assert_eq!(cfg.upload_survey_id,"pilot");assert_eq!(cfg.participant_name,"测试员工19");assert!(!cfg.upload_url.is_empty());
    }
    #[test] fn prepared_start_does_not_wait_for_in_flight_network_renewal() {
        let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();c.save_participant_name("测试员工").unwrap();prepare_preset(t.path(),&c,&preset_raw("pilot")).unwrap();
        let network=c.cloud_refresh.lock();let copy=c.clone();let root=t.path().to_owned();let(tx,rx)=std::sync::mpsc::channel();
        let thread=std::thread::spawn(move||{tx.send(prepare_preset(&root,&copy,&preset_raw("pilot"))).unwrap();});
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap().is_ok());drop(network);thread.join().unwrap();
    }
    #[test] fn worker_grants_renew_expired_grants_without_changing_scope_or_capture() {
        for scope in ["device","shared"] {
            let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();super::super::internal::fill_test_participant_name(&c);
            let device=oss_direct::device_id(t.path()).unwrap();let plan=Utc::now().timestamp_millis()+7*DAY;
            let fresh=grant(&device,scope,0,plan);
            oss_direct::import(t.path(),&c,&fresh).unwrap();
            // Persist a correctly signed expired grant as an existing old installation.
            let expired=grant(&device,scope,-8*DAY,plan);
            let mut old:Grant=serde_json::from_str(&expired).unwrap();
            // Shared profiles persist their locally bound device/prefix.
            if scope=="shared" {old.device_id=device.clone();old.prefix=oss_direct::prefix("pilot",&device);for fields in std::iter::once(&mut old.fields).chain(old.manifest_fields.iter_mut()){fields.insert("x-oss-meta-timelens-device".into(),device.clone());}}
            super::super::internal::write_json_atomic(&t.path().join(oss_direct::GRANT_FILE),&old).unwrap();
            assert!(oss_direct::load(t.path(),&c.config.read()).is_err());
            c.start().unwrap();super::super::frame_upload::start_run(&c).unwrap();
            install_renewal(t.path(),&c,&fresh).unwrap();
            assert!(c.tracking.load(Ordering::SeqCst));assert!(c.frame_upload_active.load(Ordering::SeqCst));
            assert!(oss_direct::load(t.path(),&c.config.read()).is_ok());
            let before=std::fs::read(t.path().join(oss_direct::GRANT_FILE)).unwrap();
            let changed=grant(&device,scope,0,plan+DAY);
            assert!(install_renewal(t.path(),&c,&changed).is_err());assert_eq!(std::fs::read(t.path().join(oss_direct::GRANT_FILE)).unwrap(),before);
            assert!(summary(t.path(),&c).issuer_url.is_empty());
            super::super::frame_upload::finish_run(&c);c.pause(false).unwrap();
            let a=serde_json::json!({"format":"timelens-cloudflare-activation-v1","issuerUrl":"https://issuer.invalid/","activationCode":"a".repeat(64),"batchId":"wrong-batch","scope":scope,"bucket":"test-bucket","archiveEndAt":old.archive_end_at});
            assert!(activate(t.path(),&c,&a.to_string()).is_err());
            assert!(!t.path().join(FILE).exists()); // Reject before credentials or cloud registration.
        }
    }
    #[test] fn tls_activation_retries_then_renews_with_the_same_private_device_credential() {
        use std::{io::Write,net::TcpListener};
        use rustls::{pki_types::PrivatePkcs8KeyDer,ServerConfig,ServerConnection,StreamOwned};
        let _=rustls::crypto::ring::default_provider().install_default();
        let cert=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let tls=Arc::new(ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert.cert.der().clone()],PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into()).unwrap());
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();let addr=listener.local_addr().unwrap();
        let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();super::super::internal::fill_test_participant_name(&c);
        let device=oss_direct::device_id(t.path()).unwrap();let raw=grant(&device,"device",0,Utc::now().timestamp_millis()+7*DAY);
        let g:Grant=serde_json::from_str(&raw).unwrap();
        let e=Enrollment{issuer_url:format!("https://{addr}/"),device_id:device,device_secret:"c".repeat(64),activation_code:Some("d".repeat(64)),expected:Expected{batch_id:g.user_id,scope:"device".into(),bucket:g.bucket,archive_end_at:g.archive_end_at},force_refresh:false,preset_managed:false};
        let preset=serde_json::json!({"format":"timelens-cloudflare-activation-v1","issuerUrl":e.issuer_url,"activationCode":e.activation_code,"batchId":e.expected.batch_id,"scope":e.expected.scope,"bucket":e.expected.bucket,"archiveEndAt":e.expected.archive_end_at});
        let paths=super::super::storage::DataPaths{root:t.path().to_owned(),data_dir:t.path().join("data"),db_path:t.path().join("data/db.sqlite"),shots_dir:t.path().join("data/shots"),exports_dir:t.path().join("exports")};paths.ensure_dirs().unwrap();
        let mut conn=rusqlite::Connection::open(&paths.db_path).unwrap();super::super::storage::migrations::run_migrations(&mut conn).unwrap();
        super::super::archive::initialize_owner(&paths,&conn,&c.progress.lock().task_id).unwrap();
        assert!(super::super::internal::start_local_policy(&paths,&c,false,Some(&preset.to_string())).is_err());assert!(!t.path().join(FILE).exists());assert!(!c.tracking.load(Ordering::SeqCst));
        super::super::internal::start_local_policy(&paths,&c,true,Some(&preset.to_string())).unwrap();let e=enrollment(t.path()).unwrap();let expected_secret=e.device_secret.clone();
        let body=serde_json::json!({"grant":serde_json::from_str::<serde_json::Value>(&raw).unwrap()}).to_string();
        let plan=DateTime::parse_from_rfc3339(&e.expected.archive_end_at).unwrap().timestamp_millis();
        let renewed=serde_json::json!({"grant":serde_json::from_str::<serde_json::Value>(&grant(&e.device_id,"device",60000,plan)).unwrap()}).to_string();
        let server=std::thread::spawn(move||{
            for i in 0..5 {
                let (tcp,_)=listener.accept().unwrap();tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut stream=StreamOwned::new(ServerConnection::new(tls.clone()).unwrap(),tcp);let mut bytes=Vec::new();
                loop {let mut buf=[0;2048];let n=stream.read(&mut buf).unwrap();assert!(n>0);bytes.extend_from_slice(&buf[..n]);
                    if let Some(end)=bytes.windows(4).position(|v|v==b"\r\n\r\n") {let h=String::from_utf8_lossy(&bytes[..end]);let len=h.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|n|n.trim().parse::<usize>().unwrap())).unwrap();if bytes.len()>=end+4+len {break;}}
                }
                let req=String::from_utf8(bytes).unwrap();
                if i<2 {assert!(req.starts_with("POST /v1/activate"));assert!(req.contains(&expected_secret));assert!(req.contains(&"d".repeat(64)));}
                else {assert!(req.starts_with("POST /v1/renew"));assert!(req.contains(&format!("Bearer {}",expected_secret)));assert!(!req.contains(&"d".repeat(64)));}
                let (status,b)=if i==0||i==3 {("503 Service Unavailable","{}".to_string())}else if i==4 {assert!(req.contains("\"force\":true"));("200 OK",renewed.clone())}else{("200 OK",body.clone())};
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",b.len()).unwrap();stream.flush().unwrap();
            }
        });
        let client=reqwest::blocking::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(5)).add_root_certificate(reqwest::Certificate::from_der(cert.cert.der()).unwrap()).build().unwrap();
        let closed=TcpListener::bind("127.0.0.1:0").unwrap();let closed_addr=closed.local_addr().unwrap();drop(closed);
        let mut offline=enrollment(t.path()).unwrap();offline.issuer_url=format!("https://{closed_addr}/");super::super::internal::write_json_atomic(&t.path().join(FILE),&offline).unwrap();
        assert!(refresh_with_client(t.path(),&c,true,&client).unwrap_err().contains("连接失败"));
        super::super::internal::write_json_atomic(&t.path().join(FILE),&e).unwrap();
        assert!(refresh_with_client(t.path(),&c,true,&client).is_err());assert!(enrollment(t.path()).unwrap().activation_code.is_some());
        c.pause(true).unwrap();c.edit_progress(|p|{p.needs_flush=false;Ok(())}).unwrap();super::super::frame_upload::bind_local_retention(&paths,&c).unwrap();
        let binding=super::super::archive::load(t.path()).unwrap().unwrap();assert!(binding.effective_end_ms<binding.planned_end_ms);
        refresh_with_client(t.path(),&c,true,&client).unwrap();assert!(enrollment(t.path()).unwrap().activation_code.is_none());
        refresh_with_client(t.path(),&c,true,&client).unwrap();
        let old=oss_direct::load(t.path(),&c.config.read()).unwrap();assert!(!refresh_due(&old.expires_at,Utc::now().timestamp_millis()));
        assert!(matches!(recover_with_client(t.path(),&c,&old,&client),Recovery::RetryLater));
        assert!(pending_refresh(t.path())); // Forced refresh survives in the private JSON.
        refresh_with_client(t.path(),&c,false,&client).unwrap(); // Ignores >24h local validity after OSS403.
        assert!(!pending_refresh(t.path()));assert_ne!(old.fingerprint(),oss_direct::load(t.path(),&c.config.read()).unwrap().fingerprint());
        server.join().unwrap();
        assert!(oss_direct::load(t.path(),&c.config.read()).is_ok());assert!(!c.tracking.load(Ordering::SeqCst));
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(t.path().join(FILE)).unwrap().permissions().mode()&0o077,0);}
        assert!(!serde_json::to_string(&summary(t.path(),&c)).unwrap().contains(&enrollment(t.path()).unwrap().device_secret));
    }
    #[test] fn renew_threshold_and_strict_issuer() {
        let n=Utc::now().timestamp_millis();
        assert!(!refresh_due(&DateTime::<Utc>::from_timestamp_millis(n+DAY+1).unwrap().to_rfc3339(),n));
        assert!(refresh_due(&DateTime::<Utc>::from_timestamp_millis(n+DAY).unwrap().to_rfc3339(),n));
        assert!(refresh_due("bad",n));
        for bad in ["http://localhost/","https://u:p@example.com/","https://example.com/v1/","https://example.com/?token=x"] {assert!(url(bad).is_err(),"{bad}");}
    }
    #[test] fn automatic_default_stop_and_restart_do_not_start_capture() {
        let t=tempfile::tempdir().unwrap();let c=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();
        assert!(c.config.read().auto_upload_enabled);assert!(!auto_ready(&c.config.read(),&c.progress.lock()));
        set_auto(&c,false).unwrap();let d=InternalControl::load(t.path(),Arc::new(AtomicBool::new(false))).unwrap();
        assert!(!d.config.read().auto_upload_enabled);assert!(!d.tracking.load(Ordering::SeqCst));
        set_auto(&d,true).unwrap();super::super::internal::fill_test_participant_name(&d);
        assert!(auto_ready(&d.config.read(),&d.progress.lock()));
        let mut p=d.progress.lock().clone();p.integrity_error=Some("audit".into());assert!(!auto_ready(&d.config.read(),&p));
    }
}
