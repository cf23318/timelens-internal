import "../desktop.css";
import { useEffect, useRef, useState } from "react";
import { NavLink, Outlet, Link, useLocation } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useAppStore } from "../stores/appStore";
import { useDevModeStore } from "../stores/devModeStore";
import { useInternalStore, useInternalUpdates } from "../stores/internalStore";
import { duration, shotTime, taskOverviewLabel, captureLabel, useDesktopText } from "../lib/desktop";
import { TaskControls } from "../components/TaskControls";
import { DesktopError } from "../components/DesktopFeedback";
import { DesktopPageBoundary } from "../components/DesktopPageBoundary";

export default function AppShell() {
  useInternalUpdates();
  const { t } = useTranslation();
  const { english, text } = useDesktopText();
  const location = useLocation();
  const { date, setDate, error: appError, clearError } = useAppStore();
  const devEnabled = useDevModeStore(s => s.enabled);
  const { data, error, loadError, message, draft, refresh } = useInternalStore();
  const [refreshing, setRefreshing] = useState(false);
  const lastRefresh = useRef(0);
  const datedPage = ["/timeline", "/replay"].some(p => location.pathname.startsWith(p));
  useEffect(() => {
    if (!datedPage || Date.now() - lastRefresh.current < 30000) return;
    lastRefresh.current = Date.now(); setRefreshing(true);
    void useAppStore.getState().refreshSessions().finally(() => setRefreshing(false));
  }, [location.pathname, datedPage]);
  const nav = [
    { to: "/task", title: text("采集任务", "Collection task"), icon: "▣", sub: text("进度、权限与上传", "Progress, permissions and upload") },
    { to: "/timeline", title: t("nav.timeline"), icon: "≡", sub: t("nav.timelineDesc") },
    { to: "/replay", title: t("nav.timeReplay"), icon: "▷", sub: t("nav.timeReplayDesc") },
    { to: "/settings", title: t("nav.settings"), icon: "⚙", sub: t("nav.settingsDesc") },
    { to: "/about", title: t("nav.about"), icon: "ⓘ", sub: text("内部版本与开源说明", "Internal build and attribution") },
  ];
  const current = nav.find(n => location.pathname.startsWith(n.to));
  const employee=Boolean(data?.onboarding?.employeeMode);
  const p = data?.progress;
  const expired=Boolean(data?.archive && (data.archive.deleted || Date.now()>=data.archive.deadlineMs));
  const percentage = p ? Math.min(100, Math.max(0, p.elapsedMs / Math.max(1, p.targetMs) * 100)) : 0;
  return <div className="tl-app desktop-app">
    <aside className="desktop-sidebar" aria-label={t("nav.mainNav")}>
      <Link className="desktop-brand" to="/task"><span className="desktop-logo" aria-hidden="true">T</span><span><strong>TimeLens</strong><small>{text("屏幕记录 · 内部版", "Screen capture · Internal")}</small></span></Link>
      <div className="desktop-nav-label">{text("工作空间", "Workspace")}</div>
      <nav className="desktop-nav" aria-label={t("nav.featureModules")}>
        {nav.map(n => <NavLink key={n.to} to={n.to} className={({ isActive }) => `desktop-nav-link ${isActive ? "is-active" : ""}`}><span aria-hidden="true">{n.icon}</span>{n.title}{n.to === "/settings" && draft && <span className="desktop-unsaved-dot" aria-label={text("有未保存设置", "Unsaved settings")} />}</NavLink>)}
        {devEnabled && !employee && <><div className="desktop-nav-label">{text("开发工具", "Developer tools")}</div>{[["/sessions", t("nav.sessions")], ["/ocr", t("nav.ocrSearch")], ["/health", t("nav.health")]].map(([to,label]) => <NavLink key={to} to={to} className="desktop-nav-link">{label}</NavLink>)}</>}
      </nav>
      <div className="desktop-sidebar-bottom">
        <div className="flex justify-between gap-2"><span className="desktop-caption">{text("任务进度", "Task progress")}</span><strong className="text-xs tabular-nums">{percentage.toFixed(1)}%</strong></div>
        <div className="desktop-progress mt-3"><span style={{ width: `${percentage}%` }} /></div>
        <p className="desktop-caption mt-3">{p ? `${text("剩余", "Remaining")} ${duration(Math.ceil(Math.max(0,p.targetMs-p.elapsedMs)/60000)*60000, english)}` : text("正在读取任务…", "Loading task…")}</p>
        <Link to={employee ? "/task" : "/settings?tab=admin"} className="desktop-server-state"><span className={`desktop-dot ${data && !data.uploadMissing.length ? "is-green" : "is-amber"}`} />{employee ? text("记录自动同步", "Records sync automatically") : data && !data.uploadMissing.length ? text("上传配置就绪", "Upload configured") : text("上传配置待完成", "Upload setup incomplete")}</Link>
        <p className="desktop-caption text-[11px] mt-3">{text("关闭窗口后仍在后台运行", "Closing the window keeps the app running")}</p>
      </div>
    </aside>
    <div className="desktop-workspace">
      <header className="desktop-toolbar">
        <div className="min-w-0"><h1>{current?.title ?? "TimeLens"}</h1><p>{current?.sub}</p></div>
        <div className="flex items-center gap-3 shrink-0">
          <span className={`desktop-status ${!expired && p?.status === "recording" ? "is-recording" : ""}`} role="status"><span className={`desktop-dot ${!expired && p?.status === "recording" ? "is-green" : ""}`} />{data ? taskOverviewLabel(data, english) : text("读取中", "Loading")}</span>
          {draft && <Link to="/settings" className="desktop-text-link text-xs">{text("设置未保存", "Unsaved settings")}</Link>}
          <TaskControls />
        </div>
      </header>
      {datedPage && <div className="desktop-page-tools"><label htmlFor="workspace-date">{text("查看日期", "Date")}</label><input id="workspace-date" type="date" value={date} onChange={e => setDate(e.target.value)} className="desktop-input"/><button className="desktop-button" disabled={refreshing} onClick={() => { setRefreshing(true); void Promise.all([useAppStore.getState().refreshSessions(), refresh()]).finally(() => setRefreshing(false)); }}>{refreshing ? text("刷新中…", "Refreshing…") : text("刷新", "Refresh")}</button></div>}
      {(error || loadError || appError) && <div className="desktop-global-notice"><DesktopError employee={employee} error={error || loadError || appError || ""} onRetry={() => { clearError(); void refresh(); }} onDismiss={() => { clearError(); useInternalStore.setState({ error: "", loadError: "" }); }}/></div>}
      {message && !(error || loadError || appError) && <div className="desktop-global-success" role="status"><span>✓ {message}</span><button className="desktop-icon-button" aria-label={text("关闭提示", "Dismiss notice")} onClick={() => useInternalStore.setState({ message: "" })}>×</button></div>}
      <main className="desktop-main"><DesktopPageBoundary key={location.pathname} resetKey={location.search}>{expired && !["/task","/settings","/about"].some(path=>location.pathname.startsWith(path)) ? <div className="desktop-card"><h2>{text("原始记录已到删除截止", "Original records have expired")}</h2><p>{text("已停止展示原始记录。本机清理状态见采集任务；云端清理由接收服务独立执行。", "Original records are no longer displayed. See the task for local cleanup; receiver cleanup runs separately.")}</p><Link to="/task">{text("查看清理状态", "View cleanup status")}</Link></div> : <Outlet />}</DesktopPageBoundary></main>
      <footer className="desktop-statusbar"><span>{data ? `${data.snapshots.count} ${text("张截图", "frames")} · ${text("最近入库截图", "Latest stored frame")} ${shotTime(data.snapshots.latestAtMs,english)}` : text("正在读取本地数据…", "Loading local data…")}</span><span className="truncate" title={!expired && p?.status === "recording" ? captureLabel(data?.captureNotice.code ?? "", english) : undefined}>{expired ? text("原始记录已到删除截止", "Original records have expired") : p?.status === "recording" ? `${text("最近尝试", "Last attempt")}: ${captureLabel(data?.captureNotice.code ?? "",english)}` : p?.status === "uploaded" ? text("云端清单已对账，按期保留和删除", "Remote manifest reconciled; retention deadline applies") : text("当前未采集屏幕", "Screen collection is inactive")}</span></footer>
    </div>
  </div>;
}
