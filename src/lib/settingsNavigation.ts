export const SETTINGS_PAGE_IDS = ["general", "fav", "privacy", "data", "habits", "about"] as const;
export type SettingsPageId = typeof SETTINGS_PAGE_IDS[number];
export type StorageView = "main" | "trash" | "snapshots" | "images" | "export" | "import";
export type SettingsNavigation = { page: SettingsPageId; storageView: StorageView; recordHotkey?: boolean; hotkeyRequest?: string };

const ALIASES = new Map<string, SettingsPageId>([
  ["record", "privacy"],
  ["storage", "data"],
  ["organize", "fav"],
  ["interaction", "habits"],
]);
const STORAGE_VIEWS = new Set<StorageView>(["trash", "snapshots", "images", "export", "import"]);

/** 设置按钮、初始 hash 和 settings:navigate 事件共用的唯一解析入口。 */
export function navigate(target: string): SettingsNavigation {
  let value = target.replace(/^#/, "");
  try { value = decodeURIComponent(value); } catch { /* 无法解码的旧入口按未知页处理。 */ }
  if (STORAGE_VIEWS.has(value as StorageView)) return { page: "data", storageView: value as StorageView };
  if (value === "general/hotkey" || value.startsWith("general/hotkey?")) {
    const request = new URLSearchParams(value.split("?").slice(1).join("?")).get("request");
    return { page: "general", storageView: "main", recordHotkey: true, hotkeyRequest: request || undefined };
  }
  const page = SETTINGS_PAGE_IDS.includes(value as SettingsPageId)
    ? value as SettingsPageId
    : ALIASES.get(value) ?? "general";
  return { page, storageView: "main" };
}
