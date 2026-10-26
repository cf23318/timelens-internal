import { useRef, useState } from "react";
import { useInternalStore } from "../stores/internalStore";
import { useDesktopText } from "../lib/desktop";
import * as internal from "../services/internal";

export function OssUploadSettings({ locked }: { locked: boolean }) {
  const { data, draft, busy, act, edit } = useInternalStore();
  const { text } = useDesktopText();
  const [content, setContent] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  if (!data) return null;
  const grant = data.ossGrant;
  const name = (draft ?? data.config).participantName ?? "";
  const importText = async (raw: string) => {
    if (raw.length > 65536) { await act(async () => { throw new Error("授权文件超过64KB"); }, ""); return; }
    if (await act(() => JSON.parse(raw).format === "timelens-cloudflare-activation-v1" ? internal.activateCloudIssuer(raw) : internal.importOssUploadGrant(raw), text("配置已导入；自动上传开启时将发送待传截图", "Configuration imported. Pending frames will upload when automatic upload is enabled."))) setContent("");
  };
  return <>
    <section className="desktop-card">
      <h3>{text("姓名（必填）", "Name (required)")}</h3>
      <input aria-label={text("姓名", "Name")} className="desktop-input mt-4" required maxLength={64} autoComplete="name" placeholder={text("请输入姓名", "Enter your name")} value={name} disabled={locked || busy} onChange={e => edit({ participantName: e.target.value })}/>
      <p className="desktop-caption mt-2">{text("开始采集和上传前请填写姓名。姓名用于后续截图的OSS目录；修改姓名不会移动已有记录。", "Enter your name before collection and upload. It is included in new screenshot folders; changing it does not move existing records.")}</p>
      {name.trim() && <p className="desktop-caption font-mono break-all mt-2">{text("目录示例：", "Folder example: ")}…/tasks/{text("任务ID", "task-ID")}/{name.trim()}/YYYY/MM/DD/…</p>}
    </section>
    <section className="desktop-card">
      <h3>{text("本机固定设备 ID", "Fixed device ID")}</h3>
      <p className="desktop-muted mt-2">{text("支持设备专属授权或同批次公用授权。公用配置导入后按本机固定 ID 分目录保存；此 ID 在重启和续签后不变，更换数据目录会生成新 ID。", "Device-scoped or shared batch grants are supported. Shared grants use this fixed ID for a separate folder; replacing the profile creates a new ID.")}</p>
      <input aria-label={text("固定设备ID", "Fixed device ID")} className="desktop-input font-mono mt-4" readOnly value={data.fixedDeviceId ?? ""} onFocus={e => e.target.select()}/>
      <p className="desktop-caption mt-2">{text("用户编号由管理员分配并写入授权；设备 ID 不会自动识别员工姓名。", "The administrator assigns the user ID. Device IDs do not identify employee names.")}</p>
    </section>
    <section className="desktop-card">
      <h3>{text("激活云端自动续期", "Activate automatic renewal")}</h3>
      <p className="desktop-muted mt-2">{text("先保存姓名，再导入管理员提供的云端激活 JSON；首次激活后自动检查并续期。兼容离线 OSS 授权。上传默认开启，配置就绪后会发送待传截图；采集仍需手动开始。不要填写长期 AccessKey Secret。", "Save your name, then import the administrator's activation JSON. Grants renew automatically after activation. Offline grants remain supported. Upload is on by default and sends pending frames when ready; collection still starts manually. Never enter a long-term AccessKey Secret.")}</p>
      <input ref={fileInput} className="mt-4" aria-label={text("选择上传授权文件", "Choose upload grant file")} type="file" accept=".json,application/json" disabled={locked || busy || Boolean(draft)} onChange={e => { const file=e.target.files?.[0]; if (file) void (async () => { try { if (file.size>65536) throw new Error("授权文件超过64KB"); await importText(await file.text()); } catch(error) { await act(async()=>{throw error;}, ""); } finally { if(fileInput.current)fileInput.current.value=""; } })(); }}/>
      <label className="desktop-field mt-4">{text("粘贴授权内容（可选）", "Paste grant contents (optional)")}<textarea className="desktop-input font-mono text-xs" rows={4} autoComplete="off" spellCheck={false} value={content} disabled={locked || busy || Boolean(draft)} onChange={e=>setContent(e.target.value)}/></label>
      <button className="desktop-button mt-3" type="button" disabled={locked || busy || Boolean(draft) || !content.trim()} onClick={()=>void importText(content)}>{text("导入粘贴的授权", "Import pasted grant")}</button>
      {draft && <p className="desktop-caption mt-2">{text("请先保存或放弃未保存设置，再导入授权。", "Save or discard settings before importing.")}</p>}
      {grant?.valid ? <dl className="desktop-review-list mt-4">
        <div><dt>{text("授权范围", "Grant scope")}</dt><dd>{grant.scope === "shared" ? text("公用批次 · 按设备分目录", "Shared batch · device folders") : text("设备专属", "Device-scoped")}</dd></div>
        <div><dt>{grant.scope === "shared" ? text("批次编号", "Batch ID") : text("用户编号", "User ID")}</dt><dd>{grant.userId}</dd></div>
        <div><dt>{text("授权截止", "Grant expires")}</dt><dd>{new Date(grant.expiresAt!).toLocaleString()}</dd></div>
        <div><dt>{text("上传目的地", "Upload destination")}</dt><dd>{text("由上传授权自动指定，不展示存储地址", "Assigned by the upload grant; storage address is not displayed")}</dd></div>
        <div><dt>{text("上传权限", "Upload permission")}</dt><dd>{text("仅允许新增上传，不提供云端读取或下载授权", "New uploads only; no cloud read or download authorization")}</dd></div>
        <div><dt>{text("批准调研结束", "Approved study end")}</dt><dd>{new Date(grant.archiveEndAt!).toLocaleString()}</dd></div>
      </dl> : <p className="desktop-caption mt-4" role="status">{grant?.error || text("尚未导入有效授权", "No valid grant imported")}</p>}
    </section>
    {grant?.valid && grant.scope === "shared" && <div className="desktop-alert desktop-alert-warning">{text("此配置可供同批次所有人使用。设备目录用于分组，不代表员工身份认证；持有配置的人也能向同批次其他设备目录写入，但不能读取、删除或覆盖对象。", "This batch grant is shared. Device folders group uploads without authenticating people or isolating writes. Holders cannot read, delete or overwrite objects.")}</div>}
    <section className="desktop-card"><h3>{text("自动续期状态", "Automatic renewal status")}</h3><p className="desktop-muted mt-2">{data.cloudIssuer?.configured ? data.cloudIssuer.issuerUrl : text("未激活云端续期；离线授权到期后仍需更新。", "Online renewal is not activated; offline grants still need replacement at expiry.")}</p>{data.cloudIssuer?.lastCheckedMs > 0 && <p className="desktop-caption mt-2">{text("最近检查：", "Last checked: ")}{new Date(data.cloudIssuer.lastCheckedMs).toLocaleString()}</p>}{data.cloudIssuer?.lastError && <p role="status" className="desktop-caption mt-2">{data.cloudIssuer.lastError}</p>}</section>
    <div className="desktop-alert desktop-alert-info">{text("每份上传授权最多7天，云端激活后在到期前24小时自动续期；断网会保留队列并重试。图片直接发送到OSS；云端签发密钥保存在Cloudflare Secrets。续期不改变调研结束或删除期限。停止上传的选择会保留到重启后。", "Grants last at most seven days and renew within 24 hours of expiry after online activation. Offline queues are retained for retry. Images go directly to OSS; signing keys stay in Cloudflare Secrets. Renewal does not extend study or deletion deadlines. Stopping upload persists across restarts.")}</div>
    <div className="desktop-alert desktop-alert-warning">{text("“OSS已接收”表示上传回执和CRC校验通过。新版授权还会同步任务清单；管理员需运行对账、原图回读及到期清理工具，保持管理机器在线。本机按原任务截止清理，旧接收服务不负责直传清理。", "OSS accepted means response and CRC checks passed. New grants also send task inventories. Administrators must run reconciliation, original readback and retention on an online management machine. Local retention still applies.")}</div>
  </>;
}
