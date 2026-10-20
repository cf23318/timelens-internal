import { invoke } from "@tauri-apps/api/core";

export interface InternalConfig {
  captureIntervalSecs: number;
  pollWidth: number;
  pollQuality: number;
  targetHours: number;
  allDisplays: boolean;
  screenshotBlockedApps: string[];
  screenshotBlockedDomains: string[];
  uploadMode: "receiver" | "ossDirect";
  uploadUrl: string;
  uploadSurveyId: string;
  uploadDeviceId: string;
  archiveEndAt: string;
  encryptionRecipient: string;
  domesticServerConfirmed: boolean;
}

export interface TaskProgress {
  taskId: string;
  elapsedMs: number;
  targetMs: number;
  endedAtMs: number | null;
  status: "ready" | "recording" | "paused" | "completed" | "uploading" | "uploaded";
  lastError: string | null;
  bundlePath: string | null;
  needsFlush: boolean;
  integrityError: string | null;
  receipt: Record<string, unknown> | null;
  storedBundle: Record<string, unknown> | null;
}

export const getInternalConfig = () => invoke<InternalConfig>("get_internal_config");
export const getInternalProgress = () => invoke<TaskProgress>("get_internal_progress");
export const saveInternalConfig = (config: InternalConfig) => invoke<void>("save_internal_config", { config });
export const finishInternalTask = (confirmation: UploadConfirmation) => invoke<void>("finish_internal_task", { confirmation });
export const retryInternalUpload = (confirmation: UploadConfirmation) => invoke<void>("retry_internal_upload", { confirmation });

export const taskStatusLabel: Record<TaskProgress["status"], string> = {
  ready: "待开始", recording: "录制中", paused: "已暂停", completed: "已停止，待上传", uploading: "正在逐张上传", uploaded: "归档清单已对账，按期删除",
};

export function remainingTime(p: TaskProgress): string {
  const minutes = Math.ceil(Math.max(0, p.targetMs - p.elapsedMs) / 60000);
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分钟`;
}

export interface UploadConfirmation {
  uploadMode: "receiver" | "ossDirect";
  grantFingerprint: string;
  taskId: string;
  uploadUrl: string;
  encryptionRecipient: string;
  domesticServerConfirmed: boolean;
  cleanupOnly: boolean;
  surveyId: string;
  deviceId: string;
  archiveEndAt: string;
}
export interface InternalOverview {
  fixedDeviceId: string;
  ossGrant: {valid:boolean; userId?:string; deviceId?:string; prefix?:string; endpoint?:string; expiresAt?:string; archiveEndAt?:string; fingerprint?:string; error?:string};
  config: InternalConfig;
  progress: TaskProgress;
  snapshots: { count: number; bytes: number; latestAtMs: number | null };
  captureNotice: { code: string; atMs: number };
  archive: {deadlineMs:number;effectiveEndMs:number;reconciled:boolean;deleted:boolean}|null;
  uploadMissing: string[];
  uploadActive: boolean;
  uploads: {pending:number; verified:number; ossAccepted:number; blocked:number; legacy:number; lastError:string|null};
}
export const getInternalOverview = () => invoke<InternalOverview>("get_internal_overview");

export const startFrameUpload=(confirmation:UploadConfirmation)=>invoke<void>("start_frame_upload",{confirmation});
export const stopFrameUpload=()=>invoke<void>("stop_frame_upload");

export const importOssUploadGrant=(grantJson:string)=>invoke<void>("import_oss_upload_grant",{grantJson});
