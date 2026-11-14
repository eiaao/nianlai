// tools/test-guide-component.mjs 的内存 Tauri 替身。
// 只替换 core/event/window 边界；真实 Guide.svelte、GuideFlow.reduce 与 src/lib/ipc.ts 均按生产路径执行。
const clone = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));
const query = new URLSearchParams(location.search);
const scenario = query.get("scenario") ?? "default";
const STORAGE_KEY = `nianlai-guide-${scenario}`;

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
function historyItem(id, copiedMs, preview = `记下的内容 ${id}`) {
  return {
    id, kind: "文本", preview, file_names: [], source: "Notepad.exe", copied_ms: copiedMs,
    copy_count: 1, partial: false, favorite: false, favorite_position: null,
    source_name: "记事本", source_icon: null,
  };
}
const oldItem = historyItem(1, 100, "打开引导前已经记下的一句");
const copiedItem = historyItem(2, 200, "这是刚复制并真实记下的一句话");
const newerItem = historyItem(3, 300, "呼出前又复制了这一句");
const oldSnapshot = { items: [oldItem], total: 1, recording: true, last_recorded_ms: 100, store_status: null };
const copiedSnapshot = { items: [copiedItem, oldItem], total: 2, recording: true, last_recorded_ms: 200, store_status: null };
const newerSnapshot = { items: [newerItem, copiedItem, oldItem], total: 3, recording: true, last_recorded_ms: 300, store_status: null };

const phase = query.get("phase") ?? "await_copy";
const sampleId = Number(query.get("sample") ?? (phase === "await_show" || phase === "await_paste" ? 2 : 0)) || null;
let settings = {
  hotkey: "Ctrl+`",
  theme: query.get("theme") ?? "light",
  feedback: { motion: query.get("motion") ?? "spring", follow_system: query.get("followSystem") === "1" },
  guide: {
    version: 1, phase,
    baseline_ms: phase === "await_copy" ? null : Number(query.get("baselineMs") ?? 200),
    baseline_total: phase === "await_copy" ? 0 : Number(query.get("baselineTotal") ?? 2),
    sample_id: sampleId,
    cards_dealt: phase === "completed",
    dismissed: [],
  },
};
let history = clone(oldSnapshot);
if (query.get("history") === "copied" || phase !== "await_copy") history = clone(copiedSnapshot);
if (query.get("history") === "newer") history = clone(newerSnapshot);
if (query.get("history") === "empty") history = { items: [], total: 0, recording: true, last_recorded_ms: null, store_status: null };
if (query.get("recording") === "0") history.recording = false;
if (query.get("storeStatus")) history.store_status = query.get("storeStatus");
let captureState = { paused: query.get("paused") === "1", ignore_next: false, rules: [], default_excluded_apps: [] };
let guideHealth = {
  store: query.get("storeHealth") ?? "ready",
  capture: query.get("captureHealth") ?? "recording",
  recorded_today: 1,
};
let panelState = { visible: false, session: Number(query.get("panelSession") ?? 10) };
let manualPanel = false;
let pendingPanelOpen = null;
const panelTransitions = [];
let manualSaves = query.get("manualSaves") === "1";
let historyFailures = Number(query.get("historyErrors") ?? 0);
let settingsFailures = Number(query.get("settingsErrors") ?? 0);
let captureFailures = Number(query.get("captureErrors") ?? 0);
let copyFailures = 0;
let openPanelFailures = 0;
let settingsReadFailures = Number(query.get("settingsReadErrors") ?? 0);
let manualHistory = query.get("manualHistory") === "1";
let nextSaveId = 0;
let nextHistoryId = 0;
let closeCalls = 0;
const saves = [];
const historyReads = [];
const invocations = [];
const targetedEvents = [];
const listeners = new Map();
const hitRegionCalls = [];

function hidePanel() {
  if (!panelState.visible) return;
  panelState = { ...panelState, visible: false };
  panelTransitions.push({ event: "hidden", session: panelState.session });
  emit("panel:hidden", { session: panelState.session });
}
function showPanel() {
  panelState = { visible: true, session: panelState.session + 1 };
  panelTransitions.push({ event: "shown", session: panelState.session });
  emit("guide:panel-shown", { session: panelState.session });
}
function openGuidePanel() {
  if (openPanelFailures > 0) { openPanelFailures -= 1; throw new Error("模拟面板打开失败"); }
  // Visible 时专用 guide 入口先收再开；Hidden 只开。两种路径都只在真实展示时递增 session。
  hidePanel();
  if (!manualPanel) {
    showPanel();
    return;
  }
  const pending = deferred();
  pendingPanelOpen = pending;
  return pending.promise.then(() => showPanel());
}
function pasteResult(id, result, hide = false) {
  if (hide) hidePanel();
  emit("guide:paste-result", { session: panelState.session, id, result });
}

export async function invoke(command, args = {}) {
  invocations.push({ command, args: clone(args), at: invocations.length + 1 });
  switch (command) {
    case "get_settings":
      if (settingsReadFailures > 0) { settingsReadFailures -= 1; throw new Error("模拟设置读取失败"); }
      return clone(settings);
    case "set_settings": {
      const update = clone(args.settings);
      if (!manualSaves) {
        if (settingsFailures > 0) { settingsFailures -= 1; throw new Error("模拟设置保存失败"); }
        settings = merge(settings, update);
        emit("settings:changed", clone(settings));
        return clone(settings);
      }
      const call = { id: ++nextSaveId, update, status: "pending", ...deferred() };
      saves.push(call);
      return call.promise;
    }
    case "get_history": {
      const call = { id: ++nextHistoryId, ...deferred() };
      historyReads.push(call);
      if (manualHistory) return call.promise;
      if (historyFailures > 0) { historyFailures -= 1; throw new Error("模拟复制记录读取失败"); }
      call.resolve(clone(history));
      return call.promise;
    }
    case "get_capture_state":
      if (captureFailures > 0) { captureFailures -= 1; throw new Error("模拟记录状态读取失败"); }
      return clone(captureState);
    case "get_guide_health":
      return clone(guideHealth);
    case "guide_copy_sample":
      if (copyFailures > 0) { copyFailures -= 1; throw new Error("模拟示例复制失败"); }
      return;
    case "guide_open_panel":
      return openGuidePanel();
    case "open_settings": return;
    case "open_guide": return;
    case "set_guide_hit_regions":
      hitRegionCalls.push(clone(args));
      return;
    default: throw new Error(`guide 测试没有替换命令：${command}`);
  }
}

export function listen(name, handler) {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return Promise.resolve(() => {
    handlers.delete(handler);
    if (!handlers.size) listeners.delete(name);
  });
}
export function emit(name, payload) {
  for (const handler of [...(listeners.get(name) ?? [])]) handler({ payload: clone(payload) });
}
export function emitTo(target, name, payload) {
  targetedEvents.push({ target, name, payload: clone(payload) });
  if (target === "guide") emit(name, payload);
  return Promise.resolve();
}
const currentWindow = {
  close: () => { closeCalls += 1; return Promise.resolve(); },
};
export function getCurrentWindow() { return currentWindow; }
export function getVersion() { return Promise.resolve("0.0.0-guide-test"); }

function takeSave(id) {
  const save = saves.find((entry) => entry.id === id);
  if (!save || save.status !== "pending") throw new Error(`没有等待中的保存 ${id}`);
  return save;
}
function settleSave(id, error = null) {
  const save = takeSave(id);
  save.status = error ? "failed" : "saved";
  if (error) {
    save.reject(new Error(error));
  } else {
    settings = merge(settings, save.update);
    emit("settings:changed", clone(settings));
    save.resolve(clone(settings));
  }
}
function resolveHistory(id, snapshot = history) {
  const call = historyReads.find((entry) => entry.id === id);
  if (!call) throw new Error(`没有这次记录读取 ${id}`);
  call.resolve(clone(snapshot));
}

export const testApi = {
  snapshots: { old: clone(oldSnapshot), copied: clone(copiedSnapshot), newer: clone(newerSnapshot) },
  emit,
  settings: () => clone(settings),
  history: () => clone(history),
  setHistory: (snapshot) => { history = clone(snapshot); },
  copyLatest: () => { history = clone(copiedSnapshot); },
  copyNewer: () => { history = clone(newerSnapshot); },
  setCapturePaused: (paused) => { captureState = { ...captureState, paused }; },
  guideHealth: () => clone(guideHealth),
  setGuideHealth: (health) => { guideHealth = { ...guideHealth, ...clone(health) }; },
  panelState: () => clone(panelState),
  panelTransitions: () => clone(panelTransitions),
  useManualPanel: (value = true) => { manualPanel = value; },
  completePanelOpen: () => {
    if (!pendingPanelOpen) throw new Error("没有等待中的面板打开展示");
    const pending = pendingPanelOpen;
    pendingPanelOpen = null;
    pending.resolve();
  },
  hidePanel,
  pasteResult,
  setFailures: ({ history: historyCount = 0, settings: settingsCount = 0, capture = 0, copy = 0, openPanel = 0, settingsRead = 0 } = {}) => {
    historyFailures = historyCount;
    settingsFailures = settingsCount;
    captureFailures = capture;
    copyFailures = copy;
    openPanelFailures = openPanel;
    settingsReadFailures = settingsRead;
  },
  useManualSaves: (value = true) => { manualSaves = value; },
  useManualHistory: (value = true) => { manualHistory = value; },
  saveCalls: () => saves.map(({ id, update, status }) => ({ id, update: clone(update), status })),
  pendingSaves: () => saves.filter((entry) => entry.status === "pending").map((entry) => entry.id),
  resolveSave: (id) => settleSave(id),
  rejectSave: (id, reason = "模拟保存失败") => settleSave(id, reason),
  historyReadIds: () => historyReads.map((entry) => entry.id),
  resolveHistory,
  invokeCalls: (command) => clone(command ? invocations.filter((call) => call.command === command) : invocations),
  clearInvokeCalls: () => { invocations.length = 0; },
  targetedEvents: () => clone(targetedEvents),
  closeCalls: () => closeCalls,
  hitRegionCalls: () => clone(hitRegionCalls),
  activeListeners: () => [...listeners.entries()].map(([name, handlers]) => ({ name, count: handlers.size })),
};
