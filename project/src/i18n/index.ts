import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zh from "./locales/zh-CN.json";
import en from "./locales/en.json";
import { setLanguage as setBackendLanguage } from "../services/tauri";

export const SUPPORTED_LANGUAGES = ["zh-CN", "en"] as const;
export type SupportedLanguage = (typeof SUPPORTED_LANGUAGES)[number];

const LANG_STORAGE_KEY = "timelens_ui_lang";

/** 优先使用用户保存的语言偏好；首次启动统一使用中文。 */
function resolveInitialLanguage(): SupportedLanguage {
  try {
    const saved = localStorage.getItem(LANG_STORAGE_KEY);
    if (saved === "zh-CN" || saved === "en") return saved;
  } catch {
    /* ignore */
  }
  return "zh-CN";
}

export async function setLanguage(lang: SupportedLanguage): Promise<void> {
  await setBackendLanguage(lang);
  try {
    localStorage.setItem(LANG_STORAGE_KEY, lang);
  } catch {
    /* ignore */
  }
  await i18n.changeLanguage(lang);
}

i18n.use(initReactI18next).init({
  resources: {
    "zh-CN": { translation: zh },
    en: { translation: en },
  },
  lng: resolveInitialLanguage(),
  fallbackLng: "zh-CN",
  interpolation: { escapeValue: false },
});

export default i18n;
