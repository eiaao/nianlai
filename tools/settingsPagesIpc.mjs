// tools/test-settings-pages.mjs 的内存 Tauri 替身：只替换 IPC，不替换任何生产组件。
import nianlaiLicense from "../LICENSE?raw";
import thirdPartyNotices from "../THIRD_PARTY_NOTICES.md?raw";

const clone = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));

function merge(base, update) {
  if (base && update && typeof base === "object" && typeof update === "object" && !Array.isArray(base) && !Array.isArray(update)) {
    const result = { ...base };
    for (const [key, value] of Object.entries(update)) result[key] = merge(result[key], value);
    return result;
  }
  return clone(update);
}

const DEFAULT_SETTINGS = {
  hotkey: "Ctrl+`",
  theme: "system",
  feedback: { motion: "spring", follow_system: false },
  capture: {
    excluded_apps: ["Example.exe"], sensitive_off: [],
    text: true, rich_text: true, images: true, files: true,
    max_text_kb: 2048, transient: true, transient_ms: 1000,
  },
  snapshots: { max_mb: 1024 },
  interaction: { placement: "caret", click: "paste", middle_click: "text" },
  history: { reuse_to_top: true },
};

const SETTINGS_KEY = `nianlai-settings-pages-${new URLSearchParams(location.search).get("test") ?? "default"}`;
const SETTINGS_READ_KEY = `${SETTINGS_KEY}-read-count`;
const FAVORITES_KEY = `${SETTINGS_KEY}-favorites`;
const DEFAULT_FAVORITES = [
  {
    id: 101, kind: "文本", preview: "测试常用", file_names: [], source: "Example.exe", copied_ms: 0,
    copy_count: 0, partial: false, favorite: true, favorite_position: 1, source_name: "示例程序", source_icon: null,
  },
  {
    id: 102, kind: "文本", preview: "https://example.com/meeting/room", file_names: [], source: "Example.exe", copied_ms: 0,
    copy_count: 0, partial: false, favorite: true, favorite_position: 2, source_name: "示例程序", source_icon: null,
  },
  {
    id: 103, kind: "图片", preview: "图片 1280×720", file_names: [], source: "Example.exe", copied_ms: 0,
    copy_count: 0, partial: false, favorite: true, favorite_position: 3, source_name: "示例程序", source_icon: null,
  },
  {
    id: 104, kind: "文件", preview: "报销单.pdf", file_names: ["报销单.pdf", "明细.xlsx"], source: "Example.exe", copied_ms: 0,
    copy_count: 0, partial: false, favorite: true, favorite_position: 4, source_name: "示例程序", source_icon: null,
  },
];
let settings = (() => {
  try {
    const saved = sessionStorage.getItem(SETTINGS_KEY);
    return saved ? JSON.parse(saved) : clone(DEFAULT_SETTINGS);
  } catch { return clone(DEFAULT_SETTINGS); }
})();
function persistSettings() { sessionStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)); }
let favorites = (() => {
  try {
    const saved = sessionStorage.getItem(FAVORITES_KEY);
    if (saved) return JSON.parse(saved);
  } catch { /* 新场景使用默认常用。 */ }
  const query = new URLSearchParams(location.search);
  if (query.get("favEmpty") === "1") return [];
  if (query.get("favSingle") === "1") return clone(DEFAULT_FAVORITES.slice(0, 1));
  return clone(DEFAULT_FAVORITES);
})();
function persistFavorites() { sessionStorage.setItem(FAVORITES_KEY, JSON.stringify(favorites)); }
function favoriteList() {
  return clone(favorites.filter((item) => item.favorite).sort((a, b) => a.favorite_position - b.favorite_position));
}
let favoriteLabelErrors = 0;
let favoriteOrderErrors = 0;
let favoriteWriteErrors = 0;
let deferFavoriteLabels = false;
let deferFavoriteOrders = false;
let deferFavoriteWrites = false;
const deferredFavoriteLabels = [];
const deferredFavoriteOrders = [];
const deferredFavoriteWrites = [];
let manualSaves = false;
let nextSaveId = 0;
const saves = [];
const listeners = new Map();
const invocations = [];
let autostartEnabled = false;
let autostartWriteError = null;
let autostartReadErrors = 0;
let autostartExternalOnWrite = null;
let hotkeyValidationError = null;
let deferAutostartSet = false;
let deferAutostartGet = false;
const deferredAutostartSets = [];
const deferredAutostartGets = [];

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const truncate = (value, limit) => Array.from(String(value ?? "")).slice(0, limit).join("");
function writeFavoriteLabel(args) {
  if (favoriteLabelErrors > 0) {
    favoriteLabelErrors -= 1;
    throw new Error("模拟标题备注保存失败");
  }
  const item = favorites.find((entry) => entry.id === args.id && entry.favorite);
  if (!item) throw new Error("只有常用的才能设置标题和备注");
  item.title = truncate(args.title.trim().replace(/[\r\n]/g, " "), 200);
  item.note = truncate(args.note.trim(), 2000);
  persistFavorites();
  return clone(item);
}
function writeFavoriteOrder(ids) {
  if (favoriteOrderErrors > 0) {
    favoriteOrderErrors -= 1;
    throw new Error("模拟顺序保存失败");
  }
  const current = favoriteList().map((item) => item.id);
  const requested = [...ids].sort((a, b) => a - b);
  const expected = [...current].sort((a, b) => a - b);
  if (requested.length !== expected.length || requested.some((id, index) => id !== expected[index])) {
    throw new Error("常用牌刚变过，重新打开这页再排");
  }
  ids.forEach((id, index) => {
    const item = favorites.find((entry) => entry.id === id);
    item.favorite_position = index + 1;
  });
  persistFavorites();
  return favoriteList();
}
function writeFavorite(args) {
  if (favoriteWriteErrors > 0) {
    favoriteWriteErrors -= 1;
    throw new Error("模拟常用状态保存失败");
  }
  const item = favorites.find((entry) => entry.id === args.id);
  if (!item) return null;
  if (args.favorite && !item.favorite) {
    item.favorite = true;
    item.favorite_position = Math.max(0, ...favorites.filter((entry) => entry.favorite).map((entry) => entry.favorite_position)) + 1;
  } else if (!args.favorite) {
    item.favorite = false;
    item.favorite_position = null;
    delete item.title;
    delete item.note;
  }
  persistFavorites();
  return clone(item);
}
function takeDeferred(queue, id) {
  const index = queue.findIndex((entry) => entry.id === id);
  if (index < 0) throw new Error(`没有等待中的常用写入 ${id}`);
  return queue.splice(index, 1)[0];
}

const EMPTY_FILES = { count: 0, bytes: 0 };
const storage = {
  database_bytes: 0,
  kinds: [],
  trashed: { ...EMPTY_FILES },
  reclaimable_bytes: 0,
  snapshots: { ...EMPTY_FILES },
  exports: { ...EMPTY_FILES },
  leftovers: { ...EMPTY_FILES },
  free_bytes: 1024 * 1024 * 1024,
  files: { missing: 0, partial: 0, unknown: 0, checked_ms: null },
  images: { all: { ...EMPTY_FILES }, older_30: { ...EMPTY_FILES }, older_90: { ...EMPTY_FILES } },
};
const initialQuery = new URLSearchParams(location.search);
if (initialQuery.get("storage") === "1") {
  Object.assign(storage, {
    database_bytes: 8192,
    kinds: [{ kind: "文本", count: 2, bytes: 4096 }, { kind: "图片", count: 12, bytes: 4096 }],
    files: { missing: 3, partial: 1, unknown: 0, checked_ms: Date.now() },
    images: {
      all: { count: 12, bytes: 12000 },
      older_30: { count: 7, bytes: 7000 },
      older_90: { count: 4, bytes: 4000 },
    },
  });
}

function makeTrash(count) {
  return Array.from({ length: count }, (_, index) => ({
    id: 700 + index,
    kind: index % 4 === 1 ? "图片" : index % 4 === 3 ? "文件" : "文本",
    preview: index % 4 === 3 ? "" : `回收内容 ${index + 1}`,
    file_names: index % 4 === 3 ? [`材料-${index + 1}.pdf`] : [],
    source: "Example.exe", copied_ms: Date.now() - index * 1000,
    copy_count: 1, partial: false, favorite: false, favorite_position: null,
    source_name: "示例程序", source_icon: null,
    deleted_ms: Date.now() - index * 3600_000,
  }));
}
let trash = makeTrash(Number(initialQuery.get("trash") ?? 0));
if (initialQuery.get("trashNextDay") === "1" && trash.length) {
  const now = new Date();
  const expiry = new Date(now);
  expiry.setDate(now.getDate() + 1);
  expiry.setMinutes(now.getMinutes() - 1);
  trash[0].deleted_ms = expiry.getTime() - 7 * 24 * 60 * 60 * 1000;
}
let trashReadErrors = initialQuery.get("trashError") === "1" ? 1 : 0;
let trashRestoreErrors = 0;
let trashRestoreNulls = initialQuery.get("restoreNull") === "1" ? 1 : 0;
let storageReadErrors = 0;
let deferStorageReads = initialQuery.get("storageDeferred") === "1";
const deferredStorageReads = [];
let deferTrashReads = initialQuery.get("trashDeferred") === "1";
const deferredTrashReads = [];
let deferPurges = false;
const deferredPurges = [];
let deferRestores = false;
const deferredRestores = [];
let capturePausedErrors = 0;
let captureIgnoreErrors = 0;
let captureState = {
  paused: false, ignore_next: false,
  rules: [{ id: "jwt", label: "登录凭证（JWT）", example: "eyJ…" }],
  default_excluded_apps: ["KeePass.exe"],
};
let recentApps = initialQuery.get("recent") === "1" ? ["Recent.exe"] : [];
let snapshotItems = initialQuery.get("snapshots") === "1" ? [{
  name: "test-snapshot", bytes: 4096, created_ms: Date.now(), reason: "daily", version: 1,
}] : [];
function syncTrashStorage() {
  storage.trashed = { count: trash.length, bytes: trash.length * 100 };
}
syncTrashStorage();
function writePurge(args) {
  const ids = args.ids;
  const removed = ids === null ? trash.length : trash.filter((item) => ids.includes(item.id)).length;
  trash = ids === null ? [] : trash.filter((item) => !ids.includes(item.id));
  syncTrashStorage();
  return removed;
}

function deferredSave(update) {
  const id = ++nextSaveId;
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  saves.push({ id, update: clone(update), resolve, reject, settled: false });
  return { id, promise };
}

function readAutostart() {
  if (autostartReadErrors > 0) {
    autostartReadErrors -= 1;
    throw new Error("模拟开机启动回读失败");
  }
  return autostartEnabled;
}

export async function invoke(command, args = {}) {
  invocations.push({ command, args: clone(args) });
  switch (command) {
    case "get_settings": {
      const count = Number(sessionStorage.getItem(SETTINGS_READ_KEY) ?? 0);
      sessionStorage.setItem(SETTINGS_READ_KEY, String(count + 1));
      if (new URLSearchParams(location.search).get("loadError") === "1") throw new Error("模拟设置读取失败");
      return clone(settings);
    }
    case "set_settings": {
      const update = args.settings;
      if (manualSaves) return deferredSave(update).promise;
      saves.push({ id: ++nextSaveId, update: clone(update), settled: true });
      settings = merge(settings, update);
      persistSettings();
      emit("settings:changed", clone(settings));
      return clone(settings);
    }
    case "get_hotkey_status":
      return { requested: settings.hotkey, active: settings.hotkey, error: null, detail: null, warning: null, overridden: false, disabled: false };
    case "validate_hotkey": {
      if (hotkeyValidationError) throw new Error(hotkeyValidationError);
      const parts = String(args.value).split("+");
      const key = parts.at(-1);
      return {
        hotkey: args.value, key,
        control: parts.includes("Ctrl"), alt: parts.includes("Alt"),
        shift: parts.includes("Shift"), win: parts.includes("Super"), warning: null,
      };
    }
    case "pause_hotkey": return;
    case "get_autostart": {
      if (deferAutostartGet) {
        const call = deferred();
        deferredAutostartGets.push(call);
        return call.promise;
      }
      return readAutostart();
    }
    case "set_autostart": {
      if (deferAutostartSet) {
        const call = deferred();
        deferredAutostartSets.push(call);
        return call.promise;
      }
      if (autostartWriteError) {
        const error = autostartWriteError;
        autostartWriteError = null;
        throw new Error(error);
      }
      autostartEnabled = autostartExternalOnWrite ?? args.enabled;
      autostartExternalOnWrite = null;
      return readAutostart();
    }
    case "get_capture_state":
      if (new URLSearchParams(location.search).get("captureError") === "1") throw new Error("模拟记录状态读取失败");
      return clone(captureState);
    case "set_capture_paused":
      if (capturePausedErrors > 0) {
        capturePausedErrors -= 1;
        throw new Error("模拟暂停写失败");
      }
      captureState.paused = args.paused;
      emit("capture:changed", { paused: captureState.paused, ignore_next: captureState.ignore_next });
      return;
    case "set_ignore_next":
      if (captureIgnoreErrors > 0) {
        captureIgnoreErrors -= 1;
        throw new Error("模拟下一次写失败");
      }
      captureState.ignore_next = args.ignore;
      emit("capture:changed", { paused: captureState.paused, ignore_next: captureState.ignore_next });
      return;
    case "get_history":
      return {
        items: recentApps.map((source, id) => ({
          id, kind: "文本", preview: "来源建议", file_names: [], source, copied_ms: 0,
          copy_count: 1, partial: false, favorite: false, favorite_position: null, source_name: null, source_icon: null,
        })),
        total: recentApps.length, recording: true, last_recorded_ms: null, store_status: null,
      };
    case "get_favorites": return favoriteList();
    case "set_favorite_label": {
      if (deferFavoriteLabels) {
        const call = { ...deferred(), id: args.id, args: clone(args) };
        deferredFavoriteLabels.push(call);
        return call.promise;
      }
      return writeFavoriteLabel(args);
    }
    case "reorder_favorites": {
      if (deferFavoriteOrders) {
        const call = { ...deferred(), id: invocations.length, ids: clone(args.ids) };
        deferredFavoriteOrders.push(call);
        return call.promise;
      }
      return writeFavoriteOrder(args.ids);
    }
    case "set_favorite": {
      if (deferFavoriteWrites) {
        const call = { ...deferred(), id: args.id, args: clone(args) };
        deferredFavoriteWrites.push(call);
        return call.promise;
      }
      return writeFavorite(args);
    }
    case "get_trash":
      if (deferTrashReads) {
        const call = { ...deferred(), snapshot: clone(trash) };
        deferredTrashReads.push(call);
        return call.promise;
      }
      if (trashReadErrors > 0) {
        trashReadErrors -= 1;
        throw new Error("模拟回收站读取失败");
      }
      return clone(trash);
    case "restore_item": {
      if (deferRestores) {
        const call = { ...deferred(), args: clone(args) };
        deferredRestores.push(call);
        return call.promise;
      }
      if (trashRestoreErrors > 0) {
        trashRestoreErrors -= 1;
        throw new Error("模拟拿回失败");
      }
      const index = trash.findIndex((item) => item.id === args.id);
      if (trashRestoreNulls > 0) {
        trashRestoreNulls -= 1;
        if (index >= 0) {
          trash.splice(index, 1);
          syncTrashStorage();
        }
        return null;
      }
      if (index < 0) return null;
      const [restored] = trash.splice(index, 1);
      syncTrashStorage();
      return clone(restored);
    }
    case "purge_items": {
      if (deferPurges) {
        const call = { ...deferred(), args: clone(args) };
        deferredPurges.push(call);
        return call.promise;
      }
      return writePurge(args);
    }
    case "clean_missing": {
      const removed = storage.files.missing;
      storage.files.missing = 0;
      return removed;
    }
    case "clean_images": {
      const key = args.olderDays === null ? "all" : args.olderDays === 30 ? "older_30" : "older_90";
      const removed = storage.images[key].count;
      storage.images[key] = { count: 0, bytes: 0 };
      return removed;
    }
    case "get_thumbnail": return null;
    case "get_images": return [];
    case "get_storage":
      if (deferStorageReads) {
        const call = { ...deferred(), snapshot: clone(storage) };
        deferredStorageReads.push(call);
        return call.promise;
      }
      if (storageReadErrors > 0) {
        storageReadErrors -= 1;
        throw new Error("模拟占用读取失败");
      }
      return clone(storage);
    case "get_snapshots":
      return { items: clone(snapshotItems), keep: 7, current_version: 1, restored: null, relaunch: false };
    case "create_snapshot": return;
    case "restore_snapshot": return;
    case "get_data_dir":
      if (new URLSearchParams(location.search).get("dirError") === "1") throw new Error("模拟目录读取失败");
      return "D:\\test-data\\nianlai";
    case "open_data_dir": return;
    case "get_license_notices":
      return { nianlai: nianlaiLicense, third_party: thirdPartyNotices };
    case "backup_summary": return { items: 0, favorites: 0, icons: 0, bytes: 0, default_name: "test.nianlai-backup" };
    case "choose_backup_save":
      return initialQuery.get("backupReady") === "1" ? "D:\\test-data\\test.nianlai-backup" : null;
    case "export_backup":
      return { path: args.path, bytes: 1234, items: 2, favorites: 1, icons: 1 };
    case "choose_backup_open":
      return initialQuery.get("backupReady") === "1" || initialQuery.get("backupFail") === "1" ? "test.nianlai-backup" : null;
    case "inspect_backup": {
      const ok = initialQuery.get("backupFail") !== "1";
      return {
        token: "test-token", file_name: "test.nianlai-backup", exported_ms: Date.now(), app_version: "0.0.0-test",
        schema_version: 1, upgraded_from: null,
        steps: [
          { id: "manifest", ok, detail: ok ? "清单完整" : "清单坏了" },
          { id: "hashes", ok, detail: ok ? "文件一致" : "文件不一致" },
          { id: "database", ok, detail: ok ? "内容能打开" : "内容打不开" },
        ],
        ok, merge: { new_items: 2, new_favorites: 1 }, overwrite: { items: 9 },
      };
    }
    case "import_backup":
      return { mode: args.mode, added_items: 2, added_favorites: 1, relaunch: args.mode === "overwrite", auto_restart: true };
    case "discard_import": return;
    case "open_settings": return;
    default:
      // 本骨架不执行清理、删除或备份写入；未覆盖命令保持无副作用。
      return null;
  }
}

export async function listen(event, handler) {
  if (event === "capture:changed" && new URLSearchParams(location.search).get("captureListenError") === "1") {
    throw new Error("模拟记录状态订阅失败");
  }
  const handlers = listeners.get(event) ?? new Set();
  handlers.add(handler);
  listeners.set(event, handlers);
  return () => {
    handlers.delete(handler);
    if (!handlers.size) listeners.delete(event);
  };
}

export async function getVersion() {
  if (new URLSearchParams(location.search).get("versionError") === "1") throw new Error("模拟版本读取失败");
  return "0.0.0-test";
}

function emit(event, payload) {
  for (const handler of listeners.get(event) ?? []) handler({ event, payload });
}

// 共享 ipc.ts 的 guide 边界也在这里补齐；设置测试不触发发牌或关闭 guide。
export async function emitTo(target, event, payload) {
  if (target === "settings") emit(event, payload);
}
export function getCurrentWindow() {
  return { close: async () => {} };
}

function useManualSaves() { manualSaves = true; }
function saveCalls() { return saves.map(({ id, update, settled }) => ({ id, update: clone(update), settled })); }
function resolveSave(id, response) {
  const call = saves.find((entry) => entry.id === id);
  if (!call || call.settled) throw new Error(`没有等待中的保存 ${id}`);
  call.settled = true;
  settings = response ? clone(response) : merge(settings, call.update);
  persistSettings();
  emit("settings:changed", clone(settings));
  call.resolve(clone(settings));
}
function rejectSave(id, reason) {
  const call = saves.find((entry) => entry.id === id);
  if (!call || call.settled) throw new Error(`没有等待中的保存 ${id}`);
  call.settled = true;
  call.reject(new Error(reason));
}

// 可替换计时：只接管安装之后创建的计时器，测试不等待真实 1.5 秒。
let clock = null;
function installClock() {
  if (clock) return;
  const originalSet = window.setTimeout;
  const originalClear = window.clearTimeout;
  let now = 0;
  let nextId = 1;
  const timers = new Map();
  window.setTimeout = (callback, delay = 0, ...args) => {
    const id = nextId++;
    timers.set(id, { at: now + Math.max(0, Number(delay) || 0), callback, args });
    return id;
  };
  window.clearTimeout = (id) => timers.delete(id);
  clock = { originalSet, originalClear, timers, get now() { return now; }, set now(value) { now = value; } };
}
function tickClock(milliseconds) {
  if (!clock) throw new Error("假时钟尚未安装");
  const target = clock.now + milliseconds;
  for (;;) {
    const due = [...clock.timers.entries()]
      .filter(([, timer]) => timer.at <= target)
      .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
    if (!due) break;
    const [id, timer] = due;
    clock.timers.delete(id);
    clock.now = timer.at;
    timer.callback(...timer.args);
  }
  clock.now = target;
}

export const testApi = {
  emit, useManualSaves, saveCalls, resolveSave, rejectSave, installClock, tickClock,
  defaults: () => clone(DEFAULT_SETTINGS),
  settings: () => clone(settings),
  invokeCalls: (command) => clone(command ? invocations.filter((call) => call.command === command) : invocations),
  autostartScenario: ({ writeError = null, readErrors = 0, externalOnWrite = null } = {}) => {
    autostartWriteError = writeError;
    autostartReadErrors = readErrors;
    autostartExternalOnWrite = externalOnWrite;
  },
  setAutostartExternal: (enabled) => { autostartEnabled = enabled; },
  trayToggleAutostart: () => { autostartEnabled = !autostartEnabled; },
  autostartDeferred: ({ set = deferAutostartSet, get = deferAutostartGet } = {}) => {
    deferAutostartSet = set;
    deferAutostartGet = get;
  },
  pendingAutostart: () => ({ set: deferredAutostartSets.length, get: deferredAutostartGets.length }),
  resolveAutostartSet: (actual) => {
    const call = deferredAutostartSets.shift();
    if (!call) throw new Error("没有等待中的 autostart set");
    autostartEnabled = actual;
    call.resolve(actual);
  },
  rejectAutostartSet: (reason) => {
    const call = deferredAutostartSets.shift();
    if (!call) throw new Error("没有等待中的 autostart set");
    call.reject(new Error(reason));
  },
  resolveAutostartGet: () => {
    const call = deferredAutostartGets.shift();
    if (!call) throw new Error("没有等待中的 autostart get");
    try { call.resolve(readAutostart()); } catch (error) { call.reject(error); }
  },
  settingsReadCount: () => Number(sessionStorage.getItem(SETTINGS_READ_KEY) ?? 0),
  favorites: favoriteList,
  favoriteScenario: ({ labelErrors = 0, reorderErrors = 0, favoriteErrors = 0 } = {}) => {
    favoriteLabelErrors = labelErrors;
    favoriteOrderErrors = reorderErrors;
    favoriteWriteErrors = favoriteErrors;
  },
  favoriteDeferred: ({ label = deferFavoriteLabels, order = deferFavoriteOrders, favorite = deferFavoriteWrites } = {}) => {
    deferFavoriteLabels = label;
    deferFavoriteOrders = order;
    deferFavoriteWrites = favorite;
  },
  pendingFavorites: () => ({ label: deferredFavoriteLabels.length, order: deferredFavoriteOrders.length, favorite: deferredFavoriteWrites.length }),
  resolveFavoriteLabel: (id) => {
    const call = takeDeferred(deferredFavoriteLabels, id);
    try { call.resolve(writeFavoriteLabel(call.args)); } catch (error) { call.reject(error); }
  },
  rejectFavoriteLabel: (id, reason = "模拟延迟标题备注失败") => {
    takeDeferred(deferredFavoriteLabels, id).reject(new Error(reason));
  },
  resolveFavoriteOrder: () => {
    const call = deferredFavoriteOrders.shift();
    if (!call) throw new Error("没有等待中的顺序保存");
    try { call.resolve(writeFavoriteOrder(call.ids)); } catch (error) { call.reject(error); }
  },
  rejectFavoriteOrder: (reason = "模拟延迟顺序失败") => {
    const call = deferredFavoriteOrders.shift();
    if (!call) throw new Error("没有等待中的顺序保存");
    call.reject(new Error(reason));
  },
  resolveFavoriteWrite: (id) => {
    const call = takeDeferred(deferredFavoriteWrites, id);
    try { call.resolve(writeFavorite(call.args)); } catch (error) { call.reject(error); }
  },
  rejectFavoriteWrite: (id, reason = "模拟延迟常用状态失败") => {
    takeDeferred(deferredFavoriteWrites, id).reject(new Error(reason));
  },
  hotkeyScenario: ({ validationError = null } = {}) => { hotkeyValidationError = validationError; },
  captureScenario: ({ apps = recentApps, pausedErrors = 0, ignoreErrors = 0 } = {}) => {
    recentApps = [...apps];
    capturePausedErrors = pausedErrors;
    captureIgnoreErrors = ignoreErrors;
  },
  trashScenario: ({ restoreErrors = 0, readErrors = 0, restoreNulls = 0 } = {}) => {
    trashRestoreErrors = restoreErrors;
    trashReadErrors = readErrors;
    trashRestoreNulls = restoreNulls;
  },
  storageScenario: ({ readErrors = 0 } = {}) => { storageReadErrors = readErrors; },
  storageReadDeferred: (enabled = true) => { deferStorageReads = enabled; },
  pendingStorageReads: () => deferredStorageReads.length,
  resolveStorageRead: (index = 0, value) => {
    const [call] = deferredStorageReads.splice(index, 1);
    if (!call) throw new Error(`没有等待中的占用读取 ${index}`);
    call.resolve(clone(value ?? call.snapshot));
  },
  rejectStorageRead: (index = 0, reason = "模拟延迟占用读取失败") => {
    const [call] = deferredStorageReads.splice(index, 1);
    if (!call) throw new Error(`没有等待中的占用读取 ${index}`);
    call.reject(new Error(reason));
  },
  trashReadDeferred: (enabled = true) => { deferTrashReads = enabled; },
  pendingTrashReads: () => deferredTrashReads.length,
  resolveTrashRead: (index = 0, items) => {
    const [call] = deferredTrashReads.splice(index, 1);
    if (!call) throw new Error(`没有等待中的回收站读取 ${index}`);
    call.resolve(clone(items ?? call.snapshot));
  },
  rejectTrashRead: (index = 0, reason = "模拟延迟回收站读取失败") => {
    const [call] = deferredTrashReads.splice(index, 1);
    if (!call) throw new Error(`没有等待中的回收站读取 ${index}`);
    call.reject(new Error(reason));
  },
  purgeDeferred: (enabled = true) => { deferPurges = enabled; },
  pendingPurges: () => deferredPurges.length,
  resolvePurge: (index = 0) => {
    const [call] = deferredPurges.splice(index, 1);
    if (!call) throw new Error(`没有等待中的彻底删除 ${index}`);
    call.resolve(writePurge(call.args));
  },
  rejectPurge: (index = 0, reason = "模拟彻底清空失败") => {
    const [call] = deferredPurges.splice(index, 1);
    if (!call) throw new Error(`没有等待中的彻底删除 ${index}`);
    call.reject(new Error(reason));
  },
  restoreDeferred: (enabled = true) => { deferRestores = enabled; },
  pendingRestores: () => deferredRestores.length,
  resolveRestore: (index = 0) => {
    const [call] = deferredRestores.splice(index, 1);
    if (!call) throw new Error(`没有等待中的拿回 ${index}`);
    const itemIndex = trash.findIndex((item) => item.id === call.args.id);
    if (itemIndex < 0) { call.resolve(null); return; }
    const [restored] = trash.splice(itemIndex, 1);
    syncTrashStorage();
    call.resolve(clone(restored));
  },
  rejectRestore: (index = 0, reason = "模拟延迟拿回失败") => {
    const [call] = deferredRestores.splice(index, 1);
    if (!call) throw new Error(`没有等待中的拿回 ${index}`);
    call.reject(new Error(reason));
  },
  trashItems: () => clone(trash),
  storage: () => clone(storage),
};
