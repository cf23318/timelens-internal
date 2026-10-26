import type { InternalOverview } from "../services/internal";

export interface SyncIssue { title: string; detail: string; tone: "warning" | "info" }

/** Only local, fixed messages reach the UI. Never echo an error, URL or credential. */
export function employeeSyncIssue(data: InternalOverview, english = false, now = Date.now()): SyncIssue | null {
  const text = (zh: string, en: string) => english ? en : zh;
  const issue = (title: string, detail: string, tone: SyncIssue["tone"] = "warning") => ({title, detail, tone});
  if (data.archive && (data.archive.deleted || now >= data.archive.deadlineMs)) return issue(
    text("任务已到期，已停止同步", "Task expired; sync stopped"),
    text("请联系管理员核对留存和清理状态。", "Contact the administrator to review retention and cleanup."));
  if (data.progress.integrityError) return issue(
    text("部分记录需要核对", "Records need review"),
    text("本机记录已保留，请联系管理员核对完整性或上传冲突，无需清空数据。", "Local records are retained. Ask the administrator to review integrity or upload conflicts; do not clear data."));
  const uploadError = data.uploads.lastError || data.progress.lastError;
  const cloudError = data.cloudIssuer?.lastError;
  const authorizationRetry = Boolean(uploadError?.startsWith("云端续期暂不可用"));
  const error = authorizationRetry ? cloudError || uploadError : uploadError || cloudError;
  if (!error) return data.uploads.blocked > 0 ? issue(
    text("部分记录需要核对", "Records need review"),
    text("本机记录已保留，请联系管理员核对上传状态，无需清空数据。", "Local records are retained. Ask the administrator to review upload status; do not clear data.")) : null;
  const cloud = authorizationRetry || (!uploadError && Boolean(cloudError));
  // A grant can be unexpired locally while OSS has already rejected it.
  const result = describeSyncFailure(error, cloud, Boolean(cloud && !authorizationRetry && data.uploads.blocked === 0 && data.ossGrant?.valid), english);
  if (data.uploads.blocked > 0) result.detail += text(" 部分记录需管理员核对后重试，请勿清空数据。", " Some records require administrator review before retry; do not clear data.");
  return result;
}

export function describeSyncFailure(error: string, cloud = true, stillValid = false, english = false): SyncIssue {
  const text = (zh: string, en: string) => english ? en : zh;
  const stage = cloud ? text("配置／授权", "Configuration / authorization") : text("截图／清单上传", "Image / inventory upload");
  const retained = stillValid
    ? text("现有上传授权仍有效，截图可继续上传；软件会自动重试续期。", "The current grant is still valid, so uploads can continue. Renewal retries automatically.")
    : text("待传截图保留在本机，恢复后会继续补传。", "Queued images remain on this device and resume uploading after recovery.");
  const retry = text("软件会自动重试。", "The app retries automatically.");
  const http = error.match(/HTTP\s+(\d{3})\b/);
  let title: string, action: string;
  if (http) {
    const code = Number(http[1]);
    title = `${stage}${text("失败", " failed")}（HTTP ${code}）`;
    if (code === 401 || code === 403 || code === 410) action = text("请联系管理员检查批次、设备授权或调研期限。", "Ask the administrator to check the batch, device authorization or study deadline.");
    else if (code === 409) action = text("登记或上传记录发生冲突，请联系管理员，勿重复注册或清空数据。", "Registration or upload records conflict. Contact the administrator; do not register again or clear data.");
    else if (code === 407) action = text("系统代理要求身份验证，请检查代理登录状态。", "The system proxy requires authentication. Check the proxy login.");
    else if (code === 429) action = text("服务请求过于频繁，请稍后重试。", "The service is rate limited; retry later.") + retry;
    else if (code === 408 || code >= 500) action = text("服务暂时不可用，请检查网络，持续失败请联系管理员。", "The service is temporarily unavailable. Check the network; contact the administrator if it persists.") + retry;
    else action = text("服务拒绝了请求，请联系管理员检查配置。", "The service rejected the request. Ask the administrator to review configuration.");
  } else if (error.startsWith("云端续期暂不可用")) {
    title = text("上传授权需要更新", "Upload authorization needs renewal");
    action = text("当前上传已暂停等待新授权。请检查网络和系统代理，持续失败请联系管理员；软件会继续重试续期。", "Uploads are waiting for a new grant. Check the network and system proxy; contact the administrator if it persists. Renewal retries automatically.");
  } else if (/超时|连接失败|读取失败|无法建立.*连接/.test(error)) {
    title = `${stage}${/超时/.test(error) ? text("连接超时", " timed out") : text("连接失败", " connection failed")}`;
    action = cloud
      ? text("请确认网络正常、系统代理已开启且代理程序正在运行；软件会按系统代理设置重试。", "Check the network and ensure the system proxy and its application are running. The app retries using system proxy settings.")
      : text("请检查网络能否直连存储服务，持续失败请联系管理员；软件会自动重试。", "Check direct connectivity to storage. Contact the administrator if it persists; the app retries automatically.");
  } else {
    title = text("同步配置或数据需要处理", "Sync configuration or data needs review");
    action = text("请联系管理员核对配置及数据完整性，无需清空数据。", "Ask the administrator to review configuration and data integrity; do not clear data.");
  }
  return {title, detail: `${action} ${retained}`, tone: stillValid ? "info" : "warning"};
}
