import type { InternalOverview } from "../services/internal";

export function taskLabel(status: InternalOverview["progress"]["status"], english = false): string {
  const labels = {
    ready: ["待开始", "Ready"], recording: ["采集中", "Recording"], paused: ["已暂停", "Paused"],
    completed: ["已结束 · 待上传", "Ended · Pending upload"], uploading: ["逐张上传中", "Uploading frames"],
    uploaded: ["归档清单已对账", "Archive manifest reconciled"],
  };
  return labels[status]?.[english ? 1 : 0] ?? (english ? "Unknown status" : "状态未知");
}
/** OSS acceptance is distinct from administrator reconciliation. */
export function directFramesAccepted(data: InternalOverview): boolean {
  const { progress: p, uploads: q, snapshots } = data;
  return data.config.uploadMode === "ossDirect" && p.status === "completed"
    && p.endedAtMs != null && !p.needsFlush && !p.integrityError
    && !p.receipt && !p.storedBundle && !p.bundlePath
    && q.pending === 0 && q.blocked === 0 && q.legacy === 0
    && snapshots.count > 0 && q.ossAccepted === snapshots.count;
}

/** Shared by the toolbar and task page so neither can show stale pending text. */
export function taskOverviewLabel(data: InternalOverview, english = false, now = Date.now()): string {
  if (data.archive?.deleted) return english ? "Expired and removed" : "已到期清理";
  if (data.archive && now >= data.archive.deadlineMs) return english ? "Expired · Cleanup pending" : "已到期 · 等待清理";
  if (directFramesAccepted(data)) {
    if (data.progress.lastError) return data.uploadActive
      ? (english ? "OSS accepted · Retrying inventory" : "OSS已接收 · 清单重试中")
      : (english ? "OSS accepted · Needs review" : "OSS已接收 · 需处理");
    return data.uploadActive
      ? (english ? "OSS accepted · Syncing inventory" : "OSS已接收 · 同步清单中")
      : (english ? "OSS accepted · Verification pending" : "OSS已接收 · 待核验");
  }
  if (data.config.uploadMode === "ossDirect" && data.progress.status === "completed") {
    if (data.uploads.blocked > 0) return english ? "Ended · Upload needs review" : "已结束 · 上传需处理";
    if (data.uploadActive) return data.progress.lastError
      ? (english ? "Ended · Retrying upload" : "已结束 · 自动重试中")
      : (english ? "Ended · Uploading" : "已结束 · 上传中");
  }
  return taskLabel(data.progress.status, english);
}
