import { useInternalStore } from "../stores/internalStore";
import { useAppStore } from "../stores/appStore";
import { useDesktopText } from "../lib/desktop";
import { SystemPermissionPanel } from "./SystemPermissionPanel";
import { TaskControls } from "./TaskControls";
import * as internal from "../services/internal";

export function EmployeeSetup() {
  const {data,participantDraft,busy}=useInternalStore();
  const permissions=useAppStore(s=>s.permissions);
  const {text}=useDesktopText();
  if(!data?.onboarding?.employeeMode)return null;
  const p=data.progress;
  const nameLocked=busy||data.uploadActive||p.status==="recording"||Boolean(data.archive)||Boolean(p.endedAtMs);
  const ready=permissions?.accessibilityGranted&&permissions?.screenRecordingGranted;
  return <section className="desktop-card desktop-onboarding" aria-label={text("首次使用", "Getting started")}>
    <div className="desktop-section-heading"><div><h2>{text("三步开始使用", "Start in three steps")}</h2><p>{text("完成系统授权、填写姓名，然后开始采集。其他由软件自动处理。", "Grant system access, enter your name and start. Everything else is automatic.")}</p></div></div>
    <div className="desktop-task-grid">
      <div id="permissions"><h3>{text("1. 系统权限", "1. System access")}</h3><div className="mt-3"><SystemPermissionPanel permissions={permissions} showMacosPermissionHelp={!ready}/></div></div>
      <div id="participant-name"><label className="desktop-field"><strong>{text("2. 姓名（必填）", "2. Name (required)")}</strong><input aria-label={text("姓名", "Name")} className="desktop-input mt-3" required maxLength={64} autoComplete="name" placeholder={text("请输入姓名", "Enter your name")} value={participantDraft??data.config.participantName??""} readOnly={nameLocked} onChange={e=>useInternalStore.setState({participantDraft:e.target.value})}/><small>{text("点击开始时自动保存，不需要单独保存或配置上传。", "Saved when you start. No separate save or upload setup is needed.")}</small></label>
        <h3 className="mt-6">{text("3. 开始采集", "3. Start collection")}</h3><div className="mt-3"><TaskControls/></div><p className="desktop-caption mt-3">{text("可随时暂停。暂时离线也可开始，联网后自动同步。", "Pause at any time. You can start offline; records sync when connected.")}</p>
      </div>
    </div>
  </section>;
}

export function EmployeeSyncStatus() {
  const {data,busy,act}=useInternalStore();const {text}=useDesktopText();
  if(!data)return null;
  const needsReview=Boolean(data.progress.integrityError)||data.uploads.blocked>0;
  const nameMissing=!data.config.participantName.trim();
  const enabled=data.config.autoUploadEnabled??true;
  return <section className="desktop-card"><h2>{text("自动同步", "Automatic sync")}</h2><p className="desktop-muted mt-3">{!enabled?text("自动同步已暂停，保留你的原有选择。", "Automatic sync is paused, preserving your choice."):needsReview?text("部分记录需要管理员核对，本机数据仍保留。", "Some records require administrator review; local data is retained."):nameMissing?text("填写姓名并开始后，软件会自动准备和同步记录。", "Enter your name and start; the app prepares and syncs automatically."):data.cloudIssuer?.lastError?text("暂时无法自动同步，本机数据仍保留；软件会继续检查，持续异常请联系管理员。", "Sync is unavailable. Local data is retained; the app keeps checking. Contact the administrator if it persists."):text("已设置自动同步，无需处理上传配置。", "Sync is automatic; no upload setup is needed.")}</p><p className="desktop-caption mt-4">{text("待同步", "Pending")} {data.uploads.pending} · {text("已接收", "Received")} {data.uploads.ossAccepted??0}</p>{!nameMissing&&data.onboarding?.configured&&<button className="desktop-button mt-4" disabled={busy} onClick={()=>void act(()=>internal.setAutomaticUpload(!enabled),enabled?text("已暂停同步", "Sync paused"):text("已恢复自动同步", "Automatic sync resumed"))}>{enabled?text("暂停同步", "Pause sync"):text("恢复自动同步", "Resume automatic sync")}</button>}</section>;
}
