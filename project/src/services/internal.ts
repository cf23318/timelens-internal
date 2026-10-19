import { invoke } from "@tauri-apps/api/core";

export interface InternalConfig {
  captureIntervalSecs: number;
  pollWidth: number;
  pollQuality: number;
  workdayHours: number;
  allDisplays: boolean;
  screenshotBlockedApps: string[];
  screenshotBlockedDomains: string[];
  uploadUrl: string;
  encryptionRecipient: string;
  domesticServerConfirmed: boolean;
}

export interface TaskProgress {
  taskId: string;
  elapsedMs: number;
  targetMs: number;
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
  ready: "待开始", recording: "录制中", paused: "已暂停", completed: "已停止，待回收", uploading: "正在加密回收", uploaded: "已上传并清理本地采集数据",
};

export function remainingTime(p: TaskProgress): string {
  const minutes = Math.ceil(Math.max(0, p.targetMs - p.elapsedMs) / 60000);
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分钟`;
}

export interface UploadConfirmation {
  taskId: string;
  uploadUrl: string;
  encryptionRecipient: string;
  domesticServerConfirmed: boolean;
  cleanupOnly: boolean;
}
export interface InternalOverview {
  config: InternalConfig;
  progress: TaskProgress;
  snapshots: { count: number; bytes: number; latestAtMs: number | null };
  captureNotice: { code: string; atMs: number };
  uploadMissing: string[];
}
export const getInternalOverview = () => invoke<InternalOverview>("get_internal_overview");
