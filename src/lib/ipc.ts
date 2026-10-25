// 面板和后端之间的全部 IPC：命令（invoke）和事件（listen）。命令名、事件名只在这里出现。
// 这些函数直接返回 Tauri 的 Promise，不吞错误、不碰界面状态；出错怎么提示由 state.svelte.ts 决定。
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type { UnlistenFn };

export type HistoryItem = { id: number; kind: string; preview: string; file_names: string[]; source: string; copied_ms: number; copy_count: number; partial: boolean };
export type HistorySnapshot = { items: HistoryItem[]; total: number; recording: boolean; last_recorded_ms: number | null; store_status: string | null };
export type SearchReply = { request_id: number; items: HistoryItem[] };
export type SessionEvent = { session: number };
export type Experiments = { searchBoxFocus?: boolean };
export type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
export type PanelShown = Timing & { history: HistorySnapshot; experiments?: Experiments };
export type PanelKey = SessionEvent & { key: string };
export type PanelMode = SessionEvent & { mode: "search" };
export type PanelError = SessionEvent & { message: string };
export type PasteMode = "paste" | "copy" | "text";
export type PasteResult = SessionEvent & {
  paste_result: "copied" | "sent" | "confirmed" | "copied_only";
  reason: string | null;
  actual_key: string | null;
  layer2: { status: string; timed_out: boolean } | null;
  layer3: { conclusion: string };
  as_text: { conversion: "paths" | "image_path" | "plain_text" | null; error?: string } | null;
  selected_to_result_ms: number;
  // 后端判定面板要停留（目标程序是管理员、只复制了）时带上停留毫秒数；其他结论没有这个字段。
  hint_dwell_ms?: number;
};

// ---------- 命令 ----------
export const getHistory = () => invoke<HistorySnapshot>("get_history");
export const searchHistory = (query: string, requestId: number) => invoke<SearchReply>("search_history", { query, requestId });
export const frontendReady = () => invoke("frontend_ready");
export const startSearch = (session: number) => invoke("start_search", { session });
export const searchReady = (session: number) => invoke("search_ready", { session });
export const hidePanel = (session: number) => invoke("hide_panel", { session });
export const pasteItem = (session: number, id: number, mode: PasteMode) => invoke("paste_item", { session, id, mode });
export const reportPainted = (session: number) => invoke("report_painted", { session });
// 只报输入类型和时序，不报内容。
export const reportSearchInput = (session: number, inputType: string, composing: boolean, afterReadyMs: number) =>
  invoke("report_search_input", { session, inputType, composing, afterReadyMs });
export const appVersion = () => getVersion();

// ---------- 事件 ----------
type Handler<T> = (payload: T) => void;
const on = <T>(name: string) => (handler: Handler<T>): Promise<UnlistenFn> => listen<T>(name, ({ payload }) => handler(payload));

export const onPanelShown = on<PanelShown>("panel:shown");
export const onPanelKey = on<PanelKey>("panel:key");
export const onHistoryChanged = on<SessionEvent>("panel:history-changed");
export const onPanelMode = on<PanelMode>("panel:mode");
export const onPanelHidden = on<SessionEvent>("panel:hidden");
export const onPanelTiming = on<Timing>("panel:timing");
export const onPanelError = on<PanelError>("panel:error");
export const onPasteResult = on<PasteResult>("panel:paste-result");
