// 面板和后端之间的全部 IPC：命令（invoke）和事件（listen）。命令名、事件名只在这里出现。
// 这些函数直接返回 Tauri 的 Promise，不吞错误、不碰界面状态；出错怎么提示由 state.svelte.ts 决定。
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type { UnlistenFn };

export type HistoryItem = {
  id: number; kind: string; preview: string; file_names: string[]; source: string; copied_ms: number;
  copy_count: number; partial: boolean; favorite: boolean; favorite_position: number | null;
  source_name: string | null; source_icon: string | null;
};
export type HistorySnapshot = { items: HistoryItem[]; total: number; recording: boolean; last_recorded_ms: number | null; store_status: string | null };
export type AppSettings = {
  hotkey?: string;
  theme?: "system" | "light" | "dark";
  panel?: { layout?: "a" | "b" | "c"; density?: "compact" | "comfortable" };
  feedback?: { motion?: "none" | "soft" | "spring" };
  capture?: Partial<CaptureSettings>;
  snapshots?: { max_mb?: number };
  [key: string]: unknown;
};
export type CaptureSettings = {
  excluded_apps: string[]; sensitive_off: string[];
  text: boolean; rich_text: boolean; images: boolean; files: boolean;
  max_text_kb: number; transient: boolean; transient_ms: number;
};
export type SensitiveRule = { id: string; label: string; example: string };
export type CaptureState = { paused: boolean; ignore_next: boolean; rules: SensitiveRule[]; default_excluded_apps: string[] };
export type SnapshotReason = "daily" | "upgrade" | "manual" | "before_restore";
export type SnapshotInfo = { name: string; bytes: number; created_ms: number; reason: SnapshotReason | null };
export type SnapshotState = {
  items: SnapshotInfo[]; keep: number;
  restored: { name: string; error: string | null } | null;
  relaunch: boolean;
};
export type SettingsUpdate = AppSettings;
export type HotkeyValidation = {
  hotkey: string; key: string; control: boolean; alt: boolean; shift: boolean; win: boolean;
  warning: string | null;
};
export type HotkeyStatus = {
  requested: string; active: string | null; error: string | null; detail: string | null;
  warning: string | null; overridden: boolean; disabled: boolean;
};
export type SearchReply = { request_id: number; items: HistoryItem[] };
export type SessionEvent = { session: number };
export type Experiments = { searchBoxFocus?: boolean };
export type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
export type PanelInteraction = "quick" | "activation_pending" | "activation_failed" | "activated" | "passive" | "close_only";
export type PanelShown = Timing & { history: HistorySnapshot; experiments?: Experiments; interaction: PanelInteraction; foregroundRevision: number; terminal_feedback?: boolean };
export type PanelInteractionEvent = SessionEvent & { interaction: PanelInteraction };
export type ForegroundRevision = SessionEvent & { revision: number };
export type PanelKey = SessionEvent & { key: string };
export type PanelMode = SessionEvent & { mode: "search" };
export type PanelError = SessionEvent & { message: string; close_only?: boolean };
export type PasteMode = "paste" | "copy" | "text";
export type SearchSelection = Readonly<{ revision: number; nonce: number }>;
export type PasteResult = SessionEvent & {
  paste_result: "copied" | "sent" | "confirmed" | "copied_only" | "native_submission_unknown";
  reason: string | null;
  actual_key: string | null;
  layer2: { status: string; timed_out: boolean } | null;
  layer3: { conclusion: string };
  as_text: { conversion: "paths" | "image_path" | "plain_text" | null; error?: string } | null;
  selected_to_result_ms: number;
  // 只复制后的被动提示；权限和原控件是否恢复由后端确认，前端不根据 reason 猜测。
  hint_dwell_ms?: number;
  hint_message?: string;
};

// ---------- 命令 ----------
export const getHistory = () => invoke<HistorySnapshot>("get_history");
export const searchHistory = (query: string, requestId: number) => invoke<SearchReply>("search_history", { query, requestId });
export const getFavorites = () => invoke<HistoryItem[]>("get_favorites");
export const setFavorite = (id: number, favorite: boolean) => invoke<HistoryItem | null>("set_favorite", { id, favorite });
// Delete 先挂起、撤销窗口（后端 UNDO_WINDOW，4 秒）后才真删；收藏的返回 false。
export const deleteItem = (id: number) => invoke<boolean>("delete_item", { id });
export const undoDelete = (id: number) => invoke<boolean>("undo_delete", { id });
export const getSettings = () => invoke<AppSettings>("get_settings");
export const setSettings = (settings: SettingsUpdate) => invoke<AppSettings>("set_settings", { settings });
export const validateHotkey = (value: string) => invoke<HotkeyValidation>("validate_hotkey", { value });
export const getHotkeyStatus = () => invoke<HotkeyStatus>("get_hotkey_status");
// 设置页录制新组合键期间暂时注销呼出键（后端 30 秒后自动恢复兜底）。
export const pauseHotkey = (paused: boolean) => invoke<void>("pause_hotkey", { paused });
export const openSettings = () => invoke("open_settings");
// 采集规则（A07–A11）：暂停和“忽略下一次”只在内存里，重启后恢复记录。
export const getSnapshots = () => invoke<SnapshotState>("get_snapshots");
export const createSnapshot = () => invoke<void>("create_snapshot");
export const restoreSnapshot = (name: string) => invoke<void>("restore_snapshot", { name });
export const getCaptureState = () => invoke<CaptureState>("get_capture_state");
export const setCapturePaused = (paused: boolean) => invoke<void>("set_capture_paused", { paused });
export const setIgnoreNext = (ignore: boolean) => invoke<void>("set_ignore_next", { ignore });
export const frontendReady = () => invoke("frontend_ready");
export const startSearch = (session: number) => invoke("start_search", { session });
export const retryActivation = (session: number) => invoke("retry_activation", { session });
export const searchReady = (session: number) => invoke("search_ready", { session });
export const hidePanel = (session: number) => invoke("hide_panel", { session });
export const pasteItem = (session: number, id: number, mode: PasteMode, selection?: SearchSelection) =>
  invoke("paste_item", { session, id, mode, ...(selection ? { selection } : {}) });
export const reportPainted = (session: number) => invoke("report_painted", { session });
// 只报输入类型和时序，不报内容。
export const reportSearchInput = (session: number, inputType: string, composing: boolean, afterReadyMs: number) =>
  invoke("report_search_input", { session, inputType, composing, afterReadyMs });
export const appVersion = () => getVersion();

// ---------- 事件 ----------
type Handler<T> = (payload: T) => void;
const on = <T>(name: string) => (handler: Handler<T>): Promise<UnlistenFn> => listen<T>(name, ({ payload }) => handler(payload));

export const onPanelShown = on<PanelShown>("panel:shown");
export const onPanelInteraction = on<PanelInteractionEvent>("panel:interaction");
export const onForegroundRevision = on<ForegroundRevision>("panel:foreground-revision");
export const onPanelKey = on<PanelKey>("panel:key");
export const onHistoryChanged = on<SessionEvent>("panel:history-changed");
export const onPanelMode = on<PanelMode>("panel:mode");
export const onPanelHidden = on<SessionEvent>("panel:hidden");
export const onPanelTiming = on<Timing>("panel:timing");
export const onPanelError = on<PanelError>("panel:error");
export const onPasteResult = on<PasteResult>("panel:paste-result");
export const onSettingsChanged = on<AppSettings>("settings:changed");
export const onCaptureChanged = on<{ paused: boolean; ignore_next: boolean }>("capture:changed");
