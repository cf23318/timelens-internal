import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as internal from "../services/internal";
import * as api from "../services/tauri";
import { SystemPermissionPanel } from "./SystemPermissionPanel";

export function InternalSettings() {
  const [config, setConfig] = useState<internal.InternalConfig | null>(null);
  const [progress, setProgress] = useState<internal.TaskProgress | null>(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [blockedAppsText, setBlockedAppsText] = useState("");
  const [blockedDomainsText, setBlockedDomainsText] = useState("");
  const [ocrEnabled, setOcrEnabled] = useState(false);
  useEffect(() => {
    let cancelled = false;
    Promise.all([internal.getInternalConfig(), internal.getInternalProgress(), api.getOcrSettings()]).then(([c, p, ocr]) => {
      if (!cancelled) { setConfig(c); setProgress(p); setOcrEnabled(ocr.enabled); setBlockedAppsText(c.screenshotBlockedApps.join("\n")); setBlockedDomainsText(c.screenshotBlockedDomains.join("\n")); }
    }).catch(e => { if (!cancelled) setError(String(e)); });
    const unlisten = listen<internal.TaskProgress>("internal_task_changed", e => { if (!cancelled) setProgress(e.payload); });
    return () => { cancelled = true; void unlisten.then(fn => fn()); };
  }, []);
  async function act(fn: () => Promise<unknown>, success: string) {
    setBusy(true); setError(""); setMessage("");
    try { await fn(); setProgress(await internal.getInternalProgress()); setMessage(success); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  if (!config || !progress) return <div className="p-5" role="status">{error || "正在读取采集任务…"}</div>;
  const locked = busy || progress.status === "recording" || progress.status === "uploading";
  const fieldClass = "rounded border border-[var(--tl-line)] bg-[var(--tl-panel)] p-2 text-[var(--tl-ink)]";
  const buttonClass = "rounded border border-[var(--tl-line)] px-4 py-2 disabled:opacity-40";
  const set = <K extends keyof internal.InternalConfig>(key: K, value: internal.InternalConfig[K]) => setConfig({ ...config, [key]: value });
  const lines = (text: string) => text.split(/\r?\n/).map(s => s.trim()).filter(Boolean);
  return <div className="space-y-6 p-5 text-[var(--tl-ink)]">
    <section className="space-y-3 rounded-lg border border-[var(--tl-line)] p-4">
      <h2 className="font-semibold">两工作日屏幕记录 · 内部版</h2>
      <p aria-live="polite">{internal.taskStatusLabel[progress.status]} · 剩余 {internal.remainingTime(progress)}</p>
      <p className="text-sm text-[var(--tl-muted)]">累计录制 {2 * config.workdayHours} 小时后自动停止。暂停、退出与系统睡眠不计时，重新打开后保持暂停。记录包括定时截图、应用与窗口信息、网址，以及复制粘贴流向估计；不保存按键内容或剪贴板正文。</p>
      <div className="flex flex-wrap gap-3">
        <button className={buttonClass} disabled={busy || !["ready", "paused"].includes(progress.status)} onClick={() => void act(api.startTracking, "已开始采集")}>开始 / 继续</button>
        <button className={buttonClass} disabled={busy || !(progress.status === "recording" || (progress.needsFlush && !["uploading", "uploaded"].includes(progress.status)))} onClick={() => void act(api.stopTracking, "已暂停采集")}>{progress.status !== "recording" ? "重试保存暂停" : "暂停"}</button>
        <button className={buttonClass} disabled={busy || !["ready", "recording", "paused"].includes(progress.status)} onClick={() => void act(internal.finishInternalTask, "已结束采集，开始回收")}>结束并回收</button>
        <button className={buttonClass} disabled={busy || progress.status !== "completed"} onClick={() => void act(internal.retryInternalUpload, "已启动回收")}>重试回收</button>
      </div>
      {progress.lastError && <p role="alert" className="text-amber-500">{progress.lastError}</p>}
      <p className="text-sm text-[var(--tl-muted)]">上传失败时保留本地数据，只有服务器确认接收完整文件后才清理。</p>
    </section>
    <SystemPermissionPanel />
    <label className="flex gap-2"><input type="checkbox" checked={ocrEnabled} disabled={busy || ["uploading", "uploaded"].includes(progress.status)} onChange={e => { const enabled = e.target.checked; void act(async () => { if (enabled) await api.setOcrPrivacyAcknowledged(true); const next = await api.setOcrSettings({ enabled, allowExportToAi: false }); setOcrEnabled(next.enabled); }, "本地 OCR 设置已保存"); }}/>启用本地 OCR 和识别文字脱敏（原始截图本身仍未打码）</label>
    <fieldset disabled={locked} className="space-y-4 rounded-lg border border-[var(--tl-line)] p-4 disabled:opacity-60">
      <legend className="px-2 font-semibold">采集设置（暂停后可修改）</legend>
      <div className="flex flex-wrap gap-4">
        <label className="flex flex-col gap-1">截图间隔（秒）<input className={fieldClass} type="number" min={10} max={15} value={config.captureIntervalSecs} onChange={e => set("captureIntervalSecs", Number(e.target.value))}/></label>
        <label className="flex flex-col gap-1">定时截图最大宽度<input className={fieldClass} type="number" min={1600} max={7680} value={config.pollWidth} onChange={e => set("pollWidth", Number(e.target.value))}/></label>
        <label className="flex flex-col gap-1">截图质量<input className={fieldClass} type="number" min={85} max={100} value={config.pollQuality} onChange={e => set("pollQuality", Number(e.target.value))}/></label>
        <label className="flex flex-col gap-1">每个工作日的小时数<input className={fieldClass} type="number" min={1} max={24} disabled={progress.elapsedMs > 0} value={config.workdayHours} onChange={e => set("workdayHours", Number(e.target.value))}/></label>
      </div>
      <label className="flex gap-2"><input type="checkbox" checked={config.allDisplays} onChange={e => set("allDisplays", e.target.checked)}/>采集所有显示器（默认只采集前台窗口所在屏幕）</label>
      <p className="text-sm text-[var(--tl-muted)]">切换窗口保留原始分辨率。所有屏幕模式会包含其他屏幕上的窗口；隐私规则按当前前台应用和网页判断。</p>
      <p className="text-sm text-[var(--tl-muted)]">间隔表示定时尝试截图；相近画面会去重，最多间隔约 60 秒保存一张。低于 1600 像素宽的原屏会放大，放大不会增加文字细节。</p>
      <label className="flex flex-col gap-1">不截图的应用（每行一项）<textarea className={fieldClass} rows={5} value={blockedAppsText} onChange={e => setBlockedAppsText(e.target.value)}/></label>
      <label className="flex flex-col gap-1">不截图的网站域名（每行一项，包含其子域名）<textarea className={fieldClass} rows={4} value={blockedDomainsText} onChange={e => setBlockedDomainsText(e.target.value)}/></label>
      <p className="text-sm text-[var(--tl-muted)]">默认不截图密码管理器、微信、钉钉和已列出的网银网站；应用时长和流向估计仍会记录。网银名单需要按实际使用补充。无法读取浏览器网址时跳过截图。截图本身尚未做敏感文字打码。</p>
      <label className="flex flex-col gap-1">国内服务器上传地址（HTTPS）<input className={fieldClass} type="url" value={config.uploadUrl} onChange={e => set("uploadUrl", e.target.value)}/></label>
      <label className="flex flex-col gap-1">接收服务器的加密公钥<input className={fieldClass} spellCheck={false} value={config.encryptionRecipient} onChange={e => set("encryptionRecipient", e.target.value)}/></label>
      <label className="flex gap-2"><input type="checkbox" checked={config.domesticServerConfirmed} onChange={e => set("domesticServerConfirmed", e.target.checked)}/>已由管理员确认该上传地址属于国内内部服务器</label>
      <button className={buttonClass} onClick={() => void act(() => internal.saveInternalConfig({ ...config, screenshotBlockedApps: lines(blockedAppsText), screenshotBlockedDomains: lines(blockedDomainsText) }), "设置已保存")}>保存设置</button>
    </fieldset>
    <p className="text-sm text-[var(--tl-muted)]">内部版已停用外部 AI、豆包自动粘贴和在线更新。Windows 使用空闲检测，不统计按键与点击次数。</p>
    {error && <p role="alert" className="text-red-500">{error}</p>}
    {message && <p role="status">{message}</p>}
  </div>;
}
