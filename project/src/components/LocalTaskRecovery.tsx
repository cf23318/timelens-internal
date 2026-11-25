import { useState } from "react";
import { useDesktopText } from "../lib/desktop";
import { repairLocalTask, type LocalTaskIssue } from "../services/internal";
import { useInternalStore } from "../stores/internalStore";

export function LocalTaskRecovery({ issue, deviceUnavailable }: {
  issue?: LocalTaskIssue | null;
  deviceUnavailable?: boolean;
}) {
  const { text } = useDesktopText();
  const refresh = useInternalStore(state => state.refresh);
  const [repairing, setRepairing] = useState(false);
  const [error, setError] = useState("");
  const reasons = {
    configAccess: text("无法访问录制配置", "Recording configuration is inaccessible"),
    configMissing: text("录制配置文件丢失", "Recording configuration is missing"),
    configFormat: text("录制配置文件格式异常", "Recording configuration format is invalid"),
    configValues: text("录制配置的值不合法", "Recording configuration has invalid values"),
    progressAccess: text("无法访问任务状态文件", "Task state file is inaccessible"),
    progressMissing: text("已有任务的状态文件丢失", "Existing task state file is missing"),
    progressFormat: text("任务状态文件格式异常", "Task state format is invalid"),
    progressValues: text("任务编号或累计时长异常", "Task ID or elapsed time is invalid"),
    archiveInvalid: text("归档状态或删除期限异常", "Archive state or retention deadline is invalid"),
    ownerInvalid: text("无法读取本机任务归属", "Local task ownership cannot be read"),
    identityMismatch: text("任务文件与本机记录不匹配", "Task files do not match this profile"),
    storageUnavailable: text("本机数据目录暂不可用", "Local data directory is unavailable"),
  } satisfies Record<LocalTaskIssue["kind"], string>;
  const accessProblem = issue && ["configAccess", "progressAccess", "storageUnavailable"].includes(issue.kind);
  if (!issue && !deviceUnavailable) return null;

  const repair = async () => {
    setRepairing(true);
    setError("");
    useInternalStore.setState({ message: "" });
    try {
      const result = await repairLocalTask();
      // If a poll along with the repair returned the previous state, read once more.
      await refresh();
      const state = useInternalStore.getState();
      if (state.loadError || state.data?.startupIssue ||
          (!result.deviceError && state.data?.deviceIdentityUnavailable)) {
        await refresh();
      }
      const latest = useInternalStore.getState();
      if (latest.loadError || latest.data?.startupIssue ||
          (!result.deviceError && latest.data?.deviceIdentityUnavailable)) {
        setError(text("修复已执行，暂时无法读取最新状态。请再试一次。", "Repair ran, but the latest status could not be read. Try again."));
        return;
      }
      if (result.deviceError) setError(result.deviceError);
      const message = result.taskRepaired
        ? result.deviceError
          ? text("本机任务已恢复，设备编号仍未修复。", "The local task recovered. The device ID still needs repair.")
          : result.deviceRepaired
            ? text("本机任务和原设备编号均已恢复。", "The local task and original device ID were restored.")
            : text("本机任务已恢复。", "The local task recovered.")
        : result.deviceRepaired
          ? text("原设备编号已恢复，同步将按原设置继续。", "The original device ID was restored. Sync follows your existing settings.")
          : "";
      useInternalStore.setState({ message });
    } catch (problem) {
      setError(String(problem));
    } finally {
      setRepairing(false);
    }
  };

  return <section className="desktop-alert desktop-alert-warning" aria-live="polite">
    <h2>{issue ? reasons[issue.kind] : text("设备编号暂不可用", "Device ID unavailable")}</h2>
    <p>{issue
      ? text("TimeLens 会检查任务文件和设备编号，根据本机记录修复异常项。", "TimeLens checks task files and the device ID and repairs problems using verified local records.")
      : text("录制仍可继续，待传截图留在本机。TimeLens 会尝试恢复原设备编号。", "Collection can continue and queued frames stay on this device. TimeLens will try to recover the original device ID.")}</p>
    {accessProblem && <p>{text("请检查数据盘是否可访问、是否有剩余空间，以及文件是否被占用。", "Check that the data drive is accessible, has free space, and that files are not locked.")}</p>}
    {issue && <p className="desktop-caption break-words mt-2">{text("详情", "Details")} · {issue.detail}</p>}
    {error && <p className="break-words mt-2" role="alert">{error}</p>}
    <button className="desktop-button mt-3" disabled={repairing} onClick={() => void repair()}>
      {repairing ? text("正在修复…", "Repairing…") : text("一键修复", "Repair")}
    </button>
  </section>;
}
