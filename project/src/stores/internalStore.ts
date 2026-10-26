import { useEffect } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import * as internal from "../services/internal";
import { useAppStore } from "./appStore";

interface InternalState {
  data: internal.InternalOverview | null;
  error: string;
  loadError: string;
  message: string;
  busy: boolean;
  draft: internal.InternalConfig | null;
  participantDraft:string|null;
  refresh: () => Promise<void>;
  act: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
  edit: (patch: Partial<internal.InternalConfig>) => void;
  discard: () => void;
}
let inFlight: Promise<void> | null = null;
let progressRevision = 0;
let captureRevision = 0;
export const useInternalStore = create<InternalState>((set, get) => ({
  data: null, error: "", loadError: "", message: "", busy: false, draft: null, participantDraft:null,
  refresh: () => {
    if (inFlight) return inFlight;
    const revision = progressRevision;
    const captureVersion = captureRevision;
    inFlight = internal.getInternalOverview().then(data => {
      if (revision !== progressRevision && get().data) data.progress = get().data!.progress;
      if (captureVersion !== captureRevision && get().data) data.captureNotice = get().data!.captureNotice;
      set({ data, loadError: "" });
      useAppStore.setState({ isTracking: data.progress.status === "recording" });
    }).catch(error => { set({ loadError: String(error) }); }).finally(() => { inFlight = null; });
    return inFlight;
  },
  act: async (action, message) => {
    if (get().busy) return false;
    set({ busy: true, error: "", message: "" });
    try {
      await action();
      await inFlight;
      await get().refresh();
      set({ message });
      return true;
    } catch (error) { set({ error: String(error) }); return false; }
    finally { set({ busy: false }); }
  },
  edit: patch => {
    const config = get().draft ?? get().data?.config;
    if (config) {
      const draft = { ...config, ...patch };
      set({ draft: JSON.stringify(draft) === JSON.stringify(get().data?.config) ? null : draft, message: "" });
    }
  },
  discard: () => set({ draft: null, message: "" }),
}));

/** One lightweight refresh per ten seconds; task clock events update only progress. */
export function useInternalUpdates() {
  useEffect(() => {
    const store = useInternalStore;
    void store.getState().refresh();
    let disposed = false;
    let timer: number | undefined;
    const unsubs: Array<() => void> = [];
    const register = async () => {
      for (const [event, callback] of [
        ["internal_task_changed", (payload: unknown) => {
          const progress = payload as internal.TaskProgress;
          progressRevision++;
          if (store.getState().data?.progress.status !== progress.status) captureRevision++;
          store.setState(s => s.data ? { data: { ...s.data, progress, captureNotice: progress.status !== s.data.progress.status ? { code: "", atMs: 0 } : s.data.captureNotice } } : {});
          useAppStore.setState({ isTracking: progress.status === "recording" });
        }],
        ["new_snapshot_saved", () => {
          window.clearTimeout(timer);
          timer = window.setTimeout(() => { void store.getState().refresh(); }, 750);
        }],
      ] as const) {
        const unlisten = await listen(event, e => { if (!disposed) callback(e.payload); });
        if (disposed) unlisten(); else unsubs.push(unlisten);
      }
    };
    void register().catch(error => { if (!disposed) store.setState({ loadError: String(error) }); });
    const refresh = () => { if (!document.hidden) void store.getState().refresh(); };
    const interval = window.setInterval(refresh, 10000);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      disposed = true; window.clearTimeout(timer); window.clearInterval(interval);
      unsubs.forEach(fn => fn()); window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);
}
