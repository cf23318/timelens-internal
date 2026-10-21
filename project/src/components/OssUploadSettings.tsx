import { useRef, useState } from "react";
import { useInternalStore } from "../stores/internalStore";
import { useDesktopText } from "../lib/desktop";
import * as internal from "../services/internal";

export function OssUploadSettings({ locked }: { locked: boolean }) {
  const { data, draft, busy, act } = useInternalStore();
  const { text } = useDesktopText();
  const [content, setContent] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  if (!data) return null;
  const grant = data.ossGrant;
  const importText = async (raw: string) => {
    if (raw.length > 65536) { await act(async () => { throw new Error("授权文件超过64KB"); }, ""); return; }
    if (await act(() => internal.importOssUploadGrant(raw), text("授权已导入；上传仍需单独确认", "Grant imported. Upload still requires confirmation."))) setContent("");
  };
  return <>
    <section className="desktop-card">
      <h3>{text("本机固定设备 ID", "Fixed device ID")}</h3>
      <p className="desktop-muted mt-2">{text("支持设备专属授权或同批次公用授权。公用配置导入后按本机固定 ID 分目录保存；此 ID 在重启和续签后不变，更换数据目录会生成新 ID。", "Device-scoped or shared batch grants are supported. Shared grants use this fixed ID for a separate folder; replacing the profile creates a new ID.")}</p>
      <input aria-label={text("固定设备ID", "Fixed device ID")} className="desktop-input font-mono mt-4" readOnly value={data.fixedDeviceId ?? ""} onFocus={e => e.target.select()}/>
      <p className="desktop-caption mt-2">{text("用户编号由管理员分配并写入授权；设备 ID 不会自动识别员工姓名。", "The administrator assigns the user ID. Device IDs do not identify employee names.")}</p>
    </section>
    <section className="desktop-card">
      <h3>{text("导入 OSS 上传授权", "Import OSS upload grant")}</h3>
      <p className="desktop-muted mt-2">{text("选择管理员提供的授权文件，或粘贴完整授权内容。不要填写长期 AccessKey Secret。导入会保存目的地及用户编号，不会开始采集或上传。", "Choose the administrator's grant file or paste its contents. Never enter a long-term AccessKey Secret. Import saves the destination and user ID without starting collection or upload.")}</p>
      <input ref={fileInput} className="mt-4" aria-label={text("选择上传授权文件", "Choose upload grant file")} type="file" accept=".json,application/json" disabled={locked || busy || Boolean(draft)} onChange={e => { const file=e.target.files?.[0]; if (file) void (async () => { try { if (file.size>65536) throw new Error("授权文件超过64KB"); await importText(await file.text()); } catch(error) { await act(async()=>{throw error;}, ""); } finally { if(fileInput.current)fileInput.current.value=""; } })(); }}/>
      <label className="desktop-field mt-4">{text("粘贴授权内容（可选）", "Paste grant contents (optional)")}<textarea className="desktop-input font-mono text-xs" rows={4} autoComplete="off" spellCheck={false} value={content} disabled={locked || busy || Boolean(draft)} onChange={e=>setContent(e.target.value)}/></label>
      <button className="desktop-button mt-3" type="button" disabled={locked || busy || Boolean(draft) || !content.trim()} onClick={()=>void importText(content)}>{text("导入粘贴的授权", "Import pasted grant")}</button>
      {draft && <p className="desktop-caption mt-2">{text("请先保存或放弃未保存设置，再导入授权。", "Save or discard settings before importing.")}</p>}
      {grant?.valid ? <dl className="desktop-review-list mt-4">
        <div><dt>{text("授权范围", "Grant scope")}</dt><dd>{grant.scope === "shared" ? text("公用批次 · 按设备分目录", "Shared batch · device folders") : text("设备专属", "Device-scoped")}</dd></div>
        <div><dt>{grant.scope === "shared" ? text("批次编号", "Batch ID") : text("用户编号", "User ID")}</dt><dd>{grant.userId}</dd></div>
        <div><dt>{text("授权截止", "Grant expires")}</dt><dd>{new Date(grant.expiresAt!).toLocaleString()}</dd></div>
        <div><dt>{text("OSS 地址", "OSS destination")}</dt><dd className="break-all">{grant.endpoint}</dd></div>
        <div><dt>{text("本机上传目录", "Device upload prefix")}</dt><dd className="font-mono text-xs break-all">{grant.prefix}</dd></div>
        <div><dt>{text("批准调研结束", "Approved study end")}</dt><dd>{new Date(grant.archiveEndAt!).toLocaleString()}</dd></div>
      </dl> : <p className="desktop-caption mt-4" role="status">{grant?.error || text("尚未导入有效授权", "No valid grant imported")}</p>}
    </section>
    {grant?.valid && grant.scope === "shared" && <div className="desktop-alert desktop-alert-warning">{text("此配置可供同批次所有人使用。设备目录用于分组，不代表员工身份认证；持有配置的人也能向同批次其他设备目录写入，但不能读取、删除或覆盖对象。", "This batch grant is shared. Device folders group uploads without authenticating people or isolating writes. Holders cannot read, delete or overwrite objects.")}</div>}
    <div className="desktop-alert desktop-alert-info">{text("授权从签发时起最多7天，到期仅停止上传；续签须保留原用户、设备、Bucket和批准调研结束时间。图片直接发送到OSS，长期密钥仅留在管理员Mac。上传默认关闭，需另行确认。", "Grants last at most seven days from issuance. Expiry stops upload; renewal keeps the same user, device, bucket and approved end. Images go directly to OSS. Upload is off until explicitly confirmed.")}</div>
    <div className="desktop-alert desktop-alert-warning">{text("“OSS已接收”表示上传回执和CRC校验通过。新版授权还会同步任务清单；管理员需运行对账、原图回读及到期清理工具，保持管理机器在线。本机按原任务截止清理，旧接收服务不负责直传清理。", "OSS accepted means response and CRC checks passed. New grants also send task inventories. Administrators must run reconciliation, original readback and retention on an online management machine. Local retention still applies.")}</div>
  </>;
}
