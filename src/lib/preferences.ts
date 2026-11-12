// 主题与动效是彼此独立的持久化维度。v10 起面板只有“手牌”一种布局；
// 旧设置里的 panel.layout / panel.density 不再读取或提供切换入口。
import type { AppSettings } from "./ipc";

export type ThemePreference = "system" | "light" | "dark";
export type MotionPreference = "none" | "soft" | "spring";

export type Preferences = {
  theme: ThemePreference;
  motion: MotionPreference;
  // 是否跟随系统的“减少动画效果”。默认不跟随：动效按上面的强度播放（11-09 定，默认最高动画效果）。
  followSystemMotion: boolean;
};

export const DEFAULT_PREFERENCES: Readonly<Preferences> = {
  theme: "system",
  motion: "spring",
  followSystemMotion: false,
};

export function normalizePreferences(settings: AppSettings | null | undefined): Preferences {
  const theme = settings?.theme;
  const motion = settings?.feedback?.motion;
  return {
    theme: theme === "light" || theme === "dark" || theme === "system" ? theme : "system",
    motion: motion === "none" || motion === "spring" || motion === "soft" ? motion : "spring",
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
  root.dataset.theme = preferences.theme;
  root.dataset.motion = preferences.motion;
  if (!systemMedia && typeof matchMedia === "function") {
    systemMedia = matchMedia("(prefers-reduced-motion: reduce)");
    systemMedia.addEventListener("change", () => syncReduceMotion(root));
  }
  followSystem = preferences.followSystemMotion;
  syncReduceMotion(root);
}
