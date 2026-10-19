import { InternalSettings } from "../components/InternalSettings";

export default function SettingsShellPage() {
  return (
    <div className="h-full overflow-y-auto text-[var(--tl-ink)]">
      <InternalSettings />
    </div>
  );
}
