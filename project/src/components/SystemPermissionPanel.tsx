import { useCallback, useEffect, useState } from "react";
import type { PermissionStatus } from "../types";
import * as api from "../services/tauri";
import { detectClientDesktopOs } from "../lib/platform";
import { useDesktopText } from "../lib/desktop";
import { useAppStore } from "../stores/appStore";
import { DesktopError } from "./DesktopFeedback";

type SystemPermissionPanelProps = {
  variant?: "badges" | "actions" | "both";
  className?: string;
  permissions?: PermissionStatus | null;
  onPermissionsChange?: (p: PermissionStatus) => void;
  showMacosPermissionHelp?: boolean;
  showNotifications?: boolean;
};
export function SystemPermissionPanel({ variant = "both", className = "", permissions: controlled, onPermissionsChange, showMacosPermissionHelp = true, showNotifications = false }: SystemPermissionPanelProps) {
  const { text } = useDesktopText();
  const [local, setLocal] = useState<PermissionStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const os = detectClientDesktopOs();
  const permissions = controlled !== undefined ? controlled : local;
  const refresh = useCallback(async () => {
    setBusy(true); setError("");
    try {
      const next = await api.checkPermissions();
      setLocal(next); onPermissionsChange?.(next); useAppStore.getState().setPermissions(next);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }, [onPermissionsChange]);
  useEffect(() => { if (controlled === undefined) void refresh(); }, [controlled, refresh]);
  const needsInitialCheck=controlled===null;
  useEffect(()=>{if(needsInitialCheck)void refresh();const onFocus=()=>{void refresh();};window.addEventListener("focus",onFocus);return()=>window.removeEventListener("focus",onFocus);},[needsInitialCheck,refresh]);
  const run = async (action: () => Promise<unknown>) => {
    setError(""); setBusy(true);
    try { await action(); await refresh(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const rows = [
    { id: "accessibility", label: os === "windows" ? text("前台窗口", "Foreground window") : text("辅助功能", "Accessibility"), description: text("用于识别当前应用和窗口", "Identifies the active application and window"), granted: permissions?.accessibilityGranted, open: api.openAccessibilitySettings },
    { id: "screen", label: text("屏幕录制", "Screen recording"), description: text("用于保存屏幕截图，不采集摄像头", "Saves screenshots; does not use the camera"), granted: permissions?.screenRecordingGranted, open: async () => { if (os !== "macos" || !await api.requestScreenCaptureAccess()) await api.openScreenRecordingSettings(); } },
    ...(showNotifications ? [{ id: "notifications", label: text("通知访问", "Notifications"), description: text("可选系统能力", "Optional system capability"), granted: permissions?.notificationListenerGranted, open: api.openNotificationSettings }] : []),
  ];
  return <div className={className}>
    {variant === "badges" ? <div className="flex flex-wrap gap-2">{rows.map(r => <span key={r.id} className="desktop-status">{r.granted ? "✓" : "!"} {r.label} · {permissions ? r.granted ? text("已就绪", "Ready") : text("待授权", "Permission needed") : text("检查中", "Checking")}</span>)}</div> : <>
      <div className="desktop-permission-list">{rows.map(r => <div key={r.id} className="desktop-permission-row"><span className={`desktop-permission-icon ${r.granted ? "is-ready" : ""}`} aria-hidden="true">{r.granted ? "✓" : "!"}</span><div className="min-w-0 flex-1"><strong>{r.label}</strong><p className="desktop-caption">{r.description}</p></div><span className="desktop-caption">{permissions ? r.granted ? text("已就绪", "Ready") : text("待授权", "Permission needed") : text("检查中…", "Checking…")}</span>{permissions && !r.granted && <button className="desktop-button" disabled={busy} onClick={() => void run(r.open)}>{r.id === "screen" && os === "macos" ? text("请求授权", "Request access") : text("打开设置", "Open settings")}</button>}</div>)}</div>
      <div className="flex flex-wrap items-center justify-between gap-3 mt-4"><p className="desktop-caption">{rows.every(r=>r.granted) ? text("系统权限已就绪。", "System access is ready.") : text("缺少权限时无法开始。授权后会自动检查，也可手动重新检查。", "Access is required. It is rechecked automatically after authorization, or you can recheck manually.")}</p><button className="desktop-button" disabled={busy} onClick={() => void refresh()}>{busy ? text("检查中…", "Checking…") : text("重新检查权限", "Recheck permissions")}</button></div>
      {showMacosPermissionHelp && os === "macos" && <details className="desktop-details mt-4"><summary>{text("已开启权限，仍无法使用？", "Permissions enabled but still unavailable?")}</summary><ol className="list-decimal pl-5 space-y-2 mt-3 desktop-caption"><li>{text("核对系统授权列表中的应用与当前运行的版本一致。更换安装包或签名后，可能需要重新授权。", "Confirm the authorized app matches the running build. A changed build or signature may need authorization again.")}</li><li>{text("在系统设置中完成授权，然后点击“重新检查权限”。", "Complete authorization in System Settings, then recheck permissions.")}</li><li>{text("若仍未生效，先暂停采集，正常退出后重新打开；必要时请管理员检查安装包和签名。", "If needed, pause collection, quit normally and reopen. Ask the administrator to check the build and signature.")}</li></ol></details>}
    </>}
    {error && <div className="mt-3"><DesktopError error={error} onRetry={() => void refresh()}/></div>}
  </div>;
}
