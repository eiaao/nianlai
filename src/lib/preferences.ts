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
  // 是否跟随系统的“减少动画效果”。默认不跟随：动效按上面的强度播放（11-09 定，默认最高动画效果）。
  followSystemMotion: boolean;
};

export const DEFAULT_PREFERENCES: Readonly<Preferences> = {
  layout: "a",
  theme: "system",
  motion: "spring",
  density: "compact",
  followSystemMotion: false,
};

export function normalizePreferences(settings: AppSettings | null | undefined): Preferences {
  const layout = settings?.panel?.layout;
  const theme = settings?.theme;
  const motion = settings?.feedback?.motion;
  const density = settings?.panel?.density;
  return {
    layout: layout === "a" || layout === "b" || layout === "c" ? layout : "a",
    theme: theme === "light" || theme === "dark" || theme === "system" ? theme : "system",
    motion: motion === "none" || motion === "spring" || motion === "soft" ? motion : "spring",
    density: density === "comfortable" || density === "compact" ? density : "compact",
    followSystemMotion: settings?.feedback?.follow_system === true,
  };
}

// 系统“减少动画效果”只在用户选了“跟随系统”时生效；生效时根元素带 data-reduce-motion="true"，
// 组件的样式和脚本都看这个标记（reducedMotion()），不再直接看 prefers-reduced-motion。
let followSystem = false;
let systemMedia: MediaQueryList | null = null;
function syncReduceMotion(root: HTMLElement) {
  systemMedia ??= typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
  root.dataset.reduceMotion = followSystem && systemMedia?.matches ? "true" : "false";
}
export function reducedMotion() {
  return typeof document !== "undefined" && document.documentElement.dataset.reduceMotion === "true";
}

export function applyPreferences(preferences: Preferences, root: HTMLElement = document.documentElement) {
  root.dataset.layout = preferences.layout;
  root.dataset.theme = preferences.theme;
  root.dataset.motion = preferences.motion;
  if (!systemMedia && typeof matchMedia === "function") {
    systemMedia = matchMedia("(prefers-reduced-motion: reduce)");
    systemMedia.addEventListener("change", () => syncReduceMotion(root));
  }
  followSystem = preferences.followSystemMotion;
  syncReduceMotion(root);
  // v0.1 只开放 compact；保留数据值，未来无需迁移设置。
  root.dataset.density = "compact";
}
