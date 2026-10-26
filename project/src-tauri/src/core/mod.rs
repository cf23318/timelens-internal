pub mod aggregation;
pub mod browser;
pub mod collection;
pub mod desktop;
pub mod external_ai;
pub mod intent_builtin_catalog;
pub mod intent_mapping;
pub mod internal;
pub mod models;
pub mod nudge;
pub mod ocr;
pub mod privacy;
pub mod replay;
pub mod settings;
pub mod transfer; // Legacy encrypted bundle reader; new tasks use frame_upload.
pub mod frame_upload;
pub mod recovery;
pub mod archive;
pub mod writer;

pub mod acquisition;

pub mod storage;
pub mod time_range;

pub mod oss_direct;
pub mod oss_manifest;
pub mod cloud_auth;
pub mod preset;
