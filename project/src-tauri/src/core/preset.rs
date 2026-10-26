//! Employee packages carry public registration settings or a legacy activation profile. The raw value
//! never goes to the WebView, status APIs, logs or BUILD-INFO output.
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::path::Path;
use super::internal::InternalControl;

const RAW:&str=include_str!(concat!(env!("OUT_DIR"),"/timelens-activation-preset.json"));
pub fn employee_mode()->bool {RAW!="null"}
pub(super) fn activation_raw()->Option<&'static str> {employee_mode().then_some(RAW)}
pub fn build_info()->Value {
    let credential=serde_json::from_str::<Value>(RAW).is_ok_and(|v|v["format"]=="timelens-cloudflare-activation-v1");
    let discovery=serde_json::from_str::<Value>(RAW).is_ok_and(|v|v["format"]=="timelens-cloudflare-discovery-v1");
    json!({"schema":1,"buildInfoProtocol":"timelens-build-inspection-v1","employeeMode":employee_mode(),"mode":if employee_mode(){"employee"}else{"generic-test"},"bootstrapCredentialEmbedded":credential,"configurationDiscovery":discovery,"presetFingerprint":if employee_mode(){format!("{:x}",Sha256::digest(RAW.as_bytes()))}else{String::new()}})
}
pub fn prepare(root:&Path,control:&InternalControl)->Result<(),String> {
    if employee_mode() {super::cloud_auth::prepare_preset(root,control,RAW)?;}Ok(())
}
pub fn summary(root:&Path,control:&InternalControl)->Value {
    let managed=employee_mode();
    let cloud=super::cloud_auth::summary(root,control);
    json!({"employeeMode":managed,"activationPending":managed && !super::oss_direct::summary(root,&control.config.read())["valid"].as_bool().unwrap_or(false),"configured":cloud.configured})
}

#[cfg(test)]
#[path="../../preset_build.rs"]
mod build_validation;
#[cfg(test)]
mod tests {
    #[test] fn public_build_info_never_contains_an_activation_credential() {
        let v=super::build_info();assert!(!v.to_string().contains("activationCode"));assert!(!v.to_string().contains("accessKeySecret"));
        assert_eq!(v["employeeMode"],super::employee_mode());
    }
}
