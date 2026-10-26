import { Link } from "react-router-dom";
import { useInternalStore } from "../stores/internalStore";
import { useAppStore } from "../stores/appStore";
import { useDesktopText } from "../lib/desktop";
import * as api from "../services/tauri";
import * as internal from "../services/internal";

export function TaskControls() {
  const { text } = useDesktopText();
  const { data, busy, draft, participantDraft, act } = useInternalStore();
  const permissions = useAppStore(s => s.permissions);
  if (!data) return <button className="desktop-button desktop-button-primary" disabled>{text("读取状态中…", "Loading…")}</button>;
  const p = data.progress;
  const employee=Boolean(data.onboarding?.employeeMode);
  const name=(participantDraft??data.config.participantName??"").trim();
  const expired = Boolean(data.archive && (data.archive.deleted || Date.now() >= data.archive.deadlineMs));
  const pausing = p.status === "recording" || (p.needsFlush && !["uploading", "uploaded"].includes(p.status));
  const permissionReady = permissions?.accessibilityGranted && permissions?.screenRecordingGranted;
  const startAllowed = !expired && ["ready", "paused"].includes(p.status) && p.elapsedMs < p.targetMs && !p.needsFlush && !p.integrityError && permissionReady && !draft && (!employee||Boolean(name));
  const reason = employee&&!name ? text("请填写姓名", "Enter your name") : draft ? text("请先保存或放弃未保存的设置", "Save or discard pending settings first") : !permissionReady ? text("请先完成系统权限检查", "Complete system permissions first") : text("当前任务状态不能继续采集", "This task cannot resume in its current state");
  const start=async()=>{if(employee){await internal.startEmployeeTask(name);useInternalStore.setState({participantDraft:null});}else{await api.startTracking();}};
  return <div className="flex items-center gap-2">
    {!pausing && !permissionReady && ["ready", "paused"].includes(p.status) && <Link className="desktop-text-link text-xs" to="/task#permissions">{text("检查权限", "Check permissions")}</Link>}
    <button className={`desktop-button ${pausing ? "" : "desktop-button-primary"}`} disabled={busy || (!pausing && !startAllowed)} title={!pausing && !startAllowed ? reason : undefined}
      onClick={() => void act(pausing ? api.stopTracking : start, pausing ? text("已暂停采集", "Collection paused") : text("已开始采集；记录将自动同步", "Collection started; records sync automatically"))}>
      {busy ? text("处理中…", "Working…") : pausing ? (p.needsFlush && p.status !== "recording" ? text("重试保存暂停", "Retry pause and save") : text("暂停采集", "Pause collection")) : expired ? text("任务已到期", "Task expired") : p.status === "paused" ? text("继续采集", "Resume collection") : text("开始采集", "Start collection")}
    </button>
  </div>;
}
