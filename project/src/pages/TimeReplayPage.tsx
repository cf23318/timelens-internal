import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../services/tauri";
import { snapshotTimelensUrl, type SnapshotReplayPage } from "../types";
import { useAppStore } from "../stores/appStore";

const PAGE_SIZE = 100;

function fmtTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export default function TimeReplayPage() {
  const date = useAppStore((s) => s.date);
  return <ReplayForDate key={date} date={date} />;
}

function ReplayForDate({ date }: { date: string }) {
  const { t } = useTranslation();
  const [page, setPage] = useState<SnapshotReplayPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);
  const [idx, setIdx] = useState(0);
  const [revision, setRevision] = useState(0);
  const [failedImage, setFailedImage] = useState<string | null>(null);
  const offset = Math.floor(idx / PAGE_SIZE) * PAGE_SIZE;
  const buttonClass = "rounded border border-[var(--tl-line)] px-3 py-1.5 text-xs hover:bg-[var(--tl-surface-deep)] disabled:opacity-40";

  useEffect(() => {
    let mounted = true;
    setLoading(true);
    setErr(null);
    api.getSnapshotReplay(date, offset, PAGE_SIZE)
      .then((result) => {
        if (!mounted) return;
        setPage(result);
        setIdx((current) => Math.min(current, Math.max(result.total - 1, 0)));
      })
      .catch((e) => { if (mounted) setErr(String(e)); })
      .finally(() => { if (mounted) setLoading(false); });
    return () => { mounted = false; };
  }, [date, offset, revision]);

  function reload() {
    setFailedImage(null);
    setRevision((value) => value + 1);
  }

  if (err) {
    return <div className="space-y-3 p-5" role="alert">
      <p className="text-sm text-[var(--tl-danger)]">{err}</p>
      <button type="button" className={buttonClass} onClick={reload}>{t("replay.reload")}</button>
    </div>;
  }
  if (loading || !page || page.offset !== offset) {
    return <p className="p-5 text-sm text-[var(--tl-muted)]" role="status">{t("replay.loading")}</p>;
  }
  const current = page.items[idx - page.offset];
  if (!current) {
    return <div className="space-y-3 p-5">
      <p className="text-sm text-[var(--tl-muted)]">{t("replay.empty")}</p>
      <button type="button" className={buttonClass} onClick={reload}>{t("replay.reload")}</button>
    </div>;
  }
  const progress = page.total <= 1 ? 100 : Math.round((idx / (page.total - 1)) * 100);

  return (
    <div className="h-full min-h-0 overflow-auto p-5">
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-4">
        <div className="rounded-xl border border-[var(--tl-line)] bg-[var(--tl-surface)] p-4">
          <div className="mb-2 flex items-center justify-between gap-3">
            <h1 className="font-medium text-[var(--tl-ink)]">{t("replay.title")}</h1>
            <p className="text-xs text-[var(--tl-muted)]">{idx + 1} / {page.total}</p>
          </div>
          <input
            type="range"
            aria-label={t("replay.position")}
            min={0}
            max={Math.max(page.total - 1, 0)}
            value={idx}
            onChange={(e) => setIdx(Number(e.target.value))}
            className="w-full"
          />
          <div className="mt-2 flex flex-wrap items-center justify-between gap-3">
            <p className="text-xs text-[var(--tl-muted)]">{t("replay.progress", { progress })}</p>
            <div className="flex gap-2">
              <button type="button" className={buttonClass} disabled={idx === 0} onClick={() => setIdx((value) => Math.max(0, value - 1))}>{t("replay.previous")}</button>
              <button type="button" className={buttonClass} disabled={idx >= page.total - 1} onClick={() => setIdx((value) => Math.min(page.total - 1, value + 1))}>{t("replay.next")}</button>
              <button type="button" className={buttonClass} onClick={reload}>{t("replay.reload")}</button>
            </div>
          </div>
          {!page.ocrEnabled && <p className="mt-3 text-xs text-[var(--tl-muted)]">{t("replay.ocrDisabled")}</p>}
        </div>

        <article className="rounded-2xl border border-[var(--tl-line)] bg-[var(--tl-surface)] shadow-sm">
          <div className="border-b border-[var(--tl-line)] px-5 py-4">
            <h2 className="text-lg font-semibold text-[var(--tl-ink)]">{current.windowTitle || t("common.noTitle")}</h2>
            <div className="mt-2 flex flex-wrap items-center gap-3 text-xs text-[var(--tl-muted)]">
              <span>{current.appName || t("replay.unknownApp")}</span>
              <span>{fmtTime(current.capturedAtMs)}</span>
            </div>
          </div>
          <div className={`grid gap-4 p-4 ${page.ocrEnabled ? "md:grid-cols-[1.35fr_1fr]" : ""}`}>
            <div className="overflow-hidden rounded-xl border border-[var(--tl-line)] bg-black/5">
              {failedImage === current.snapshotId ? <p className="p-6 text-sm text-[var(--tl-danger)]" role="alert">{t("replay.imageUnavailable")}</p> : <img
                key={current.snapshotId}
                src={snapshotTimelensUrl(current.snapshotId)}
                alt={t("replay.frame", { number: idx + 1, time: fmtTime(current.capturedAtMs) })}
                onError={() => setFailedImage(current.snapshotId)}
                className="max-h-[70vh] w-full object-contain"
              />}
            </div>
            {page.ocrEnabled && <div className="rounded-xl border border-[var(--tl-line)] bg-[var(--tl-surface-deep)] p-3">
              <p className="mb-2 text-xs font-medium text-[var(--tl-muted)]">{t("replay.ocrPreview")}</p>
              <p className="whitespace-pre-wrap break-words text-sm leading-6 text-[var(--tl-ink)]">{current.ocrPreview || t("replay.noOcrText")}</p>
            </div>}
          </div>
        </article>

        <p className="text-xs text-[var(--tl-muted)]">{t("replay.loadedRange", { start: page.offset + 1, end: page.offset + page.items.length, total: page.total })}</p>
        <div className="flex gap-2 overflow-auto pb-1" aria-label={t("replay.thumbnails")}>
          {page.items.map((item, i) => (
            <button
              key={item.snapshotId}
              type="button"
              aria-label={t("replay.frame", { number: page.offset + i + 1, time: fmtTime(item.capturedAtMs) })}
              aria-pressed={page.offset + i === idx}
              className={`shrink-0 rounded-lg border p-1 ${page.offset + i === idx ? "border-[var(--tl-cyan)] bg-[var(--tl-accent-12)]" : "border-[var(--tl-line)] bg-[var(--tl-surface)]"}`}
              onClick={() => setIdx(page.offset + i)}
            >
              <img src={snapshotTimelensUrl(item.snapshotId)} alt="" loading="lazy" decoding="async" className="h-14 w-24 rounded object-cover object-top" />
              <p className="mt-1 text-[10px] text-[var(--tl-muted)]">{fmtTime(item.capturedAtMs)}</p>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
