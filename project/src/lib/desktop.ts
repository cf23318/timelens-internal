import { useTranslation } from "react-i18next";
export { taskLabel, taskOverviewLabel, directFramesAccepted } from "./taskStatus";

export function useDesktopText() {
  const { i18n } = useTranslation();
  const english = i18n.language.startsWith("en");
  return { english, text: (zh: string, en: string) => english ? en : zh };
}
export function duration(ms: number, english = false): string {
  const minutes = Math.floor(Math.max(0, ms) / 60000);
  const hours = Math.floor(minutes / 60);
  return english ? `${hours}h ${minutes % 60}m` : `${hours} 小时 ${minutes % 60} 分钟`;
}
export function fileSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return bytes < 1024 ** 3 ? `${(bytes / 1024 ** 2).toFixed(1)} MB` : `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}
export function shotTime(ms: number | null, english = false): string {
  return ms == null ? (english ? "No saved frames" : "尚无截图") : new Date(ms).toLocaleString(english ? "en-AU" : "zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}
export function captureLabel(code: string, english = false): string {
  const labels: Record<string, [string, string]> = {
    privacy: ["命中隐私规则，已跳过截图", "Privacy rule matched; screenshot skipped"],
    idle: ["电脑空闲，已跳过截图", "Computer idle; screenshot skipped"],
    permission: ["缺少屏幕权限，已跳过截图", "Screen permission missing; screenshot skipped"],
    duplicate: ["画面相近，已去重", "Similar frame skipped"],
    capture_error: ["截图失败，请检查系统权限", "Screenshot failed; check permissions"],
    save_error: ["截图保存失败，请查看任务错误", "Screenshot could not be saved"],
    saved: ["截图已保存并入队", "Screenshot committed and queued"],
    queued: ["截图已进入保存队列", "Screenshot queued for saving"],
  };
  return labels[code]?.[english ? 1 : 0] ?? (english ? "Waiting for the next screenshot" : "等待下一次截图尝试");
}
export function missingLabel(code: string, english = false): string {
  const labels: Record<string, [string, string]> = {
    participantName: ["姓名（必填）", "Name (required)"],
    profile: ["全新且归属明确的调研数据目录", "Clean, task-owned study profile"],
    archiveEnd: ["批准调研结束时间（含时区）", "Approved study end with timezone"],
    survey: ["调研编号", "Survey ID"], device: ["设备编号", "Device ID"],
    ossGrant: ["有效的本机OSS上传授权", "Valid OSS upload grant for this device"],
    server: ["上传地址", "Upload endpoint"], key: ["加密公钥", "Encryption public key"],
    domestic: ["国内服务器确认", "Domestic server confirmation"], credential: ["管理员上传凭证", "Administrator upload credential"],
  };
  return labels[code]?.[english ? 1 : 0] ?? code;
}
export function friendlyError(error: string, english = false, employee = false): string {
  if (/姓名/.test(error)) return english ? employee ? "Enter a valid name on the collection task page." : "Enter a valid name in Settings → Administrator before collection or upload." : error;
  if (employee && /上传准备|尚未配置|凭证|激活|预置|服务器|配置/.test(error)) return english ? "Automatic setup is unavailable. The app keeps checking; contact the administrator if it persists. No upload settings are required." : "自动配置暂时不可用，软件会继续检查；持续异常请联系管理员，无需自行修改上传设置。";
  if (/权限|permission|accessibility|screen capture/i.test(error)) return english ? "Permission check failed. Review system permissions and retry." : "权限未满足，请在“任务与权限”中检查后重试。";
  if (/变化|stale/i.test(error)) return english ? "The task or configuration changed. Review it again before confirming." : "任务或配置已变化，请刷新并重新确认。";
  if (/网络|network|timeout|timed out|connection|dns/i.test(error)) return english ? "Connection failed. Check the server and network, then retry. Local data is retained." : "连接失败，请检查服务器和网络后重试。本地数据会保留。";
  if (/上传准备|尚未配置|凭证/i.test(error)) return english ? "Upload setup is incomplete. Review administrator settings. Local data is retained." : "上传准备尚未完成，请检查管理员配置。本地数据会保留。";
  if (/回收|上传期间|完整性|落盘|任务开始|小时|间隔|宽度|质量|规则|公钥|HTTPS|暂停/.test(error)) return english ? "This action is unavailable for the current task or settings. Review the details below." : error;
  return english ? "The action could not be completed. Review details and retry." : "操作未完成，请查看详情后重试。";
}
