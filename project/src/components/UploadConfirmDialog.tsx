import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useInternalStore } from "../stores/internalStore";
import { useDesktopText, missingLabel } from "../lib/desktop";
import * as internal from "../services/internal";

export function UploadConfirmDialog({ onClose, background = false }: { onClose: () => void; background?: boolean }) {
  const { text, english } = useDesktopText();
  const { data, busy, act } = useInternalStore();
  const dialog = useRef<HTMLDialogElement>(null);
  const initial = useRef(data);
  const [acknowledged, setAcknowledged] = useState(false);
  const [localError, setLocalError] = useState("");
  useEffect(() => { dialog.current?.showModal(); return () => { dialog.current?.close(); }; }, []);
  if (!data || !initial.current) return null;
  const original = initial.current;
  const direct = original.config.uploadMode === "ossDirect";
  const legacy = Boolean(original.progress.receipt || original.progress.storedBundle || original.progress.bundlePath);
  const confirmation: internal.UploadConfirmation = {
    participantName: original.config.participantName ?? "",
    uploadMode: original.config.uploadMode ?? "receiver", grantFingerprint: direct ? original.ossGrant?.fingerprint ?? "" : "",
    taskId: original.progress.taskId, uploadUrl: original.config.uploadUrl,
    encryptionRecipient: original.config.encryptionRecipient, domesticServerConfirmed: original.config.domesticServerConfirmed,
    cleanupOnly: false, surveyId: original.config.uploadSurveyId, deviceId: original.config.uploadDeviceId, archiveEndAt: original.config.archiveEndAt,
  };
  const stale = data.progress.taskId !== confirmation.taskId || data.progress.status !== original.progress.status || (direct && data.ossGrant?.fingerprint !== original.ossGrant?.fingerprint) || JSON.stringify(data.config) !== JSON.stringify(original.config);
  const blocked = Boolean(data.archive && (data.archive.deleted || Date.now() >= data.archive.deadlineMs)) || legacy || (background && data.uploadActive) || stale || Boolean((data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError) || data.uploadMissing.length > 0;
  const confirm = async () => {
    if (blocked || !acknowledged || busy) return;
    setLocalError("");
    const ok = await act(() => background ? internal.startFrameUpload(confirmation) : original.progress.status === "completed" ? internal.retryInternalUpload(confirmation) : internal.finishInternalTask(confirmation), direct ? text("OSS直传已启动，本地图片按任务留存期限保留", "Direct OSS upload started. Local retention applies.") : text("逐张上传已启动，按归档期限保留", "Frame upload started. The archive retention deadline applies."));
    if (ok) onClose(); else setLocalError(text("未能启动，请关闭此窗口查看错误详情。", "Could not start. Close this dialog to review the error."));
  };
  return <dialog ref={dialog} className="desktop-dialog" aria-labelledby="upload-confirm-title" onCancel={e => { if (busy) e.preventDefault(); else onClose(); }}>
    <div className="desktop-dialog-header"><div><div className="desktop-eyebrow">{text("操作确认", "Confirmation")}</div><h2 id="upload-confirm-title">{background ? text("启用本次运行的后台上传", "Enable upload for this run") : text("结束采集并上传", "End collection and upload")}</h2></div><button className="desktop-icon-button" aria-label={text("关闭", "Close")} onClick={onClose} disabled={busy}>×</button></div>
    <div className="desktop-dialog-body">
      <p>{background ? text("确认后开始逐张上传队列中的截图，并继续上传本次运行新保存的截图。不会自动开始采集；重启后需要重新确认上传。", "Uploads queued and newly saved frames during this run. Collection does not start automatically; restart requires upload confirmation again.") : text("确认后结束采集，将队列中的原始 WebP 逐张通过 HTTPS 上传。不制作加密包；结束后的任务不能继续采集。", "Ends collection and uploads queued original WebP images over HTTPS, without an encrypted package. An ended task cannot resume collection.")}</p>
      <dl className="desktop-review-list">
        {direct && <div><dt>{text("当前姓名", "Current name")}</dt><dd>{confirmation.participantName || text("未填写", "Not entered")}<small className="desktop-caption block">{text("已保存截图沿用原姓名目录，旧记录可能没有姓名。", "Saved frames keep their original name folders; legacy records may be unnamed.")}</small></dd></div>}
        <div><dt>{text("任务编号", "Task ID")}</dt><dd className="font-mono text-xs break-all">{confirmation.taskId}</dd></div>
        <div><dt>{text("数据范围", "Contents")}</dt><dd>{text("新上传队列中的原始截图、姓名目录，以及调研/设备编号、时间、显示器编号、大小和校验值。活动数据库不上传。", "Original queued screenshots, name folders and survey/device IDs, timestamps, display IDs, sizes and checksums. The activity database is not uploaded.")}</dd></div>
        <div><dt>{text("当前队列", "Current queue")}</dt><dd>{text("待传", "Pending")} {original.uploads.pending} · {text("已确认", "Verified")} {original.uploads.verified} · {direct && <>{text("OSS已接收", "OSS accepted")} {original.uploads.ossAccepted ?? 0} · </>}{text("需处理", "Needs review")} {original.uploads.blocked}</dd></div>
        <div><dt>{text("未加入队列的旧截图", "Legacy images excluded")}</dt><dd>{original.uploads.legacy}</dd></div>
        <div><dt>{text("已保存目的地", "Saved destination")}</dt><dd className="break-all">{confirmation.uploadUrl || text("尚未配置", "Not configured")}</dd></div>
        <div><dt>{text("批准调研结束", "Approved study end")}</dt><dd>{confirmation.archiveEndAt}</dd></div>
        {direct && <><div><dt>{text("授权用户 / 截止", "Grant user / expiry")}</dt><dd>{original.ossGrant?.userId} / {original.ossGrant?.expiresAt}</dd></div><div><dt>{text("固定上传目录", "Upload prefix")}</dt><dd className="break-all">{original.ossGrant?.prefix}</dd></div></>}
        <div><dt>{text(direct ? "用户 / 设备" : "调研 / 设备", direct ? "User / device" : "Survey / device")}</dt><dd>{confirmation.surveyId} / {confirmation.deviceId}</dd></div>
      </dl>
      <div className="desktop-alert desktop-alert-info">{direct ? text("截图直接上传到上述OSS目录；上传响应及CRC通过后显示“OSS已接收”，不代表云端清单对账。本机按任务结束/批准结束较早者加30天清理，OSS及下载副本由管理员另行安排到期删除。7天授权到期仅停止上传，不删除待传图片。", "Images go directly to the shown OSS prefix. CRC-checked acceptance is distinct from inventory reconciliation. Local records expire 30 days after the earlier actual/approved end. Administrators arrange OSS and downloaded-copy cleanup. Grant expiry only stops upload.") : text("确认后绑定本任务的归档和删除策略：实际结束或批准结束取较早者，最多保留30天，补传不延期。本机原始及派生记录、OSS和受管恢复副本到期删除；旧数据目录不自动接管。", "Confirmation enrolls this task for archiving and deletion within 30 days after the earlier actual/approved end. Late uploads do not extend retention. Local records, OSS and managed restores expire together. Legacy profiles are not adopted.")}</div>
      {blocked && <div className="desktop-alert desktop-alert-warning" role="status">{legacy ? text("该任务已有旧版加密包或回执，请先人工核对。", "This task has a legacy bundle or receipt requiring review.") : background && data.uploadActive ? text("后台上传已开启。", "Background upload is already enabled.") : stale ? text("任务或配置已变化，请关闭后重新查看。", "The task or settings changed. Close and review again.") : (data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError ? text("数据保存或完整性检查未完成，不能开始上传。", "Pending writes or integrity checks prevent upload.") : <>{text("上传准备未完成：", "Incomplete setup: ")}{data.uploadMissing.map(c => missingLabel(c,english)).join("、")}。 <Link to="/settings?tab=admin" onClick={onClose} className="desktop-text-link">{text("检查管理员配置", "Review administrator settings")}</Link></>}</div>}
      <label className="desktop-checkbox-row"><input type="checkbox" checked={acknowledged} disabled={busy || blocked} onChange={e => setAcknowledged(e.target.checked)}/><span>{direct ? text("我已核对用户、设备、OSS目录及授权截止，了解上述上传与留存安排。", "I reviewed the user, device, OSS prefix, expiry and retention arrangements.") : text("我已核对上述信息，了解截图将发送到上述服务器，并同意本任务按上述期限自动删除。", "I reviewed the details and understand the destination and automatic deletion deadline for this task.")}</span></label>
      {localError && <p className="text-sm text-red-600" role="alert">{localError}</p>}
    </div>
    <div className="desktop-dialog-footer"><button className="desktop-button" onClick={onClose} disabled={busy} autoFocus>{text("取消", "Cancel")}</button><button className="desktop-button desktop-button-danger" disabled={busy || blocked || !acknowledged} onClick={() => void confirm()}>{busy ? text("处理中…", "Working…") : background ? text("确认开启后台上传", "Enable background upload") : text("确认结束并上传", "Confirm end and upload")}</button></div>
  </dialog>;
}
