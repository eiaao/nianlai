// 普通真实牌 main 对照：expected 来自固定 v10 合并提交 499d29a，绝不从当前实现自录。
import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(import.meta.url);
const { compileModule } = require("svelte/compiler");
const ts = require("typescript");
const BASE = "499d29a";
const flush = () => new Promise((resolve) => setImmediate(resolve));

function source(path, baseline) {
  return baseline
    ? execFileSync("git", ["-C", root, "show", `${BASE}:${path}`], { encoding: "utf8" })
    : readFileSync(resolve(root, path), "utf8");
}
const IPC = `
export const calls = [];
const handlers = new Map();
const record = (command, args = {}) => { calls.push({ command, args: JSON.parse(JSON.stringify(args)) }); };
const real = (id, favorite = false) => ({ id, kind: "文本", preview: "真实 " + id, file_names: [], source: "Notepad.exe", copied_ms: 100 - id,
  copy_count: 1, partial: false, favorite, favorite_position: favorite ? 1 : null, source_name: "记事本", source_icon: null });
export async function getSettings() { record("get_settings"); return null; }
export async function getHistory() { record("get_history"); return null; }
export async function getFavorites() { record("get_favorites"); return []; }
export async function appVersion() { return "0.0.0-parity"; }
export async function frontendReady() { record("frontend_ready"); }
export async function reportPainted(session) { record("report_painted", { session }); }
export async function pasteItem(session, id, mode, selection) { record("paste_item", { session, id, mode, ...(selection ? { selection } : {}) }); }
export async function startQueue(session, ids, automatic, mode, selection) { record("start_queue", { session, ids, automatic, mode, ...(selection ? { selection } : {}) }); }
export async function setFavorite(id, favorite) { record("set_favorite", { id, favorite }); return real(id, favorite); }
export async function deleteItem(id) { record("delete_item", { id }); return true; }
export async function undoDelete() { record("undo_delete"); return null; }
export async function setPreviewKeys(open) { record("set_preview_keys", { open }); }
export async function hidePanel(session) { record("hide_panel", { session }); }
export async function startSearch(session) { record("start_search", { session }); }
export async function searchReady(session) { record("search_ready", { session }); }
export async function retryActivation(session) { record("retry_activation", { session }); }
export async function cancelQueue(session) { record("cancel_queue", { session }); }
export async function setHitRegions() {}
export async function searchHistory(query, requestId) { record("search_history", { query, requestId }); return { request_id: requestId, items: [] }; }
export async function getFilePaths() { return null; }
export async function getImage() { return null; }
export async function getFullText() { return null; }
export async function copySelection() {}
export async function openSettings() {}
export async function reportSearchInput() {}
const subscribe = (name) => async (handler) => { handlers.set(name, handler); return () => handlers.delete(name); };
export const onPanelShown = subscribe("shown");
export const onPanelInteraction = subscribe("interaction");
export const onForegroundRevision = subscribe("foreground");
export const onPanelKey = subscribe("key");
export const onHistoryChanged = subscribe("history");
export const onPanelMode = subscribe("mode");
export const onPanelHidden = subscribe("hidden");
export const onQueueChanged = subscribe("queue");
export const onPanelTiming = subscribe("timing");
export const onPanelError = subscribe("error");
export const onPasteResult = subscribe("paste");
export function emit(name, payload) { return handlers.get(name)?.(payload); }
`;

async function compilePanel(baseline) {
  const temporary = mkdtempSync(join(resolve(tmpdir()), baseline ? "nianlai-task13-base-" : "nianlai-task13-current-"));
  const ipcPath = join(temporary, "ipc.mjs");
  const cardsPath = join(temporary, "cards.mjs");
  const preferencesPath = join(temporary, "preferences.mjs");
  const guideCardsPath = join(temporary, "guideCards.mjs");
  const panelActionsPath = join(temporary, "panelActions.mjs");
  const platformPath = join(temporary, "platform.mjs");
  const statePath = join(temporary, "state.mjs");
  writeFileSync(ipcPath, IPC);
  const transpile = (text) => ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext, useDefineForClassFields: true } }).outputText;
  writeFileSync(cardsPath, transpile(source("src/lib/cards.ts", baseline)));
  writeFileSync(preferencesPath, transpile(source("src/lib/preferences.ts", baseline)));
  if (!baseline) writeFileSync(platformPath, transpile(source("src/lib/platform.ts", false)));
  if (!baseline) writeFileSync(guideCardsPath, transpile(source("src/lib/guideCards.ts", false)).replace('"./platform"', JSON.stringify(pathToFileURL(platformPath).href)));
  if (!baseline) writeFileSync(panelActionsPath, transpile(source("src/lib/panelActions.ts", false)));
  const stripped = transpile(source("src/lib/state.svelte.ts", baseline));
  const clientUrl = pathToFileURL(require.resolve("svelte/internal/client")).href;
  const imports = new Map([
    ["svelte/internal/client", clientUrl], ["svelte", clientUrl],
    ["./ipc", pathToFileURL(ipcPath).href], ["./preferences", pathToFileURL(preferencesPath).href],
    ["./cards", pathToFileURL(cardsPath).href], ["./guideCards", pathToFileURL(guideCardsPath).href],
    ["./panelActions", pathToFileURL(panelActionsPath).href],
    ["./platform", pathToFileURL(platformPath).href],
  ]);
  const compiled = compileModule(stripped, { filename: "state.svelte.js", generate: "client", dev: false }).js.code
    .replace(/(from\s+)(["'])(svelte\/internal\/client|svelte|\.\/ipc|\.\/preferences|\.\/cards|\.\/guideCards|\.\/panelActions|\.\/platform)\2/g,
      (_, prefix, _quote, specifier) => prefix + JSON.stringify(imports.get(specifier)));
  writeFileSync(statePath, compiled);
  const [{ PanelState }, ipc] = await Promise.all([import(pathToFileURL(statePath).href), import(pathToFileURL(ipcPath).href)]);
  return { PanelState, ipc, cleanup: () => rmSync(temporary, { recursive: true, force: true }) };
}

async function runScenario(baseline) {
  const { PanelState, ipc, cleanup } = await compilePanel(baseline);
  const frames = new Map();
  let frameId = 0;
  const originals = new Map();
  for (const [name, value] of Object.entries({
    requestAnimationFrame: (callback) => { const id = ++frameId; frames.set(id, callback); return id; },
    cancelAnimationFrame: (id) => frames.delete(id),
    document: { activeElement: null, hasFocus: () => true },
  })) {
    originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  }
  try {
    const panel = new PanelState();
    panel.input = { focus() { document.activeElement = this; }, blur() { document.activeElement = null; } };
    const disconnect = panel.connect();
    await flush(); await flush();
    let session = 70;
    const items = [101, 102].map((id) => ({ id, kind: "文本", preview: `真实 ${id}`, file_names: [], source: "Notepad.exe", copied_ms: 200 - id,
      copy_count: 1, partial: false, favorite: false, favorite_position: null, source_name: "记事本", source_icon: null }));
    const show = () => {
      session += 1;
      ipc.emit("shown", { session, interaction: "quick", foregroundRevision: 1, hotkeyToShowMs: 1,
        history: { items, total: 2, recording: true, last_recorded_ms: 99, store_status: null },
        // 当前实现消费这个兼容可选字段；baseline 会自然忽略它。cards_dealt=false 是普通路径对照条件。
        guide: { version: 1, phase: "completed", baseline_ms: null, baseline_total: 0, sample_id: null, cards_dealt: false, dismissed: [] } });
    };
    const runFrames = async () => { for (const callback of [...frames.values()]) callback(1000); frames.clear(); await flush(); };
    const pasteDone = (result = "sent") => {
      ipc.emit("paste", { session, paste_result: result, reason: null, actual_key: null, layer2: null,
        layer3: { conclusion: "confirmed" }, as_text: null, selected_to_result_ms: 1 });
    };
    show(); await flush(); await runFrames(); await runFrames();
    panel.navigate("1"); await flush(); pasteDone(); await flush();
    panel.navigate("Shift+Enter"); await flush(); pasteDone("copied"); await flush();
    panel.navigate("Ctrl+Enter"); await flush(); pasteDone(); await flush();
    await panel.chooseByMouse(0, { button: 0, type: "click", isTrusted: true, detail: 1, timeStamp: 1000 }); await flush(); pasteDone(); await flush();
    panel.navigate("Ctrl+D"); await flush();
    panel.navigate("Space");
    panel.navigate("ArrowRight");
    panel.navigate("Space");
    panel.navigate("Enter"); await flush();
    ipc.emit("queue", { session, remaining: 0, total: 2, automatic: true, busy: false, stopped: "done" }); await flush();
    ipc.emit("hidden", { session }); show(); await flush();
    panel.navigate("Home");
    panel.navigate("Delete:down"); await flush();
    panel.navigate("Delete");
    panel.navigate("Tab"); await flush();
    panel.navigate("Tab"); await flush();
    disconnect();
    return ipc.calls;
  } finally {
    for (const [name, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name];
    }
    cleanup();
  }
}

test("cards_dealt=false 的真实牌 IPC 与 499d29a 逐命令、参数、顺序一致", async () => {
  const expected = await runScenario(true);
  const actual = await runScenario(false);
  assert.ok(expected.length >= 10, `基线必须实际执行完整交互，而非空序列：${JSON.stringify(expected)}`);
  for (const command of ["paste_item", "set_favorite", "start_queue", "delete_item", "set_preview_keys"]) {
    assert.ok(expected.some((call) => call.command === command), `基线缺少 ${command}：${JSON.stringify(expected)}`);
  }
  assert.ok(expected.filter((call) => call.command === "paste_item").length >= 4, JSON.stringify(expected));
  assert.deepEqual(actual, expected);
  assert.equal(actual.filter((call) => call.command === "get_autostart").length, 0);
  console.log(`main 对照通过：${BASE} 与当前均 ${expected.length} 次命令；粘贴/复制/纯文本/鼠标/常用/多选/队列/删除/预览顺序一致`);
});
