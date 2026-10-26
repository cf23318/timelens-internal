import { friendlyError, useDesktopText } from "../lib/desktop";
export function DesktopError({ error, onRetry, onDismiss, employee = false }: { error: string; onRetry?: () => void; onDismiss?: () => void; employee?:boolean }) {
  const { english, text } = useDesktopText();
  return <div className="desktop-alert desktop-alert-error" role="alert">
    <div className="flex items-start justify-between gap-3"><div><strong>{text("操作未完成", "Action not completed")}</strong><p>{friendlyError(error, english, employee)}</p></div>{onDismiss && <button className="desktop-icon-button" aria-label={text("关闭提示", "Dismiss notice")} onClick={onDismiss}>×</button>}</div>
    <details className="mt-2"><summary>{text("查看详情", "Details")}</summary><p className="break-words whitespace-pre-wrap mt-2 text-xs">{error}</p></details>
    {onRetry && <button className="desktop-button mt-3" onClick={onRetry}>{text("重新读取状态", "Reload status")}</button>}
  </div>;
}
