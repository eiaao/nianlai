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
  // M2-3：文件条目的失效检查结论；文件都在时没有这个字段。
  file_state?: FileMark;
  // M2-6：常用的标题和备注（E04）；没有时不带。
  title?: string;
  note?: string;
};
export type FileMark = { state: "missing" | "partial" | "unknown"; missing: number; unknown: number; total: number; checked_ms: number };
export type PathCheck = { path: string; state: "present" | "missing" | "unknown" };
export type StorageFiles = { count: number; bytes: number };
export type StorageOverview = {
  database_bytes: number;
  kinds: { kind: string; count: number; bytes: number }[];
  trashed: StorageFiles;
  reclaimable_bytes: number;
  snapshots: StorageFiles;
  exports: StorageFiles;
  leftovers: StorageFiles;
  free_bytes: number | null;
  files: { missing: number; partial: number; unknown: number; checked_ms: number | null };
  images: { all: StorageFiles; older_30: StorageFiles; older_90: StorageFiles };
};
export type ImageItem = { id: number; bytes: number; copied_ms: number; favorite: boolean };
export type TrashItem = HistoryItem & { deleted_ms: number };
export type HistorySnapshot = { items: HistoryItem[]; total: number; recording: boolean; last_recorded_ms: number | null; store_status: string | null };
export type AppSettings = {
  hotkey?: string;
  theme?: "system" | "light" | "dark";
  feedback?: { motion?: "none" | "soft" | "spring"; follow_system?: boolean };
  capture?: Partial<CaptureSettings>;
  snapshots?: { max_mb?: number };
  interaction?: Partial<InteractionSettings>;
  history?: { reuse_to_top?: boolean };
  [key: string]: unknown;
};
export type CaptureSettings = {
  excluded_apps: string[]; sensitive_off: string[];
  text: boolean; rich_text: boolean; images: boolean; files: boolean;
  max_text_kb: number; transient: boolean; transient_ms: number;
};
export type Placement = "caret" | "mouse" | "center";
export type ClickAction = "paste" | "copy" | "text" | "none";
export type InteractionSettings = { placement: Placement; click: ClickAction; middle_click: ClickAction };
export type SensitiveRule = { id: string; label: string; example: string };
export type CaptureState = { paused: boolean; ignore_next: boolean; rules: SensitiveRule[]; default_excluded_apps: string[] };
export type SnapshotReason = "daily" | "upgrade" | "manual" | "before_restore";
export type SnapshotInfo = { name: string; bytes: number; created_ms: number; reason: SnapshotReason | null; version: number | null };
export type SnapshotState = {
  items: SnapshotInfo[]; keep: number; current_version: number;
  restored: { name: string; error: string | null; kind?: "snapshot" | "import" } | null;
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
export type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
export type PanelInteraction = "quick" | "activation_pending" | "activation_failed" | "activated" | "passive" | "close_only";
// panel:shown 还带 caret / placement（后端避让结果），前端不用，所以不声明。
export type PanelShown = Timing & { history: HistorySnapshot; interaction: PanelInteraction; foregroundRevision: number; terminal_feedback?: boolean };
export type PanelInteractionEvent = SessionEvent & { interaction: PanelInteraction };
export type ForegroundRevision = SessionEvent & { revision: number };
export type PanelKey = SessionEvent & { key: string };
export type PanelMode = SessionEvent & { mode: "search" };
export type PanelError = SessionEvent & { message: string; close_only?: boolean };
export type PasteMode = "paste" | "copy" | "text";
export type QueueState = SessionEvent & {
  remaining: number;
  total: number;
  automatic: boolean;
  busy: boolean;
  error?: string | null;
  /** 逐张放时跳过（已不在、复制模式不支持等）的张数。 */
  skipped?: number;
  /** 队列结束的原因名（done / cancelled / escape 是正常结束，其余如 idle_timeout 是异常停止），只在结束时出现。 */
  stopped?: string | null;
  /** 异常停止时没放出去的条目 id（按原顺序）。 */
  remaining_ids?: number[];
};
export type HitRegion = { left: number; top: number; right: number; bottom: number };
/** 避让外形（窗口客户区 CSS 像素）：后端只按它挪窗口避开光标行；regions 只管点击。 */
export type Outline = { left: number; top: number; width: number; height: number };
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

// M2-4 备份（B07）。
export type BackupSummary = { items: number; favorites: number; icons: number; bytes: number; default_name: string };
export type BackupExported = { path: string; bytes: number; items: number; favorites: number; icons: number };
export type BackupStep = { id: "manifest" | "hashes" | "database"; ok: boolean; detail: string };
export type BackupInspection = {
  token: string; file_name: string; exported_ms: number; app_version: string; schema_version: number;
  upgraded_from: number | null; steps: BackupStep[]; ok: boolean;
  merge: { new_items: number; new_favorites: number } | null;
  overwrite: { items: number } | null;
};
export type BackupImported = { mode: "merge" | "overwrite"; added_items: number; added_favorites: number; relaunch: boolean; auto_restart: boolean };

// ---------- 命令 ----------
export const getHistory = () => invoke<HistorySnapshot>("get_history");
export const searchHistory = (query: string, requestId: number) => invoke<SearchReply>("search_history", { query, requestId });
export const getFavorites = () => invoke<HistoryItem[]>("get_favorites");
export const setFavorite = (id: number, favorite: boolean) => invoke<HistoryItem | null>("set_favorite", { id, favorite });
// M2-6 整理：ids 是收藏区（不含回收站里的）想要的完整顺序；返回新的收藏列表。
export const reorderFavorites = (ids: number[]) => invoke<HistoryItem[]>("reorder_favorites", { ids });
export const setFavoriteLabel = (id: number, title: string, note: string) => invoke<HistoryItem>("set_favorite_label", { id, title, note });
// Delete 先挂起、撤销窗口（后端 UNDO_WINDOW，4 秒）后才真删；收藏的返回 false。
export const deleteItem = (id: number) => invoke<boolean>("delete_item", { id });
export const undoDelete = () => invoke<HistoryItem | null>("undo_delete");
export const getTrash = () => invoke<TrashItem[]>("get_trash");
export const restoreItem = (id: number) => invoke<HistoryItem | null>("restore_item", { id });
export const purgeItems = (ids: number[] | null) => invoke<number>("purge_items", { ids });
export const getSettings = () => invoke<AppSettings>("get_settings");
export const setSettings = (settings: SettingsUpdate) => invoke<AppSettings>("set_settings", { settings });
export const validateHotkey = (value: string) => invoke<HotkeyValidation>("validate_hotkey", { value });
export const getHotkeyStatus = () => invoke<HotkeyStatus>("get_hotkey_status");
// 设置页录制新组合键期间暂时注销呼出键（后端 30 秒后自动恢复兜底）。
export const pauseHotkey = (paused: boolean) => invoke<void>("pause_hotkey", { paused });
export const openSettings = (page?: string) => invoke("open_settings", { page: page ?? null });
// 存储管理（M2-3）
export const getFilePaths = (id: number) => invoke<PathCheck[] | null>("get_file_paths", { id });
export const getStorage = () => invoke<StorageOverview>("get_storage");
export const checkFiles = () => invoke<void>("check_files");
export const cleanMissing = () => invoke<number>("clean_missing");
export const cleanImages = (olderDays: number | null) => invoke<number>("clean_images", { olderDays });
export const trashItems = (ids: number[]) => invoke<number>("trash_items", { ids });
export const getImages = () => invoke<ImageItem[]>("get_images");
export const getThumbnail = (id: number) => invoke<string | null>("get_thumbnail", { id });
export const cleanExports = () => invoke<StorageFiles>("clean_exports");
export const cleanLeftovers = () => invoke<StorageFiles>("clean_leftovers");
export const compactHistory = () => invoke<number>("compact_history");
// 完整预览（M2-7）
export type TextChunk = { text: string; total: number; shown: number };
export type ImageView = { url: string; width: number; height: number; bytes: number };
export const getFullText = (id: number, limit: number | null) => invoke<TextChunk | null>("get_full_text", { id, limit });
export const getImage = (id: number, max: number | null) => invoke<ImageView | null>("get_image", { id, max });
export const copySelection = (id: number, text: string) => invoke<void>("copy_selection", { id, text });
export const setPreviewKeys = (open: boolean) => invoke<void>("set_preview_keys", { open });
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
export const startQueue = (session: number, ids: number[], automatic: boolean, mode: PasteMode, selection?: SearchSelection) =>
  invoke("start_queue", { session, ids, automatic, mode, ...(selection ? { selection } : {}) });
export const cancelQueue = (session: number) => invoke("cancel_queue", { session });
// 原生命中区域使用 CSS 像素；后端按当前 devicePixelRatio 转成窗口客户区坐标。
// session 用于后端丢弃旧会话迟到的上报。avoid：undefined 表示这次不带（沿用上次），null 表示没有避让外形。
export const setHitRegions = (regions: HitRegion[], scale: number, armed = true, session?: number, avoid?: Outline | null) =>
  invoke<void>("set_hit_regions", { regions, scale, armed, ...(session === undefined ? {} : { session }), ...(avoid === undefined ? {} : { avoid }) });
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
export const onPanelHidden = on<SessionEvent & { queue?: boolean }>("panel:hidden");
export const onQueueChanged = on<QueueState>("queue:changed");
export const onPanelTiming = on<Timing>("panel:timing");
export const onPanelError = on<PanelError>("panel:error");
export const onPasteResult = on<PasteResult>("panel:paste-result");
export const onSettingsChanged = on<AppSettings>("settings:changed");
export const onCaptureChanged = on<{ paused: boolean; ignore_next: boolean }>("capture:changed");

// ---------- M2-4 备份 ----------
export const backupSummary = () => invoke<BackupSummary>("backup_summary");
export const chooseBackupSave = (defaultName: string) => invoke<string | null>("choose_backup_save", { defaultName });
export const chooseBackupOpen = () => invoke<string | null>("choose_backup_open");
export const exportBackup = (path: string) => invoke<BackupExported>("export_backup", { path });
export const revealInExplorer = (path: string) => invoke<void>("reveal_in_explorer", { path });
export const inspectBackup = (path: string) => invoke<BackupInspection>("inspect_backup", { path });
export const importBackup = (token: string, mode: "merge" | "overwrite") => invoke<BackupImported>("import_backup", { token, mode });
export const discardImport = (token: string) => invoke<void>("discard_import", { token });
