import { useEffect, useState } from "react";
import { useDesktopText } from "../lib/desktop";
import * as api from "../services/tauri";

const SNOOZE_KEY = "timelens_update_snooze";
const SNOOZE_MS = 3 * 60 * 60 * 1000;

interface Snooze {
  version: string;
  untilMs: number;
}

function readSnooze(): Snooze | null {
  try {
    const value = JSON.parse(localStorage.getItem(SNOOZE_KEY) ?? "null") as Snooze | null;
    if (typeof value?.version === "string" && Number.isFinite(value.untilMs) && value.untilMs > Date.now()) {
      return value;
    }
  } catch { /* Unavailable storage only affects reminder persistence. */ }
  return null;
}

export default function UpdateNotice() {
  const { text } = useDesktopText();
  const [version, setVersion] = useState<string | null>(null);
  const [snoozed, setSnoozed] = useState(readSnooze);
  const [openFailedVersion, setOpenFailedVersion] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const check = () => {
      void api.checkForUpdate()
        .then((latest) => {
          if (!active) return;
          const now = Date.now();
          setSnoozed((previous) => previous && previous.untilMs > now ? previous : null);
          setVersion(latest);
        })
        .catch(() => {});
    };
    check();
    const listening = api.listenEvent("update_check_requested", check).catch(() => null);
    return () => {
      active = false;
      void listening.then((stop) => stop?.());
    };
  }, []);

  if (!version || version === snoozed?.version) return null;

  function remindLater(currentVersion: string) {
    const next: Snooze = { version: currentVersion, untilMs: Date.now() + SNOOZE_MS };
    setSnoozed(next);
    try {
      localStorage.setItem(SNOOZE_KEY, JSON.stringify(next));
    } catch { /* The running window can still defer this version. */ }
  }

  return (
    <div className="desktop-global-notice">
      <div className="desktop-alert desktop-alert-info flex flex-wrap items-center justify-between gap-3">
        <div role="status">
          <strong>{text("有新版本可用", "Update available")} · v{version}</strong>
          {openFailedVersion === version && (
            <p>{text("无法打开浏览器。请前往 GitHub 发布页下载。", "Could not open your browser. Open the GitHub releases page to download.")}</p>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button className="desktop-button" onClick={() => remindLater(version)}>
            {text("稍后", "Later")}
          </button>
          <button className="desktop-button desktop-button-primary" onClick={() => {
            setOpenFailedVersion(null);
            void api.openUpdateRelease(version).catch(() => setOpenFailedVersion(version));
          }}>
            {text("前往下载", "View download")}
          </button>
        </div>
      </div>
    </div>
  );
}
