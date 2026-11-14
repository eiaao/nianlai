import type { GuideCardId, GuideSettings, HistoryItem } from "./ipc";

export const GUIDE_CARD_IDS: readonly GuideCardId[] = ["t1", "t2", "t3", "t4"];
export const GUIDE_CARD_RESERVED_IDS: Readonly<Record<GuideCardId, number>> = { t1: -1, t2: -2, t3: -3, t4: -4 };
export const GUIDE_CARD_COPY: Readonly<Record<GuideCardId, { title: string; body: string }>> = {
  t1: { title: "按数字，直接贴", body: "每张牌左上角有数字，按一下就贴过去。现在按 1 试试。" },
  t2: { title: "它记什么", body: "文字、图片、文件都记；密码管理器里复制的不记。想停一会儿，点托盘图标选“暂停记录”。" },
  t3: { title: "呼出键", body: "" },
  t4: { title: "开机时启动", body: "" },
};

export function defaultGuideSettings(): GuideSettings {
  return { version: 1, phase: "await_copy", baseline_ms: null, baseline_total: 0, sample_id: null, cards_dealt: false, dismissed: [] };
}

export function normalizePanelGuide(value?: GuideSettings): GuideSettings {
  const fallback = defaultGuideSettings();
  if (!value) return fallback;
  return {
    ...fallback, ...value, version: 1,
    dismissed: GUIDE_CARD_IDS.filter((id) => value.dismissed?.includes(id)),
  };
}

export function guideCardItem(id: GuideCardId): HistoryItem {
  const copy = GUIDE_CARD_COPY[id];
  return {
    id: GUIDE_CARD_RESERVED_IDS[id], guide_id: id, kind: "文本",
    preview: copy.body || copy.title, title: copy.title,
    file_names: [], source: "", source_name: null, source_icon: null,
    copied_ms: 0, copy_count: 0, partial: false, favorite: false, favorite_position: null,
  };
}

export function dealtGuideItems(guide: GuideSettings): HistoryItem[] {
  if (!guide.cards_dealt) return [];
  return GUIDE_CARD_IDS.filter((id) => !guide.dismissed.includes(id)).map(guideCardItem);
}

export function displayHotkey(value: string): string {
  return value.split("+").filter(Boolean).map((part) => part
    .replace(/^control$/i, "Ctrl").replace(/^super$/i, "Win")
    .replace(/^Key([A-Z])$/, "$1").replace(/^Digit([0-9])$/, "$1")
    .replace(/^Backquote$/, "`")).join(" + ");
}
