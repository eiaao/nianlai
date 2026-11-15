import type { HistoryItem, PanelActionId } from "./ipc";

/** 面板动作牌（搜索时置顶的“打开设置”等）。与新手牌一样只在前端合成，负数保留 ID 绝不送入内容 IPC。 */
export const PANEL_ACTION_RESERVED_IDS: Readonly<Record<PanelActionId, number>> = { open_settings: -100 };

export const PANEL_ACTION_COPY: Readonly<Record<PanelActionId, { title: string; body: string }>> = {
  open_settings: { title: "打开设置", body: "按 ↵ 或点一下，打开拈来的设置窗口。" },
};

// 完整命中任一关键词，或输入的是较长关键词的开头（拉丁字母至少 3 个，避免 "se" 这类常见前缀误触发）。
const KEYWORDS: Readonly<Record<PanelActionId, readonly string[]>> = {
  open_settings: ["设置", "打开设置", "设定", "偏好设置", "选项", "settings", "setting", "preferences", "preference", "options", "config", "shezhi"],
};

export function matchesAction(id: PanelActionId, query: string): boolean {
  const value = query.trim().toLowerCase();
  if (!value) return false;
  return KEYWORDS[id].some((keyword) => keyword === value
    || (/^[a-z]{3,}$/.test(value) && /^[a-z]+$/.test(keyword) && keyword.startsWith(value)));
}

export function panelActionItem(id: PanelActionId): HistoryItem {
  const copy = PANEL_ACTION_COPY[id];
  return {
    id: PANEL_ACTION_RESERVED_IDS[id], action: id, kind: "文本",
    preview: copy.body, title: copy.title,
    file_names: [], source: "", source_name: null, source_icon: null,
    copied_ms: 0, copy_count: 0, partial: false, favorite: false, favorite_position: null,
  };
}

/** 搜索词命中的动作牌，按固定顺序置顶。 */
export function matchedActionItems(query: string): HistoryItem[] {
  return (Object.keys(PANEL_ACTION_RESERVED_IDS) as PanelActionId[])
    .filter((id) => matchesAction(id, query)).map(panelActionItem);
}
