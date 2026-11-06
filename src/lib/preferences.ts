// 布局、主题、动效与密度是四个彼此独立的持久化维度。
// Rust 的 settings::Settings 会原样保留未知字段，因此这里不需要扩展后端 schema。
import type { AppSettings } from "./ipc";

export type PanelLayout = "a" | "b" | "c";
export type ThemePreference = "system" | "light" | "dark";
export type MotionPreference = "none" | "soft" | "spring";
export type DensityPreference = "compact" | "comfortable";

export type Preferences = {
  layout: PanelLayout;
  theme: ThemePreference;
  motion: MotionPreference;
  density: DensityPreference;
};

export const DEFAULT_PREFERENCES: Readonly<Preferences> = {
  layout: "a",
  theme: "system",
  motion: "soft",
  density: "compact",
};

export function normalizePreferences(settings: AppSettings | null | undefined): Preferences {
  const layout = settings?.panel?.layout;
  const theme = settings?.theme;
  const motion = settings?.feedback?.motion;
  const density = settings?.panel?.density;
  return {
    layout: layout === "a" || layout === "b" || layout === "c" ? layout : "a",
    theme: theme === "light" || theme === "dark" || theme === "system" ? theme : "system",
    motion: motion === "none" || motion === "spring" || motion === "soft" ? motion : "soft",
    density: density === "comfortable" || density === "compact" ? density : "compact",
  };
}

export function applyPreferences(preferences: Preferences, root: HTMLElement = document.documentElement) {
  root.dataset.layout = preferences.layout;
  root.dataset.theme = preferences.theme;
  root.dataset.motion = preferences.motion;
  // v0.1 只开放 compact；保留数据值，未来无需迁移设置。
  root.dataset.density = "compact";
}
