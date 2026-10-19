import { useEffect, useRef, useState } from "react";
import { useInternalStore } from "../stores/internalStore";
import { useDesktopText, fileSize, missingLabel } from "../lib/desktop";
import * as internal from "../services/internal";
import { Link } from "react-router-dom";

export function UploadConfirmDialog({ onClose }: { onClose: () => void }) {
  const { text, english } = useDesktopText();
  const { data, busy, act } = useInternalStore();
  const dialog = useRef<HTMLDialogElement>(null);
  const initial = useRef(data);
  const [acknowledged, setAcknowledged] = useState(false);
  const [localError, setLocalError] = useState("");
  useEffect(() => { dialog.current?.showModal(); return () => { dialog.current?.close(); }; }, []);
  if (!data || !initial.current) return null;
  const original = initial.current;
  const cleanupOnly = Boolean(original.progress.receipt);
  const confirmation: internal.UploadConfirmation = {
    taskId: original.progress.taskId, uploadUrl: original.config.uploadUrl,
    encryptionRecipient: original.config.encryptionRecipient, domesticServerConfirmed: original.config.domesticServerConfirmed,
    cleanupOnly,
  };
  const stale = data.progress.taskId !== confirmation.taskId || data.progress.status !== original.progress.status || Boolean(data.progress.receipt) !== cleanupOnly || JSON.stringify(data.config) !== JSON.stringify(original.config);
  const blocked = stale || Boolean((data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError) || (!cleanupOnly && data.uploadMissing.length > 0);
  const confirm = async () => {
    if (blocked || !acknowledged || busy) return;
    setLocalError("");
    const ok = await act(() => original.progress.status === "completed" ? internal.retryInternalUpload(confirmation) : internal.finishInternalTask(confirmation), text("已启动回收，请等待任务状态更新", "Recovery started. Wait for the task status to update."));
    if (ok) onClose(); else setLocalError(text("未能启动，请关闭此窗口查看错误详情。", "Could not start. Close this dialog to review the error."));
  };
  return <dialog ref={dialog} className="desktop-dialog" aria-labelledby="upload-confirm-title" onCancel={e => { if (busy) e.preventDefault(); else onClose(); }}>
    <div className="desktop-dialog-header"><div><div className="desktop-eyebrow">{text("操作确认", "Confirmation")}</div><h2 id="upload-confirm-title">{cleanupOnly ? text("重试本地数据清理", "Retry local cleanup") : text("结束采集并上传", "End collection and upload")}</h2></div><button className="desktop-icon-button" aria-label={text("关闭", "Close")} onClick={onClose} disabled={busy}>×</button></div>
    <div className="desktop-dialog-body">
      <p>{cleanupOnly ? text("服务器已确认接收此任务。本次仅重试本地清理，不会重新上传。", "A server receipt already exists. This action retries local cleanup without uploading again.") : text("确认后将结束本次采集，使用已保存的配置加密打包并上传。结束后的任务不能继续采集。", "This ends the task, encrypts its data and uploads using saved settings. An ended task cannot resume collection.")}</p>
      <dl className="desktop-review-list"><div><dt>{text("任务编号", "Task ID")}</dt><dd className="font-mono text-xs break-all">{confirmation.taskId}</dd></div><div><dt>{text("数据范围", "Contents")}</dt><dd>{text("本次采集数据库、已保存截图、索引与清单。数据库含窗口、网址、事件及统计，不只是图片。", "Collection database, saved screenshots, indexes and manifest. The database includes window, URL, event and metric records.")}</dd></div><div><dt>{text("已入库截图", "Stored screenshots")}</dt><dd>{original.snapshots.count} · {fileSize(original.snapshots.bytes)} {text("（仅截图体积，非最终包大小）", "(images only, not final package size)")}</dd></div>{cleanupOnly ? <div><dt>{text("服务器回执", "Server receipt")}</dt><dd>{text("已确认接收，本次仅重试本地清理", "Receipt confirmed; this action only retries local cleanup")}</dd></div> : <><div><dt>{text("已保存目的地", "Saved destination")}</dt><dd className="break-all">{confirmation.uploadUrl || text("尚未配置", "Not configured")}</dd></div><div><dt>{text("加密公钥", "Public key")}</dt><dd className="font-mono text-xs break-all">{confirmation.encryptionRecipient || text("尚未配置", "Not configured")}</dd></div></>}</dl>
      <div className="desktop-alert desktop-alert-info">{cleanupOnly ? text("本地清理会删除已采集的数据库、截图和数据包，请确认已完成必要核对。", "Local cleanup removes collected database records, screenshots and packages. Complete any necessary review first.") : text("上传失败会保留本地数据。只有服务器回执与任务、文件大小和校验值匹配后，才清理本地采集数据。", "Failed uploads retain local data. Cleanup only follows a server receipt matching the task, file size and checksum.")}</div>
      {blocked && <div className="desktop-alert desktop-alert-warning" role="status">{stale ? text("任务或配置已变化，请关闭后重新查看。", "The task or settings changed. Close and review again.") : (data.progress.needsFlush && data.progress.status !== "recording") || data.progress.integrityError ? text("数据保存或完整性检查未完成，不能开始回收。", "Pending writes or integrity checks prevent recovery.") : <>{text("上传准备未完成：", "Incomplete setup: ")}{data.uploadMissing.map(c => missingLabel(c,english)).join("、")}。 <Link to="/settings?tab=admin" onClick={onClose} className="desktop-text-link">{text("检查管理员配置", "Review administrator settings")}</Link></>}</div>}
      <label className="desktop-checkbox-row"><input type="checkbox" checked={acknowledged} disabled={busy || blocked} onChange={e => setAcknowledged(e.target.checked)}/><span>{text("我已核对上述信息，并了解本地数据的清理规则。", "I reviewed the details and understand the local cleanup rules.")}</span></label>
      {localError && <p className="text-sm text-red-600" role="alert">{localError}</p>}
    </div>
    <div className="desktop-dialog-footer"><button className="desktop-button" onClick={onClose} disabled={busy} autoFocus>{text("取消", "Cancel")}</button><button className="desktop-button desktop-button-danger" disabled={busy || blocked || !acknowledged} onClick={() => void confirm()}>{busy ? text("处理中…", "Working…") : cleanupOnly ? text("确认清理本地数据", "Confirm local cleanup") : text("确认结束并上传", "Confirm end and upload")}</button></div>
  </dialog>;
}
