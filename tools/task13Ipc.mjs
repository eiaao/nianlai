// Task 13 真实组件测试的内存 Tauri 边界；不触碰系统剪贴板、按键、注册表或真实窗口。
const clone = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));
const query = new URLSearchParams(location.search);
const view = query.get("view") ?? "panel";
function merge(base, update) {
  if (base && update && typeof base === "object" && typeof update === "object" && !Array.isArray(base) && !Array.isArray(update)) {
    const result = { ...base };
    for (const [key, value] of Object.entries(update)) result[key] = merge(result[key], value);
    return result;
  }
  return clone(update);
}
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const realItem = {
  id: 101, kind: "文本", preview: "真实第一张", file_names: [], source: "Notepad.exe", copied_ms: 100,
  copy_count: 1, partial: false, favorite: query.get("favorite") === "1", favorite_position: query.get("favorite") === "1" ? 1 : null, source_name: "记事本", source_icon: null,
};
const secondItem = { ...realItem, id: 102, preview: "真实第二张", copied_ms: 90 };
const phase = query.get("phase") ?? "completed";
let settings = {
  hotkey: "Ctrl+`", theme: "light",
  feedback: { motion: "none", follow_system: false },
  interaction: { placement: "caret", click: "paste", middle_click: "text" },
  capture: { excluded_apps: [], sensitive_off: [], text: true, rich_text: true, images: true, files: true, max_text_kb: 2048, transient: false, transient_ms: 1000 },
  snapshots: { max_mb: 1024 }, history: { reuse_to_top: true },
  guide: {
    version: 1, phase,
    baseline_ms: ["await_copy", "await_show", "await_paste"].includes(phase) ? 100 : null,
    baseline_total: ["await_copy", "await_show", "await_paste"].includes(phase) ? 1 : 0,
    sample_id: ["await_show", "await_paste"].includes(phase) ? 101 : null,
    cards_dealt: query.get("dealt") === "0" ? false : phase === "completed" || query.get("dealt") === "1",
    dismissed: (query.get("dismissed") ?? "").split(",").filter(Boolean),
  },
};
let history = {
  items: query.get("empty") === "1" ? [] : query.get("two") === "1" ? [clone(realItem), clone(secondItem)] : [clone(realItem)],
  total: query.get("empty") === "1" ? 0 : query.get("two") === "1" ? 2 : 1,
  recording: true, last_recorded_ms: 100, store_status: null,
};
const deletedItems = [];
let session = 40;
let manualSaves = query.get("manualSaves") === "1";
let nextSave = 0;
const saves = [];
const invocations = [];
const listeners = new Map();
const targetedEvents = [];
let autostart = query.get("autostart") === "1";
let autostartWriteError = null;
let autostartReadErrors = 0;
let deferAutostartSet = false;
let deferAutostartGet = false;
const autostartSets = [];
const autostartGets = [];
let openGuideErrors = 0;
let deferOpenGuide = false;
const openGuideCalls = [];
let openSettingsErrors = 0;
let notifyErrors = 0;
let deferGuideNotifications = false;
const guideNotifications = [];
let validationError = null;
let saveErrors = 0;

function emit(name, payload = {}) {
  for (const handler of [...(listeners.get(name) ?? [])]) handler({ event: name, payload: clone(payload) });
}
function shown(overrides = {}) {
  return {
    session: ++session, interaction: "quick", foregroundRevision: 1, hotkeyToShowMs: 1,
    history: clone(history), guide: clone(settings.guide), ...clone(overrides),
  };
}
function readAutostart() {
  if (autostartReadErrors > 0) { autostartReadErrors -= 1; throw new Error("模拟自启回读失败"); }
  return autostart;
}

export async function invoke(command, args = {}) {
  invocations.push({ command, args: clone(args), at: invocations.length + 1 });
  switch (command) {
    case "get_settings": return clone(settings);
    case "set_settings": {
      const update = clone(args.settings);
      if (manualSaves) {
        const call = { id: ++nextSave, update, status: "pending", ...deferred() };
        saves.push(call);
        return call.promise;
      }
      if (saveErrors > 0) { saveErrors -= 1; throw new Error("模拟设置保存失败"); }
      settings = merge(settings, update);
      emit("settings:changed", clone(settings));
      return clone(settings);
    }
    case "get_history": return clone(history);
    case "get_favorites": return history.items.filter((item) => item.favorite);
    case "search_history": return { request_id: args.requestId, items: history.items.filter((item) => item.preview.includes(args.query)) };
    case "get_autostart": {
      if (deferAutostartGet) { const call = deferred(); autostartGets.push(call); return call.promise; }
      return readAutostart();
    }
    case "set_autostart": {
      if (deferAutostartSet) { const call = { ...deferred(), requested: args.enabled }; autostartSets.push(call); return call.promise; }
      if (autostartWriteError) { const error = autostartWriteError; autostartWriteError = null; throw new Error(error); }
      autostart = args.enabled;
      return readAutostart();
    }
    case "validate_hotkey": {
      if (validationError) throw new Error(validationError);
      return { hotkey: args.value, key: String(args.value).split("+").at(-1), control: true, alt: false, shift: false, win: false, warning: null };
    }
    case "get_hotkey_status": return { requested: settings.hotkey, active: settings.hotkey, error: null, detail: null, warning: null, overridden: false, disabled: false };
    case "open_settings":
      if (openSettingsErrors > 0) { openSettingsErrors -= 1; throw new Error("模拟设置窗口打开失败"); }
      if (view === "pair") queueMicrotask(() => emit("settings:navigate", args.page));
      return;
    case "open_guide":
      if (openGuideErrors > 0) { openGuideErrors -= 1; throw new Error("模拟引导窗口打开失败"); }
      if (deferOpenGuide) { const call = deferred(); openGuideCalls.push(call); return call.promise; }
      return;
    case "hide_panel": emit("panel:hidden", { session: args.session }); return;
    case "set_favorite": return { ...realItem, id: args.id, favorite: args.favorite };
    case "delete_item": {
      const index = history.items.findIndex((item) => item.id === args.id);
      if (index < 0) return false;
      const [removed] = history.items.splice(index, 1);
      deletedItems.push(clone(removed));
      history.total = Math.max(0, history.total - 1);
      return true;
    }
    case "undo_delete": {
      const restored = deletedItems.pop();
      if (!restored) return null;
      history.items = [clone(restored), ...history.items];
      history.total += 1;
      return clone(restored);
    }
    case "get_full_text": return { text: "不应读取新手牌", total: 7, shown: 7 };
    case "get_image": return null;
    case "get_file_paths": return null;
    case "copy_selection": return;
    case "start_queue": return;
    case "get_data_dir": return "D:\\test-data";
    case "get_license_notices": return { nianlai: "MIT", third_party: "Third party" };
    case "open_data_dir": return;
    default: return null;
  }
}
export function listen(name, handler) {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return Promise.resolve(() => { handlers.delete(handler); if (!handlers.size) listeners.delete(name); });
}
export function emitTo(target, name, payload) {
  targetedEvents.push({ target, name, payload: clone(payload) });
  if (view === "pair" || (target === "panel" && view === "panel") || (target === "settings" && view === "settings")) emit(name, payload);
  if (notifyErrors > 0 && name === "guide:cards-dealt") { notifyErrors -= 1; return Promise.reject(new Error("模拟定向通知失败")); }
  if (deferGuideNotifications && name === "guide:cards-dealt") { const call = deferred(); guideNotifications.push(call); return call.promise; }
  return Promise.resolve();
}
export function getVersion() { return Promise.resolve("0.0.0-task13"); }
export function getCurrentWindow() { return { close: () => Promise.resolve() }; }

export const testApi = {
  emit,
  shown,
  settings: () => clone(settings),
  history: () => clone(history),
  invokeCalls: (command) => clone(command ? invocations.filter((call) => call.command === command) : invocations),
  clearInvokeCalls: () => { invocations.length = 0; },
  targetedEvents: () => clone(targetedEvents),
  useManualSaves: (value = true) => { manualSaves = value; },
  saveCalls: () => saves.map(({ id, update, status }) => ({ id, update: clone(update), status })),
  pendingSaves: () => saves.filter((save) => save.status === "pending").map((save) => save.id),
  resolveSave: (id, response = null) => {
    const save = saves.find((entry) => entry.id === id && entry.status === "pending");
    if (!save) throw new Error(`没有待保存 ${id}`);
    save.status = "saved";
    settings = response ? clone(response) : merge(settings, save.update);
    emit("settings:changed", clone(settings));
    save.resolve(clone(settings));
  },
  resolveSaveSilently: (id, response) => {
    const save = saves.find((entry) => entry.id === id && entry.status === "pending");
    if (!save) throw new Error(`没有待保存 ${id}`);
    save.status = "saved"; save.resolve(clone(response));
  },
  rejectSave: (id, reason = "模拟保存失败") => {
    const save = saves.find((entry) => entry.id === id && entry.status === "pending");
    if (!save) throw new Error(`没有待保存 ${id}`);
    save.status = "failed"; save.reject(new Error(reason));
  },
  failNextSave: () => { saveErrors += 1; },
  externalGuide: (patch) => { settings = merge(settings, { guide: patch }); emit("settings:changed", clone(settings)); },
  silentlyGuide: (patch) => { settings = merge(settings, { guide: patch }); },
  setHistory: (value) => { history = clone(value); },
  autostartScenario: ({ writeError = null, readErrors = 0 } = {}) => { autostartWriteError = writeError; autostartReadErrors = readErrors; },
  setAutostart: (value) => { autostart = value; },
  deferAutostart: ({ set = false, get = false } = {}) => { deferAutostartSet = set; deferAutostartGet = get; },
  pendingAutostart: () => ({ set: autostartSets.length, get: autostartGets.length }),
  resolveAutostartSet: (actual) => { const call = autostartSets.shift(); if (!call) throw new Error("没有待处理自启写入"); autostart = actual; call.resolve(actual); },
  rejectAutostartSet: (reason) => { const call = autostartSets.shift(); if (!call) throw new Error("没有待处理自启写入"); call.reject(new Error(reason)); },
  resolveAutostartGet: () => { const call = autostartGets.shift(); if (!call) throw new Error("没有待处理自启读取"); try { call.resolve(readAutostart()); } catch (error) { call.reject(error); } },
  failOpenGuide: () => { openGuideErrors += 1; },
  deferOpenGuide: (value = true) => { deferOpenGuide = value; },
  pendingOpenGuide: () => openGuideCalls.length,
  resolveOpenGuide: () => { const call = openGuideCalls.shift(); if (!call) throw new Error("没有待处理引导窗口打开"); call.resolve(); },
  deferGuideNotification: (value = true) => { deferGuideNotifications = value; },
  pendingGuideNotifications: () => guideNotifications.length,
  resolveGuideNotification: () => { const call = guideNotifications.shift(); if (!call) throw new Error("没有待处理新手牌通知"); call.resolve(); },
  deletedCount: () => deletedItems.length,
  failOpenSettings: () => { openSettingsErrors += 1; },
  failNotify: () => { notifyErrors += 1; },
  hotkeyValidationError: (reason) => { validationError = reason; },
  activeListeners: () => [...listeners.entries()].map(([name, handlers]) => ({ name, count: handlers.size })),
};
