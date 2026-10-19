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
}

export const getInternalConfig = () => invoke<InternalConfig>("get_internal_config");
export const getInternalProgress = () => invoke<TaskProgress>("get_internal_progress");
export const saveInternalConfig = (config: InternalConfig) => invoke<void>("save_internal_config", { config });
export const finishInternalTask = () => invoke<void>("finish_internal_task");
export const retryInternalUpload = () => invoke<void>("retry_internal_upload");

export const taskStatusLabel: Record<TaskProgress["status"], string> = {
  ready: "待开始", recording: "录制中", paused: "已暂停", completed: "已停止，待回收", uploading: "正在加密回收", uploaded: "已上传并清理本地采集数据",
};

export function remainingTime(p: TaskProgress): string {
  const minutes = Math.ceil(Math.max(0, p.targetMs - p.elapsedMs) / 60000);
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分钟`;
}
