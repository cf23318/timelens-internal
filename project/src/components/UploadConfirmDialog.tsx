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
  const legacy = Boolean(original.progress.receipt || original.progress.storedBundle || original.progress.bundlePath);
  const confirmation: internal.UploadConfirmation = {
    taskId: original.progress.taskId, uploadUrl: original.config.uploadUrl,
    encryptionRecipient: original.config.encryptionRecipient, domesticServerConfirmed: original.config.domesticServerConfirmed,
    cleanupOnly: false, surveyId: original.config.uploadSurveyId, deviceId: original.config.uploadDeviceId,
  };
  const stale = data.progress.taskId !== confirmation.taskId || data.progress.status !== original.progress.status || JSON.stringify(data.config) !== JSON.stringify(original.config);
  const blocked = legacy || (background && data.uploadActive) || stale || Boolean((data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError) || data.uploadMissing.length > 0;
  const confirm = async () => {
    if (blocked || !acknowledged || busy) return;
    setLocalError("");
    const ok = await act(() => background ? internal.startFrameUpload(confirmation) : original.progress.status === "completed" ? internal.retryInternalUpload(confirmation) : internal.finishInternalTask(confirmation), text("逐张上传已启动，本地数据保留", "Frame upload started. Local data is retained."));
    if (ok) onClose(); else setLocalError(text("未能启动，请关闭此窗口查看错误详情。", "Could not start. Close this dialog to review the error."));
  };
  return <dialog ref={dialog} className="desktop-dialog" aria-labelledby="upload-confirm-title" onCancel={e => { if (busy) e.preventDefault(); else onClose(); }}>
    <div className="desktop-dialog-header"><div><div className="desktop-eyebrow">{text("操作确认", "Confirmation")}</div><h2 id="upload-confirm-title">{background ? text("启用本次运行的后台上传", "Enable upload for this run") : text("结束采集并上传", "End collection and upload")}</h2></div><button className="desktop-icon-button" aria-label={text("关闭", "Close")} onClick={onClose} disabled={busy}>×</button></div>
    <div className="desktop-dialog-body">
      <p>{background ? text("确认后开始逐张上传队列中的截图，并继续上传本次运行新保存的截图。不会自动开始采集；重启后需要重新确认上传。", "Uploads queued and newly saved frames during this run. Collection does not start automatically; restart requires upload confirmation again.") : text("确认后结束采集，将队列中的原始 WebP 逐张通过 HTTPS 上传。不制作加密包；结束后的任务不能继续采集。", "Ends collection and uploads queued original WebP images over HTTPS, without an encrypted package. An ended task cannot resume collection.")}</p>
      <dl className="desktop-review-list">
        <div><dt>{text("任务编号", "Task ID")}</dt><dd className="font-mono text-xs break-all">{confirmation.taskId}</dd></div>
        <div><dt>{text("数据范围", "Contents")}</dt><dd>{text("新上传队列中的原始截图，以及调研/设备编号、时间、显示器编号、大小和校验值。活动数据库不上传。", "Original queued screenshots and survey/device IDs, timestamps, display IDs, sizes and checksums. The activity database is not uploaded.")}</dd></div>
        <div><dt>{text("当前队列", "Current queue")}</dt><dd>{text("待传", "Pending")} {original.uploads.pending} · {text("已确认", "Verified")} {original.uploads.verified} · {text("需处理", "Needs review")} {original.uploads.blocked}</dd></div>
        <div><dt>{text("未加入队列的旧截图", "Legacy images excluded")}</dt><dd>{original.uploads.legacy}</dd></div>
        <div><dt>{text("已保存目的地", "Saved destination")}</dt><dd className="break-all">{confirmation.uploadUrl || text("尚未配置", "Not configured")}</dd></div>
        <div><dt>{text("调研 / 设备", "Survey / device")}</dt><dd>{confirmation.surveyId} / {confirmation.deviceId}</dd></div>
      </dl>
      <div className="desktop-alert desktop-alert-info">{text("每张图片都核对服务器回执。成功或失败均保留本地截图和活动数据库；旧截图不会自动加入新队列。", "Every receipt is verified. Successful and failed uploads retain local images and the activity database. Legacy screenshots are not automatically enrolled.")}</div>
      {blocked && <div className="desktop-alert desktop-alert-warning" role="status">{legacy ? text("该任务已有旧版加密包或回执，请先人工核对。", "This task has a legacy bundle or receipt requiring review.") : background && data.uploadActive ? text("后台上传已开启。", "Background upload is already enabled.") : stale ? text("任务或配置已变化，请关闭后重新查看。", "The task or settings changed. Close and review again.") : (data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError ? text("数据保存或完整性检查未完成，不能开始上传。", "Pending writes or integrity checks prevent upload.") : <>{text("上传准备未完成：", "Incomplete setup: ")}{data.uploadMissing.map(c => missingLabel(c,english)).join("、")}。 <Link to="/settings?tab=admin" onClick={onClose} className="desktop-text-link">{text("检查管理员配置", "Review administrator settings")}</Link></>}</div>}
      <label className="desktop-checkbox-row"><input type="checkbox" checked={acknowledged} disabled={busy || blocked} onChange={e => setAcknowledged(e.target.checked)}/><span>{text("我已核对上述信息，了解截图原图将发送到上述服务器。", "I reviewed the details and understand that original screenshots are sent to this server.")}</span></label>
      {localError && <p className="text-sm text-red-600" role="alert">{localError}</p>}
    </div>
    <div className="desktop-dialog-footer"><button className="desktop-button" onClick={onClose} disabled={busy} autoFocus>{text("取消", "Cancel")}</button><button className="desktop-button desktop-button-danger" disabled={busy || blocked || !acknowledged} onClick={() => void confirm()}>{busy ? text("处理中…", "Working…") : background ? text("确认开启后台上传", "Enable background upload") : text("确认结束并上传", "Confirm end and upload")}</button></div>
  </dialog>;
}
