import { create } from "zustand";

export type UiTheme =
  | "tech" | "white" | "spotify" | "linear" | "cursor"
  | "catppuccin" | "rosepine" | "sakura" | "cotton-candy" | "peach-milk"
  | "dracula" | "tokyo-night" | "one-dark"
  | "claude" | "gruvbox-light" | "raycast" | "nord" | "gruvbox-dark";

const ALL_THEMES: UiTheme[] = [
  "tech", "white", "spotify", "linear", "cursor",
  "catppuccin", "rosepine", "sakura", "cotton-candy", "peach-milk",
  "dracula", "tokyo-night", "one-dark",
  "claude", "gruvbox-light", "raycast", "nord", "gruvbox-dark",
];

const STORAGE_KEY = "timelens_ui_theme";
export const DEFAULT_THEME: UiTheme = "white";

function readStored(): UiTheme {
  try {
    if (typeof localStorage === "undefined") return DEFAULT_THEME;
    const v = localStorage.getItem(STORAGE_KEY) as UiTheme | null;
    return v && ALL_THEMES.includes(v) ? v : DEFAULT_THEME;
  } catch {
    return DEFAULT_THEME;
  }
}

interface ThemeState {
  theme: UiTheme;
  setTheme: (t: UiTheme) => void;
}

export const useThemeStore = create<ThemeState>((set) => ({
  theme: readStored(),
  setTheme: (t) => {
    try {
      localStorage.setItem(STORAGE_KEY, t);
    } catch {
      /* ignore */
    }
    set({ theme: t });
  },
}));
