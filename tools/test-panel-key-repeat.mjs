// 仓库根目录运行：node tools/test-panel-key-repeat.mjs
// 编译真实 PanelState；仅 IPC 与键盘事件对象为内存替身，不调用 Tauri、剪贴板或系统输入。
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, mkdtempSync, readFileSync, rmdirSync, unlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(import.meta.url);
const { compileModule } = require("svelte/compiler");
const ts = require("typescript");

const transpile = (path) => ts.transpileModule(readFileSync(join(root, path), "utf8"), {
  compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext },
}).outputText;

async function compilePanel() {
  const temporaryBase = resolve(tmpdir());
  const temporary = mkdtempSync(join(temporaryBase, "nianlai-panel-key-repeat-"));
  const ipcPath = join(temporary, "ipc.mjs");
  const compiledPath = join(temporary, "state.svelte.mjs");
  const preferencesPath = join(temporary, "preferences.mjs");
  const cardsPath = join(temporary, "cards.mjs");
  const cleanup = () => {
    assert.equal(dirname(temporary), temporaryBase);
    assert.ok(basename(temporary).startsWith("nianlai-panel-key-repeat-"));
    // 只删除本次创建的几个文件和空目录，不递归清理任何路径。
    for (const path of [compiledPath, preferencesPath, cardsPath, ipcPath]) if (existsSync(path)) unlinkSync(path);
    rmdirSync(temporary);
    assert.equal(existsSync(temporary), false);
  };
  try {
    writeFileSync(ipcPath, `
export const calls = [];
let rejectNext = false;
let rejectSearch = false;
let pendingPaste = null;
let rejectQueue = false;
const handlers = new Map();
export const previewKeys = [];
export async function setPreviewKeys(open) { previewKeys.push(open); }
export async function copySelection(id, text) { calls.push({ command: "copySelection", id, text }); }
export function reset() { calls.length = 0; previewKeys.length = 0; rejectNext = false; rejectSearch = false; rejectQueue = false; pendingPaste = null; handlers.clear();
  store.history = null; store.favorites = []; store.deletable = true; store.undoable = null; }
export function rejectNextPaste() { rejectNext = true; }
export function rejectNextQueue(reason = true) { rejectQueue = reason; }
export function rejectNextSearch() { rejectSearch = true; }
export function deferNextPaste() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  pendingPaste = promise;
  return { resolve, reject };
}
export async function pasteItem(session, id, mode, selection) {
  calls.push({ command: "pasteItem", session, id, mode, ...(selection ? { selection } : {}) });
  if (rejectNext) { rejectNext = false; throw new Error("模拟 IPC 失败"); }
  const pending = pendingPaste;
  pendingPaste = null;
  if (pending) await pending;
}
export async function hidePanel(session) { calls.push({ command: "hidePanel", session }); }
export async function startQueue(session, ids, automatic, mode, selection) {
  calls.push({ command: "startQueue", session, ids, automatic, mode, ...(selection ? { selection } : {}) });
  if (rejectQueue) { const reason = rejectQueue; rejectQueue = false; throw typeof reason === "string" ? reason : new Error("模拟队列 IPC 失败"); }
}
export async function cancelQueue(session) { calls.push({ command: "cancelQueue", session }); }
export async function setHitRegions() {}
export async function retryActivation(session) { calls.push({ command: "retryActivation", session }); }
export async function startSearch(session) {
  calls.push({ command: "startSearch", session });
  if (rejectSearch) { rejectSearch = false; throw new Error("模拟搜索切换失败"); }
}
// 交互优化（收藏、删除、撤销）用的可控替身；默认 getHistory 返回 null，启动预热直接跳过，不影响旧用例。
export const store = { history: null, favorites: [], deletable: true, undoable: null };
export async function getHistory() { return store.history; }
export async function getFavorites() { return store.favorites; }
export async function setFavorite(id, favorite) {
  calls.push({ command: "setFavorite", id, favorite });
  return { id, kind: "文本", preview: "合成条目", file_names: [], source: "合成来源", copied_ms: 0, copy_count: 1, partial: false,
    favorite, favorite_position: favorite ? 9 : null };
}
export async function deleteItem(id) { calls.push({ command: "deleteItem", id }); return store.deletable; }
export async function undoDelete() {
  calls.push({ command: "undoDelete" });
  return store.undoable ? { ...(store.history?.items.find((item) => item.id === store.undoable) ?? {}), id: store.undoable, favorite: false } : null;
}
// 面板里的其余 IPC 调用不再用 ?. 兜底，替身补全为无副作用的空实现。
export async function getSettings() { return null; }
export async function openSettings() {}
export async function getFilePaths() { return null; }
export async function getImage() { return null; }
export async function getFullText() { return null; }
export async function frontendReady() {}
export async function reportPainted() {}
export async function searchReady() {}
export async function appVersion() { return "0.0.0-test"; }
const subscribe = (name) => async (handler) => { handlers.set(name, handler); return () => handlers.delete(name); };
export const onPanelShown = subscribe("shown");
export const onPanelInteraction = subscribe("interaction");
export const onForegroundRevision = subscribe("foreground-revision");
export const onPanelKey = subscribe("key");
export const onHistoryChanged = subscribe("history-changed");
export const onPanelMode = subscribe("mode");
export const onPanelHidden = subscribe("hidden");
export const onQueueChanged = subscribe("queue");
export const onPanelTiming = subscribe("timing");
export const onPanelError = subscribe("error");
export const onPasteResult = subscribe("paste-result");
export function emit(name, payload) { return handlers.get(name)?.(payload); }
`);
    const source = readFileSync(join(root, "src/lib/state.svelte.ts"), "utf8");
    const stripped = ts.transpileModule(source, {
      compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext, useDefineForClassFields: true },
    }).outputText;
    // Node 的 svelte 入口默认走服务端；这里的 tick/untrack 使用浏览器入口同一份客户端实现。
    const clientUrl = pathToFileURL(require.resolve("svelte/internal/client")).href;
    const imports = new Map([
      ["svelte/internal/client", clientUrl],
      ["svelte", clientUrl],
      ["./ipc", pathToFileURL(ipcPath).href],
      ["./preferences", pathToFileURL(preferencesPath).href],
      ["./cards", pathToFileURL(cardsPath).href],
    ]);
    writeFileSync(cardsPath, transpile("src/lib/cards.ts"));
    // 真实的 preferences.ts 一并转译，面板与设置页用同一份偏好映射。
    writeFileSync(preferencesPath, ts.transpileModule(readFileSync(join(root, "src/lib/preferences.ts"), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext },
    }).outputText);
    const compiled = compileModule(stripped, { filename: "state.svelte.js", generate: "client", dev: false }).js.code
      .replace(/(from\s+)(["'])(svelte\/internal\/client|svelte|\.\/ipc|\.\/preferences|\.\/cards)\2/g,
        (_, prefix, _quote, specifier) => prefix + JSON.stringify(imports.get(specifier)));
    writeFileSync(compiledPath, compiled);
    const { PanelState } = await import(pathToFileURL(compiledPath).href);
    const ipc = await import(pathToFileURL(ipcPath).href);
    const cards = await import(pathToFileURL(cardsPath).href);
    return { PanelState, ipc, cards, cleanup };
  } catch (error) {
    cleanup();
    throw error;
  }
}

const submissions = [
  { name: "Enter", key: "Enter", mode: "paste", index: 1 },
  { name: "Shift+Enter", key: "Enter", shiftKey: true, mode: "copy", index: 1 },
  { name: "Ctrl+Enter", key: "Enter", ctrlKey: true, mode: "text", index: 1 },
  ...Array.from({ length: 9 }, (_, index) => ({ name: `Ctrl+${index + 1}`, key: String(index + 1), ctrlKey: true, mode: "paste", index })),
];

function keyboard(panel, options, type) {
  const event = {
    type, isTrusted: true, timeStamp: performance.now(),
    key: options.key, keyCode: options.key === "Enter" ? 13 : 0,
    code: /^[1-9]$/.test(options.key) ? `Digit${options.key}` : options.key, location: 0,
    ctrlKey: false, altKey: false, metaKey: false, shiftKey: false,
    repeat: false, isComposing: false, target: panel.input, defaultPrevented: false,
    ...options,
    preventDefault() { this.defaultPrevented = true; },
  };
  panel[type](event);
  return event;
}
const keydown = (panel, options) => keyboard(panel, options, "keydown");
const keyup = (panel, options) => keyboard(panel, options, "keyup");

function click(options = {}) {
  return { type: "click", isTrusted: true, timeStamp: performance.now(), button: 0, detail: 1, ...options };
}

const items = () => Array.from({ length: 9 }, (_, index) => ({
  id: 101 + index, kind: "文本", preview: "合成条目", file_names: [], source: "合成来源",
  copied_ms: 0, copy_count: 1, partial: false,
}));
const shown = (options = {}) => ({
  session: 37, interaction: "quick", foregroundRevision: 3, hotkeyToShowMs: 1,
  history: { items: items(), total: 9, recording: true, last_recorded_ms: null, store_status: null }, ...options,
});

test("通用终止反馈只恢复当前隐藏会话的关闭提示", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);

  for (const message of ["剪贴板已变化，本次未粘贴，请关闭面板后重新呼出", "粘贴结果未确认，请先检查目标，避免重复粘贴"]) {
    await suite.test(message, async (context) => {
      const { panel, focusCalls } = await createPanel(context, PanelState, ipc);
      const beforeFocus = focusCalls();
      ipc.emit("hidden", { session: 37 });
      ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
      ipc.emit("error", { session: 37, close_only: true, message });
      assert.equal(panel.visible, true);
      assert.equal(panel.session, 37);
      assert.equal(panel.interaction, "close_only");
      assert.equal(panel.message, message);
      assert.equal(panel.filtered.length, 0);
      assert.equal(panel.locked, true);
      assert.equal(focusCalls(), beforeFocus);
      keydown(panel, { key: "Enter" });
      keyup(panel, { key: "Enter" });
      await panel.choose(1, "paste", click());
      panel.searchPointer({ preventDefault() {} });
      ipc.emit("interaction", { session: 37, interaction: "activated" });
      ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
      assert.equal(panel.interaction, "close_only");
      assert.equal(panel.message, message);
      assert.deepEqual(ipc.calls, []);
      panel.close();
      assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
      ipc.emit("hidden", { session: 37 });
      ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
      assert.equal(panel.visible, false, "已关闭的终止提示不能被同会话重复事件重新打开");
      ipc.emit("shown", shown({ session: 38 }));
      assert.equal(panel.visible, true);
      assert.equal(panel.interaction, "quick");
      assert.equal(panel.session, 38);
    });
  }

  await suite.test("可见会话、未标记显示和非终止交互不能使用受限恢复", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.message = "当前选择";
    ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
    assert.equal(panel.interaction, "quick");
    assert.equal(panel.message, "当前选择");
    assert.equal(panel.selected, 1);
    ipc.emit("hidden", { session: 37 });
    for (const options of [
      { interaction: "close_only" },
      { terminal_feedback: true, interaction: "quick" },
      { terminal_feedback: true, interaction: "close_only", session: 36 },
      { terminal_feedback: true, interaction: "close_only", session: 38 },
    ]) ipc.emit("shown", shown(options));
    assert.equal(panel.visible, false);
    assert.equal(panel.session, 37);
  });

  await suite.test("新会话和取消后的隐藏状态不接受迟到旧反馈", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.emit("hidden", { session: 37 });
    ipc.emit("error", { session: 37, close_only: true, message: "取消后的错误" });
    assert.equal(panel.visible, false);
    ipc.emit("shown", shown({ session: 38 }));
    panel.message = "新会话";
    ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
    ipc.emit("error", { session: 37, close_only: true, message: "旧反馈" });
    assert.equal(panel.visible, true);
    assert.equal(panel.session, 38);
    assert.equal(panel.interaction, "quick");
    assert.equal(panel.message, "新会话");
    ipc.emit("hidden", { session: 38 });
    ipc.emit("shown", shown({ interaction: "close_only", terminal_feedback: true }));
    assert.equal(panel.visible, false);
  });
});

test("Activated DOM 快速取用、焦点准备与自然搜索输入", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const actions = submissions.filter((action) => action.key === "Enter" || Number(action.key) <= 7).map((action) => action.name.startsWith("Ctrl+") && action.key !== "Enter"
    ? { ...action, name: action.key, ctrlKey: false } : action);
  const expected = (action) => ({ command: "pasteItem", session: 37, id: 101 + action.index, mode: action.mode });

  for (const action of actions) {
    await suite.test(`${action.name} 固定首次按下的条目和模式，等确认键及修饰键真实松开`, async (context) => {
      const { panel } = await activatedPanel(context, PanelState, ipc);
      const event = keydown(panel, action);
      assert.equal(event.defaultPrevented, true);
      assert.deepEqual(ipc.calls, []);
      keyup(panel, action);
      if (action.ctrlKey || action.shiftKey) {
        assert.deepEqual(ipc.calls, [], "Enter 已松开但修饰键仍下按时不能归还前台");
        keyup(panel, { key: action.ctrlKey ? "Control" : "Shift", code: action.ctrlKey ? "ControlLeft" : "ShiftLeft" });
      }
      assert.deepEqual(ipc.calls, [expected(action)]);
    });
  }

  await suite.test("重复按下不替换选择，只有重复事件的键不能单独提交", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Enter", repeat: true });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Enter" });
    panel.selected = 6;
    keydown(panel, { key: "Enter", repeat: true });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: "paste" }]);
  });

  await suite.test("不可信事件、无按下的抬起及不同 code 不能提交", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keyup(panel, { key: "Enter" });
    keydown(panel, { key: "Enter", isTrusted: false });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter", code: "NumpadEnter" });
    keyup(panel, { key: "Enter", isTrusted: false });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: "paste" }]);
  });

  await suite.test("按下后列表变化仍绑定原条目，条目移除时取消", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Enter" });
    panel.history = { ...panel.history, items: items().reverse() };
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: "paste" }]);
    panel.busy = false;
    keydown(panel, { key: "1" });
    panel.history = { ...panel.history, items: [] };
    keyup(panel, { key: "1" });
    assert.equal(ipc.calls.length, 1);
  });

  await suite.test("方向和翻页键允许重复导航，确认还要等待其它已观察键抬起", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "ArrowRight" });
    keydown(panel, { key: "ArrowRight", repeat: true });
    assert.equal(panel.selected, 3);
    keydown(panel, { key: "End" });
    keyup(panel, { key: "End" });
    assert.equal(panel.selected, 6, "扇面里 End 停在第 7 张，不越出扇面");
    keydown(panel, { key: "Home" });
    keyup(panel, { key: "Home" });
    keydown(panel, { key: "ArrowRight" });
    assert.equal(panel.selected, 1);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "ArrowRight" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: "paste" }]);
  });

  await suite.test("Esc 只在匹配可信松键后收起", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  for (const boundary of ["inputBlur", "windowBlur", "hidden", "session", "mode"]) {
    await suite.test(`${boundary} 清除待松键选择，迟到 keyup 不能提交`, async (context) => {
      const { panel, runFrame } = await activatedPanel(context, PanelState, ipc);
      keydown(panel, { key: "Enter" });
      if (boundary === "inputBlur" || boundary === "windowBlur") {
        document.activeElement = null;
        panel[boundary]();
      } else if (boundary === "hidden") ipc.emit("hidden", { session: 37 });
      else if (boundary === "session") {
        ipc.emit("shown", shown({ session: 38, interaction: "activated" }));
        await flush(); await runFrame(); await runFrame();
      } else {
        const switching = ipc.emit("mode", { session: 37, mode: "search" });
        await flush(); await runFrame(); await runFrame(); await switching;
      }
      keyup(panel, { key: "Enter" });
      assert.deepEqual(ipc.calls, []);
    });
  }

  await suite.test("焦点准备失败可见、可本地鼠标重试，不误调后端 retry_activation", async (context) => {
    const { panel, runFrame, allowFocus } = await activatedPanel(context, PanelState, ipc, { focusRefused: true });
    assert.equal(panel.domFocus, "failed");
    assert.equal(panel.interactive, false);
    assert.equal(panel.inputSurface, true, "准备期间必须能先挂载真实输入框");
    assert.equal(panel.filtered.length, 0);
    allowFocus();
    panel.retryDomFocus(click({ isTrusted: false }));
    assert.equal(panel.domFocus, "failed");
    panel.retryDomFocus(click());
    await flush(); await runFrame(); await runFrame();
    assert.equal(panel.domFocus, "ready");
    assert.equal(document.activeElement, panel.input);
    assert.equal(panel.locked, false);
    assert.equal(panel.filtered.length, 9);
    assert.deepEqual(ipc.calls, []);
  });

  await suite.test("文档失焦时即使 activeElement 为 input 也不能报本地就绪", async (context) => {
    const harness = await createPanel(context, PanelState, ipc, { interaction: "activation_pending" });
    document.hasFocus = () => false;
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    await flush(); await harness.runFrame(); await harness.runFrame();
    assert.equal(document.activeElement, harness.panel.input);
    assert.equal(harness.panel.domFocus, "failed");
    assert.equal(harness.panel.interactive, false);
  });

  await suite.test("没有动画帧回调时焦点准备超时失败，不无限隐藏列表", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await createPanel(context, PanelState, ipc, { interaction: "activation_pending" });
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    await flush();
    context.mock.timers.tick(501);
    assert.equal(panel.domFocus, "failed");
    assert.equal(panel.interactive, false);
  });

  await suite.test("普通首字由 beforeinput 自然承载，切换中数字不误取用且不重放、不重聚焦", async (context) => {
    const { panel, runFrame, focusCalls } = await activatedPanel(context, PanelState, ipc);
    const callsBefore = focusCalls();
    assert.equal(keydown(panel, { key: "a", code: "KeyA" }).defaultPrevented, false);
    const event = beforeInput(panel);
    assert.equal(event.defaultPrevented, false);
    panel.query = "合成首字";
    assert.equal(panel.searchStarting, true);
    assert.equal(panel.interactive, false);
    keydown(panel, { key: "1" });
    keyup(panel, { key: "1" });
    beforeInput(panel);
    assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
    const switching = ipc.emit("mode", { session: 37, mode: "search" });
    await flush(); await runFrame(); await runFrame(); await switching;
    assert.equal(panel.query, "合成首字");
    assert.equal(panel.searchStarting, false);
    assert.equal(panel.mode, "search");
    assert.equal(focusCalls(), callsBefore, "已聚焦的同一输入框不能在搜索切换中再聚焦");
    assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
  });

  await suite.test("IME compositionstart 先进入搜索，同一输入框组合不被切换打断", async (context) => {
    const { panel, runFrame, advance, focusCalls } = await activatedPanel(context, PanelState, ipc);
    const callsBefore = focusCalls();
    panel.startComposition({ type: "compositionstart", isTrusted: true, target: panel.input, timeStamp: performance.now() });
    beforeInput(panel, { inputType: "insertCompositionText", isComposing: true });
    panel.query = "合成中文";
    const switching = ipc.emit("mode", { session: 37, mode: "search" });
    await flush(); await runFrame(); await runFrame(); await switching;
    assert.equal(panel.composing, true);
    assert.equal(panel.query, "合成中文");
    assert.equal(focusCalls(), callsBefore);
    keydown(panel, { key: "Enter", isComposing: true });
    assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
    panel.endComposition();
    advance(50);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    assert.equal(ipc.calls.at(-1).command, "pasteItem");
    assert.deepEqual(ipc.calls.at(-1).selection, { revision: 3, nonce: 1 });
  });

  for (const action of [{ key: "/", code: "Slash" }, { key: "f", code: "KeyF", ctrlKey: true }]) {
    await suite.test(`${action.key === "/" ? "/" : "Ctrl+F"} 阻止默认输入并保留空查询搜索入口`, async (context) => {
      const { panel } = await activatedPanel(context, PanelState, ipc);
      const event = keydown(panel, action);
      assert.equal(event.defaultPrevented, true);
      assert.equal(panel.query, "");
      assert.equal(panel.searchStarting, true);
      assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
    });
  }

  await suite.test("搜索切换 IPC 失败或超时可见，不回退 Quick 误选", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    ipc.rejectNextSearch();
    keydown(panel, { key: "/", code: "Slash" });
    await flush();
    assert.equal(panel.searchStarting, false);
    assert.equal(panel.searchStartFailed, true);
    keydown(panel, { key: "1" });
    keyup(panel, { key: "1" });
    assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
    panel.close();
    assert.equal(ipc.calls.at(-1).command, "hidePanel");
  });

  await suite.test("普通 Quick 仍只由 panel:key 处理，DOM 字符不替它触发搜索", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    keydown(panel, { key: "1" });
    keyup(panel, { key: "1" });
    beforeInput(panel);
    assert.deepEqual(ipc.calls, []);
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste" }]);
  });

  for (const action of [{ key: "Enter", ctrlKey: true, mode: "text" }, { key: "Enter", shiftKey: true, mode: "copy" }]) {
    await suite.test(`Activated Search ${action.mode} 保留按下时快照，先松 Enter 再松修饰键才提交`, async (context) => {
      const { panel } = await activatedSearch(context, PanelState, ipc);
      keydown(panel, action);
      assert.deepEqual(ipc.calls, []);
      keyup(panel, action);
      panel.searchItems = items().reverse();
      panel.selected = 6;
      assert.deepEqual(ipc.calls, []);
      keyup(panel, { key: action.ctrlKey ? "Control" : "Shift", code: action.ctrlKey ? "ControlLeft" : "ShiftLeft" });
      assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: action.mode, selection: { revision: 3, nonce: 1 } }]);
    });
  }

  await suite.test("Activated Search 松键期间 revision 变化取消，下一新选择用递增 nonce", async (context) => {
    const { panel, advance } = await activatedSearch(context, PanelState, ipc);
    keydown(panel, { key: "Enter" });
    advance(1);
    ipc.emit("foreground-revision", { session: 37, revision: 4 });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, []);
    assert.equal(panel.busy, false);
    assert.match(panel.message, /重新选择/);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 102, mode: "paste", selection: { revision: 4, nonce: 2 } }]);
  });

  await suite.test("Activated Search 方向长按加 Ctrl+数字等所有已观察键抬起", async (context) => {
    const { panel } = await activatedSearch(context, PanelState, ipc);
    keydown(panel, { key: "ArrowDown" });
    keydown(panel, { key: "ArrowDown", repeat: true });
    keydown(panel, { key: "1", ctrlKey: true });
    keyup(panel, { key: "1", ctrlKey: true });
    keyup(panel, { key: "Control", code: "ControlLeft" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "ArrowDown" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste", selection: { revision: 3, nonce: 1 } }]);
  });

  await suite.test("Activated 鼠标选择同样等待已观察修饰键，不凭 click 跳过配对", async (context) => {
    const { panel } = await activatedSearch(context, PanelState, ipc);
    await panel.choose(0, "copy", click({ ctrlKey: true }));
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Control", code: "ControlLeft" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "copy", selection: { revision: 3, nonce: 1 } }]);
  });

  await suite.test("Activated 鼠标等待松键时不刷新 revision，前台变化后取消", async (context) => {
    const { panel, advance } = await activatedSearch(context, PanelState, ipc);
    keydown(panel, { key: "ArrowDown" });
    await panel.choose(0, "paste", click());
    advance(1);
    ipc.emit("foreground-revision", { session: 37, revision: 4 });
    keyup(panel, { key: "ArrowDown" });
    assert.deepEqual(ipc.calls, []);
    await panel.choose(0, "paste", click());
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste", selection: { revision: 4, nonce: 2 } }]);
  });

  await suite.test("搜索切换没有 mode 回执会超时失败，不能永久停留准备态", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    context.mock.timers.enable({ apis: ["setTimeout"] });
    keydown(panel, { key: "/", code: "Slash" });
    context.mock.timers.tick(1001);
    assert.equal(panel.searchStarting, false);
    assert.equal(panel.searchStartFailed, true);
    assert.equal(panel.interactive, false);
    assert.match(panel.message, /重新呼出/);
  });

  await suite.test("Esc 撤销等待修饰键的选择，全部真实松键后只关闭", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Enter", ctrlKey: true });
    keyup(panel, { key: "Enter", ctrlKey: true });
    keydown(panel, { key: "Escape", ctrlKey: true });
    keyup(panel, { key: "Escape", ctrlKey: true });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Control", code: "ControlLeft" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  await suite.test("已经提交且 busy 时新 Esc 仍能在匹配松键后请求关闭", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    assert.equal(panel.busy, true);
    keydown(panel, { key: "Escape" });
    assert.equal(ipc.calls.length, 1);
    keyup(panel, { key: "Escape" });
    assert.equal(ipc.calls.length, 2);
    assert.equal(ipc.calls[1].command, "hidePanel");
  });

  await suite.test("重复或组合中的 Esc 不能产生关闭请求", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "Escape", repeat: true });
    keyup(panel, { key: "Escape" });
    panel.composing = true;
    keydown(panel, { key: "Escape", isComposing: true });
    keyup(panel, { key: "Escape", isComposing: true });
    assert.deepEqual(ipc.calls, []);
  });

  await suite.test("迟到的其它键或修饰键 keyup 不清除当前待选择的 held 证据", async (context) => {
    const { panel, advance } = await activatedPanel(context, PanelState, ipc);
    keydown(panel, { key: "ArrowRight" });
    advance(1);
    keydown(panel, { key: "Enter", ctrlKey: true });
    keyup(panel, { key: "Enter", ctrlKey: true });
    keyup(panel, { key: "Control", code: "ControlLeft", timeStamp: 1000 });
    keyup(panel, { key: "ArrowRight", timeStamp: 1000 });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Control", code: "ControlLeft" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "ArrowRight" });
    assert.equal(ipc.calls.length, 1);
    assert.equal(ipc.calls[0].id, 103);
  });
});
const pasteResult = (options = {}) => ({
  session: 37, paste_result: "copied_only", reason: "target_may_be_elevated", actual_key: null,
  layer2: null, layer3: { conclusion: "unknown" }, as_text: null, selected_to_result_ms: 1, ...options,
});
const flush = () => new Promise((resolve) => setImmediate(resolve));

async function createPanel(context, PanelState, ipc, options = {}) {
  let now = 1000;
  context.mock.method(performance, "now", () => now);
  const frames = new Map();
  let frameId = 0;
  const restoreGlobals = [];
  for (const [name, value] of Object.entries({
    requestAnimationFrame: (callback) => { const id = ++frameId; frames.set(id, callback); return id; },
    cancelAnimationFrame: (id) => frames.delete(id),
    document: { activeElement: null, hasFocus: () => true },
  })) {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
    restoreGlobals.push(() => { if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name]; });
  }
  ipc.reset();
  const panel = new PanelState();
  let focusCalls = 0;
  let focusRefused = options.focusRefused === true;
  panel.input = { focus() { focusCalls += 1; if (!focusRefused) document.activeElement = this; }, blur() { if (document.activeElement === this) document.activeElement = null; } };
  const disconnect = panel.connect();
  context.after(() => { disconnect(); restoreGlobals.forEach((restore) => restore()); });
  await flush();
  ipc.emit("shown", shown(options));
  // 普通 quick 的夹具代表“搜索中”：搜索词非空，结果就是 searchItems（总是摊开）。
  if (!options.interaction || options.interaction === "quick") { panel.mode = "search"; panel.query = "合成"; }
  panel.searchItems = items();
  panel.selected = 1;
  assert.equal(panel.busy, false);
  return {
    panel, advance: (milliseconds) => { now += milliseconds; },
    focusCalls: () => focusCalls, allowFocus: () => { focusRefused = false; },
    runFrame: async () => {
      const current = [...frames];
      frames.clear();
      current.forEach(([, callback]) => callback(now));
      await flush();
    },
  };
}

async function activatedPanel(context, PanelState, ipc, options = {}) {
  const harness = await createPanel(context, PanelState, ipc, { ...options, interaction: "activation_pending" });
  ipc.emit("interaction", { session: 37, interaction: "activated" });
  await flush();
  await harness.runFrame();
  await harness.runFrame();
  return harness;
}

async function activatedSearch(context, PanelState, ipc) {
  const harness = await activatedPanel(context, PanelState, ipc);
  harness.panel.searchPointer({ type: "pointerdown", isTrusted: true, target: harness.panel.input, timeStamp: performance.now(), preventDefault() {} });
  const switching = ipc.emit("mode", { session: 37, mode: "search" });
  await flush(); await harness.runFrame(); await harness.runFrame(); await switching;
  ipc.calls.length = 0;
  return harness;
}

function beforeInput(panel, options = {}) {
  const event = {
    type: "beforeinput", isTrusted: true, timeStamp: performance.now(), target: panel.input,
    inputType: "insertText", isComposing: false, defaultPrevented: false, ...options,
    preventDefault() { this.defaultPrevented = true; },
  };
  panel.beforeInput(event);
  return event;
}

test("原生协调失败结束旧取用会话并保留关闭入口", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const message = "剪贴板忙，本次未提交，请关闭面板后重新呼出";

  await suite.test("busy不隐藏，终态拒绝全部迟到重开和取用，只留鼠标关闭", async (context) => {
    const { panel } = await activatedSearch(context, PanelState, ipc);
    keydown(panel, { key: "Enter" }); keyup(panel, { key: "Enter" });
    assert.equal(panel.busy, true);
    assert.equal(ipc.calls.filter((call) => call.command === "pasteItem").length, 1);
    ipc.emit("interaction", { session: 37, interaction: "close_only" });
    ipc.emit("error", { session: 37, message, close_only: true });
    assert.equal(panel.visible, true);
    assert.equal(panel.interaction, "close_only");
    assert.equal(panel.inputSurface, false);
    assert.equal(panel.interactive, false);
    assert.equal(panel.filtered.length, 0);
    assert.equal(panel.locked, true);
    assert.equal(panel.message, message);
    assert.equal(ipc.calls.some((call) => call.command === "hidePanel"), false);
    ipc.calls.length = 0;
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    await ipc.emit("mode", { session: 37, mode: "search" });
    ipc.emit("error", { session: 37, message: "迟到错误" });
    ipc.emit("history-changed", { session: 37 }); await flush();
    ipc.emit("paste-result", pasteResult());
    keydown(panel, { key: "Enter" }); keyup(panel, { key: "Enter" });
    await panel.choose(0, "paste", click());
    panel.searchPointer({ preventDefault() {} });
    assert.equal(panel.interaction, "close_only");
    assert.equal(panel.message, message);
    assert.deepEqual(ipc.calls, []);
    panel.close(click());
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  await suite.test("只接受可信新Esc配对关闭，失焦与重复事件不能关闭", async (context) => {
    const { panel } = await activatedSearch(context, PanelState, ipc);
    ipc.emit("error", { session: 37, message, close_only: true });
    keyup(panel, { key: "Escape" });
    keydown(panel, { key: "Escape", isTrusted: false }); keyup(panel, { key: "Escape" });
    keydown(panel, { key: "Escape", repeat: true }); keyup(panel, { key: "Escape" });
    keydown(panel, { key: "Escape", isComposing: true }); keyup(panel, { key: "Escape" });
    keydown(panel, { key: "Escape" }); panel.windowBlur(); keyup(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  await suite.test("等待中Esc仍请求关闭，取消不显示busy且旧结果不影响新呼出", async (context) => {
    const { panel } = await activatedSearch(context, PanelState, ipc);
    const pending = ipc.deferNextPaste();
    keydown(panel, { key: "Enter" }); keyup(panel, { key: "Enter" });
    keydown(panel, { key: "Escape" }); keyup(panel, { key: "Escape" });
    assert.equal(ipc.calls.filter((call) => call.command === "hidePanel").length, 1);
    ipc.emit("interaction", { session: 37, interaction: "passive" });
    ipc.emit("hidden", { session: 37 });
    pending.resolve(); await flush();
    assert.equal(panel.visible, false);
    assert.doesNotMatch(panel.message, /剪贴板忙/);
    ipc.emit("shown", shown({ session: 38 }));
    ipc.emit("error", { session: 37, message, close_only: true });
    ipc.emit("hidden", { session: 37 });
    assert.equal(panel.visible, true);
    assert.equal(panel.interaction, "quick");
    assert.equal(panel.busy, false);
    assert.equal(panel.message, "");
  });
});

test("搜索提交键的首次按下、自动重复和组合输入保护", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const expectedPaste = (action, nonce = 1) => ({ command: "pasteItem", session: 37, id: 101 + action.index, mode: action.mode, selection: { revision: 3, nonce } });

  for (const action of submissions) {
    await suite.test(`${action.name} 新按下保留原提交模式`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      const event = keydown(panel, action);
      assert.equal(event.defaultPrevented, true);
      if (Number(action.key) > 7) {
        assert.deepEqual(ipc.calls, [], "Ctrl+8/9 只拦截，不取用任何牌");
        assert.equal(panel.busy, false);
        return;
      }
      assert.deepEqual(ipc.calls, [expectedPaste(action)]);
    });
    await suite.test(`${action.name} 自动重复不得提交，即使 busy=false`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      const event = keydown(panel, { ...action, repeat: true });
      assert.equal(event.defaultPrevented, true);
      assert.deepEqual(ipc.calls, [], `${action.name} 的重复事件不得发出粘贴或复制 IPC`);
      assert.equal(panel.busy, false);
      assert.equal(panel.selected, 1);
    });
  }

  await suite.test("组合 Enter 上屏超过 50ms 后，重复仍不提交，新的 Enter 正常提交", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    panel.startComposition();
    keydown(panel, { key: "Enter", isComposing: true });
    assert.deepEqual(ipc.calls, []);
    panel.endComposition();
    advance(51);
    keydown(panel, { key: "Enter", repeat: true });
    assert.deepEqual(ipc.calls, [], "长按上屏 Enter 的自动重复不能成为新提交");
    assert.equal(panel.busy, false);
    keydown(panel, { key: "Enter", repeat: false });
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])]);
  });

  await suite.test("IPC 失败解除 busy 后，自动重复不能重试，新的 Enter 仍可重试", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.rejectNextPaste();
    keydown(panel, { key: "Enter" });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(panel.busy, false);
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])]);
    keydown(panel, { key: "Enter", repeat: true });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])], "失败不能把长按重复转换成重试授权");
    keydown(panel, { key: "Enter", repeat: false });
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0]), expectedPaste(submissions[0], 2)]);
  });

  await suite.test("方向键长按继续导航，不提交", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    keydown(panel, { key: "ArrowDown" });
    keydown(panel, { key: "ArrowDown", repeat: true });
    keydown(panel, { key: "ArrowDown", repeat: true });
    assert.equal(panel.selected, 4);
    keydown(panel, { key: "ArrowUp", repeat: true });
    assert.equal(panel.selected, 3);
    assert.deepEqual(ipc.calls, []);
  });

  for (const guard of ["composing", "isComposing", "keyCode229"]) {
    await suite.test(`${guard} 仍阻止提交和方向键导航`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      if (guard === "composing") panel.startComposition();
      const options = guard === "isComposing" ? { isComposing: true } : guard === "keyCode229" ? { keyCode: 229 } : {};
      for (const action of [...submissions, { key: "ArrowDown" }, { key: "ArrowUp" }]) {
        keydown(panel, { ...action, ...options });
        assert.deepEqual(ipc.calls, []);
        assert.equal(panel.selected, 1);
      }
    });
  }

  await suite.test("组合结束后的原 50ms 保护及阈值保持不变", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    panel.startComposition();
    panel.endComposition();
    advance(49);
    for (const action of [...submissions, { key: "ArrowDown" }, { key: "ArrowUp" }]) {
      keydown(panel, action);
      assert.deepEqual(ipc.calls, []);
      assert.equal(panel.selected, 1);
    }
    advance(1);
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])]);
  });
});

test("搜索选择快照与受限激活交互的反例", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const expected = (options = {}) => ({ command: "pasteItem", session: 37, id: 102, mode: "paste", selection: { revision: 3, nonce: 1 }, ...options });

  await suite.test("鼠标选择同样携带选择时的 revision 和 nonce", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    void panel.choose(2, "copy", click());
    assert.deepEqual(ipc.calls, [expected({ id: 103, mode: "copy" })]);
  });

  await suite.test("选择在任何 await 前固定会话、条目、模式、revision 和 nonce", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    const pending = ipc.deferNextPaste();
    const choosing = panel.choose(1, "text", click());
    assert.deepEqual(ipc.calls, [expected({ mode: "text" })], "第一处 await 前必须已经发出完整快照");
    advance(1);
    ipc.emit("foreground-revision", { session: 37, revision: 4 });
    panel.searchItems = [{ ...items()[0], id: 999 }];
    panel.selected = 0;
    ipc.emit("shown", shown({ session: 38, foregroundRevision: 9 }));
    pending.resolve();
    await choosing;
    assert.deepEqual(ipc.calls, [expected({ mode: "text" })]);
  });

  await suite.test("选择后收到的 revision 不得改写已发送的快照", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    keydown(panel, { key: "Enter" });
    advance(2);
    ipc.emit("foreground-revision", { session: 37, revision: 4 });
    assert.deepEqual(ipc.calls, [expected()]);
  });

  await suite.test("迟到 DOM 选择早于 revision 接收时刻时拒绝，并要求新选择", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    const event = click();
    advance(1);
    ipc.emit("foreground-revision", { session: 37, revision: 4 });
    await panel.choose(1, "paste", event);
    assert.deepEqual(ipc.calls, []);
    assert.equal(panel.busy, false);
    assert.match(panel.message, /重新选择|重试/);
    await panel.choose(1, "paste", click());
    assert.deepEqual(ipc.calls, [expected({ selection: { revision: 4, nonce: 1 } })]);
  });

  await suite.test("选择前的新 revision 允许新的可信确认，不延用旧会话整轮锁存", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    advance(10);
    ipc.emit("foreground-revision", { session: 37, revision: 8 });
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expected({ selection: { revision: 8, nonce: 1 } })]);
  });

  for (const [name, timeStamp] of [["早于快照", 999], ["负数", -1], ["NaN", NaN], ["无穷大", Infinity], ["未来时间", 1001], ["Unix 时间源", Date.now()]]) {
    await suite.test(`${name} 的选择时间戳不能建立许可`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      keydown(panel, { key: "Enter", timeStamp });
      assert.deepEqual(ipc.calls, []);
      assert.equal(panel.busy, false);
    });
  }

  for (const kind of ["keyboard", "mouse", "missing"]) {
    await suite.test(`${kind} 非可信或缺失 DOM 事件不能建立许可`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      if (kind === "keyboard") keydown(panel, { key: "Enter", isTrusted: false });
      else await panel.choose(1, "paste", kind === "mouse" ? click({ isTrusted: false }) : undefined);
      assert.deepEqual(ipc.calls, []);
      assert.equal(panel.busy, false);
    });
  }

  await suite.test("同一 DOM 事件在 IPC 失败后不能再使用，新的事件 nonce 继续递增", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.rejectNextPaste();
    const event = keydown(panel, { key: "Enter" });
    await flush();
    panel.keydown(event);
    assert.deepEqual(ipc.calls, [expected()]);
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expected(), expected({ selection: { revision: 3, nonce: 2 } })]);
  });

  await suite.test("组合期间及结束后 50ms 内鼠标也不能取用", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    panel.startComposition();
    await panel.choose(1, "paste", click());
    panel.endComposition();
    advance(49);
    await panel.choose(1, "paste", click());
    assert.deepEqual(ipc.calls, []);
    advance(1);
    await panel.choose(1, "paste", click());
    assert.deepEqual(ipc.calls, [expected()]);
  });

  for (const revision of [2, 3]) {
    await suite.test(`旧/重复 revision ${revision} 保持高水位，挡旧 DOM 后允许新的确认`, async (context) => {
      const { panel, advance } = await createPanel(context, PanelState, ipc);
      const event = click();
      advance(1);
      ipc.emit("foreground-revision", { session: 37, revision });
      await panel.choose(1, "paste", event);
      assert.deepEqual(ipc.calls, []);
      await panel.choose(1, "paste", click());
      assert.deepEqual(ipc.calls, [expected()]);
    });
  }

  for (const revision of [-1, NaN, Number.MAX_SAFE_INTEGER + 1]) {
    await suite.test(`无效 revision ${String(revision)} 保守冻结，新的合法通知才恢复`, async (context) => {
      const { panel, advance } = await createPanel(context, PanelState, ipc);
      advance(1);
      ipc.emit("foreground-revision", { session: 37, revision });
      keydown(panel, { key: "Enter" });
      assert.deepEqual(ipc.calls, []);
      assert.match(panel.message, /重新呼出/);
      advance(1);
      ipc.emit("foreground-revision", { session: 37, revision: 4 });
      keydown(panel, { key: "Enter" });
      assert.deepEqual(ipc.calls, [expected({ selection: { revision: 4, nonce: 1 } })]);
    });
  }

  await suite.test("防抖期内产生但延迟超过 50ms 才处理的选择不能取用", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    panel.startComposition();
    panel.endComposition();
    advance(49);
    const event = click();
    advance(10);
    await panel.choose(1, "paste", event);
    keydown(panel, { key: "Enter", timeStamp: 1049 });
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expected()]);
  });

  await suite.test("旧会话和未来会话通知不能推进当前会话 revision 或其接收时刻", async (context) => {
    const { panel, advance } = await createPanel(context, PanelState, ipc);
    advance(100);
    for (const session of [36, 38]) {
      ipc.emit("foreground-revision", { session, revision: 10 });
      ipc.emit("interaction", { session, interaction: "passive" });
      ipc.emit("mode", { session, mode: "search" });
    }
    keydown(panel, { key: "Enter", timeStamp: 1050 });
    assert.deepEqual(ipc.calls, [expected()]);
  });

  await suite.test("同一会话重复 shown 不重置 nonce，不覆盖条目和 revision", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.rejectNextPaste();
    keydown(panel, { key: "Enter" });
    await flush();
    ipc.emit("shown", shown({ foregroundRevision: 0 }));
    assert.equal(panel.mode, "search");
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expected(), expected({ selection: { revision: 3, nonce: 2 } })]);
  });

  await suite.test("新会话独立初始化 nonce，旧请求错误不能解除新会话 busy", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    const pending = ipc.deferNextPaste();
    keydown(panel, { key: "Enter" });
    ipc.emit("shown", shown({ session: 38, foregroundRevision: 5 }));
    panel.mode = "search";
    panel.searchItems = items();
    keydown(panel, { key: "Enter" });
    pending.reject(new Error("旧会话失败"));
    await flush();
    assert.equal(panel.busy, true);
    assert.deepEqual(ipc.calls, [expected(), expected({ session: 38, id: 101, selection: { revision: 5, nonce: 1 } })]);
  });

  await suite.test("panel:key 和无 DOM 的 navigate 不能在搜索模式建立许可", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    for (const key of ["Enter", "Ctrl+Enter", "Shift+Enter", "1"]) {
      ipc.emit("key", { session: 37, key });
      panel.navigate(key);
    }
    assert.deepEqual(ipc.calls, []);
    assert.equal(panel.busy, false);
  });

  for (const interaction of ["activation_pending", "activation_failed"]) {
    await suite.test(`${interaction} 没有可取用列表，数字和鼠标都不能提交`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc, { interaction });
      assert.equal(panel.filtered.length, 0);
      assert.equal(panel.locked, true);
      ipc.emit("key", { session: 37, key: "1" });
      keydown(panel, { key: "Enter" });
      await panel.choose(1, "paste", click());
      panel.searchPointer({ preventDefault() {} });
      assert.deepEqual(ipc.calls, []);
    });
  }

  await suite.test("激活失败只允许当前会话的可信鼠标重试", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc, { interaction: "activation_failed" });
    await panel.retryActivation(click({ isTrusted: false }));
    await panel.retryActivation(click({ detail: 0 }));
    assert.deepEqual(ipc.calls, []);
    await panel.retryActivation(click());
    assert.deepEqual(ipc.calls, [{ command: "retryActivation", session: 37 }]);
  });

  await suite.test("已激活列表仍是 quick，DOM 数字松开取用且 panel:key 不双发", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    assert.equal(panel.mode, "quick");
    assert.equal(panel.filtered.length, 9);
    keydown(panel, { key: "1" });
    assert.equal(panel.mode, "quick");
    assert.deepEqual(ipc.calls, []);
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "1" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste" }]);
  });

  await suite.test("activated 可显式进入搜索，重复 activated 不能重置搜索，DOM 选择仍需快照", async (context) => {
    const { panel, runFrame } = await activatedPanel(context, PanelState, ipc);
    panel.searchPointer({ type: "pointerdown", isTrusted: true, target: panel.input, timeStamp: performance.now(), preventDefault() {} });
    assert.deepEqual(ipc.calls, [{ command: "startSearch", session: 37 }]);
    ipc.calls.length = 0;
    const switching = ipc.emit("mode", { session: 37, mode: "search" });
    await flush();
    await runFrame();
    await runFrame();
    await switching;
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    assert.equal(panel.mode, "search");
    assert.equal(panel.locked, false);
    keydown(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [], "Activated Search 也必须等待选择键松开");
    keyup(panel, { key: "Enter" });
    assert.deepEqual(ipc.calls, [expected()]);
  });

  await suite.test("被动提示移除全部取用和搜索入口，迟到 activated 不能重新打开", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.emit("paste-result", pasteResult({ hint_dwell_ms: 3000, hint_message: "已复制，请点击目标输入位置后按 Ctrl+V" }));
    assert.equal(panel.interaction, "passive");
    assert.equal(panel.filtered.length, 0);
    assert.equal(panel.locked, true);
    assert.equal(panel.busy, true);
    assert.equal(panel.message, "已复制，请点击目标输入位置后按 Ctrl+V");
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    ipc.emit("interaction", { session: 37, interaction: "passive" });
    await ipc.emit("mode", { session: 37, mode: "search" });
    keydown(panel, { key: "Enter" });
    ipc.emit("key", { session: 37, key: "1" });
    await panel.choose(1, "paste", click());
    panel.searchPointer({ preventDefault() {} });
    assert.equal(panel.interaction, "passive");
    assert.equal(panel.message, "已复制，请点击目标输入位置后按 Ctrl+V");
    assert.deepEqual(ipc.calls, []);
    panel.close();
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  await suite.test("原生命令结果未知始终被动锁定，提示先检查目标而非再次粘贴", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.emit("paste-result", pasteResult({ paste_result: "native_submission_unknown", reason: "native_submission_unknown" }));
    assert.equal(panel.interaction, "passive");
    assert.equal(panel.busy, true);
    assert.equal(panel.locked, true);
    assert.equal(panel.message, "粘贴结果未确认，请先检查目标，避免重复粘贴");
    assert.doesNotMatch(panel.message, /已复制|Ctrl\+V|已粘贴/);
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    await flush();
    assert.deepEqual(ipc.calls, []);
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    assert.equal(panel.interaction, "passive");
  });

  await suite.test("没有 hint_message 时不把权限未知目标说成管理员", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    ipc.emit("paste-result", pasteResult({ hint_dwell_ms: 3000 }));
    assert.match(panel.message, /已复制/);
    assert.doesNotMatch(panel.message, /管理员/);
    assert.match(panel.message, /点击目标输入位置/);
  });

  await suite.test("已确认的被动提示不被迟到同会话 IPC 失败或错误事件覆盖", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    const pending = ipc.deferNextPaste();
    keydown(panel, { key: "Enter" });
    const hint = "已复制，请点击目标输入位置后按 Ctrl+V";
    ipc.emit("paste-result", pasteResult({ hint_dwell_ms: 3000, hint_message: hint }));
    pending.reject(new Error("迟到的 IPC 失败"));
    await flush();
    assert.equal(panel.message, hint);
    ipc.emit("error", { session: 37, message: "迟到的错误" });
    assert.equal(panel.message, hint);
    assert.equal(panel.busy, true);
  });

  await suite.test("旧会话或隐藏后的结果不改变当前可见状态和提示", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.message = "当前会话";
    ipc.emit("paste-result", pasteResult({ session: 36, hint_dwell_ms: 3000, hint_message: "旧提示" }));
    assert.equal(panel.message, "当前会话");
    ipc.emit("hidden", { session: 37 });
    ipc.emit("paste-result", pasteResult({ hint_dwell_ms: 3000, hint_message: "迟到提示" }));
    assert.equal(panel.visible, false);
    assert.equal(panel.message, "当前会话");
  });
});

test("普通 Quick 的 Tab 与 Esc 浮层分层键位", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);

  await suite.test("Tab 开合，Esc 先关浮层、再次 Esc 才请求收起", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    assert.equal(panel.previewOpen, false);
    ipc.emit("key", { session: 37, key: "Tab" });
    assert.equal(panel.previewOpen, true);
    ipc.emit("key", { session: 37, key: "Tab" });
    assert.equal(panel.previewOpen, false);
    ipc.emit("key", { session: 37, key: "Tab" });
    assert.equal(panel.previewOpen, true);
    ipc.emit("key", { session: 37, key: "Escape" });
    assert.equal(panel.previewOpen, false);
    assert.equal(panel.visible, true);
    assert.deepEqual(ipc.calls, []);
    ipc.emit("key", { session: 37, key: "Escape" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });

  await suite.test("完整预览开着时 Z 切换适应 / 原尺寸，钩子同步开关；关着时 Z 不起作用", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    ipc.emit("key", { session: 37, key: "Z" });
    assert.equal(panel.previewZoom, "fit", "预览没开，Z 不起作用");
    ipc.emit("key", { session: 37, key: "Tab" });
    assert.deepEqual(ipc.previewKeys, [true], "打开预览时通知钩子");
    ipc.emit("key", { session: 37, key: "Z" });
    assert.equal(panel.previewZoom, "original");
    ipc.emit("key", { session: 37, key: "Z" });
    assert.equal(panel.previewZoom, "fit");
    ipc.emit("key", { session: 37, key: "Z" });
    ipc.emit("key", { session: 37, key: "Escape" });
    assert.deepEqual(ipc.previewKeys, [true, false], "关闭预览时通知钩子");
    ipc.emit("key", { session: 37, key: "Tab" });
    assert.equal(panel.previewZoom, "fit", "重新打开从“适应”开始");
  });

  await suite.test("完整预览开着时方向键、翻页键用来滚动预览，不切换条目；关掉后照常切换", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    const start = panel.selected;
    ipc.emit("key", { session: 37, key: "Tab" });
    for (const key of ["ArrowDown", "ArrowUp", "PageDown", "End", "Home"]) ipc.emit("key", { session: 37, key });
    assert.equal(panel.selected, start, "预览开着时不切换条目");
    assert.equal(panel.previewOpen, true);
    ipc.emit("key", { session: 37, key: "Tab" });
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.selected, start + 1, "关掉预览后方向键照常切换");
  });

  await suite.test("完整预览里 Ctrl+C 只复制选中的文字，面板不关；没选中时提示", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    const original = globalThis.window;
    let selected = "";
    globalThis.window = { ...(original ?? {}), getSelection: () => ({ toString: () => selected }) };
    context.after(() => { globalThis.window = original; });
    ipc.emit("key", { session: 37, key: "Ctrl+C" });
    assert.deepEqual(ipc.calls, [], "预览没开时不处理");
    ipc.emit("key", { session: 37, key: "Tab" });
    ipc.emit("key", { session: 37, key: "Ctrl+C" });
    await flush();
    assert.deepEqual(ipc.calls, []);
    assert.match(panel.message, /先用鼠标选中/);
    selected = "选中的字😀";
    ipc.emit("key", { session: 37, key: "Ctrl+C" });
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "copySelection", id: panel.currentItem.id, text: "选中的字😀" }]);
    assert.equal(panel.notice, "已复制选中的 5 字");
    assert.equal(panel.message, "");
    assert.equal(panel.previewOpen, true, "复制后预览和面板都不关");
  });

  await suite.test("搜索模式 Space 仍交给输入框，手牌左右键始终换牌", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    const before = panel.selected;
    const event = keydown(panel, { key: " ", code: "Space" });
    assert.equal(event.defaultPrevented, false);
    assert.deepEqual(ipc.calls, []);
    panel.mode = "quick";
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.selected, before + 1);
    ipc.emit("key", { session: 37, key: "ArrowLeft" });
    assert.equal(panel.selected, before);
  });
});

test("交互优化：收藏不占槽位、本次呼出不挪位置、删除可撤销、修饰键提示与静默成功", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const favorite = (item, position) => ({ ...item, favorite: true, favorite_position: position });
  const pinnedHistory = () => {
    const list = items().map((item) => ({ ...item, favorite: false, favorite_position: null }));
    list[7] = favorite(list[7], 1);
    list[8] = favorite(list[8], 2);
    return { items: list, total: list.length, recording: true, last_recorded_ms: null, store_status: null };
  };
  const quick = async (context, history = pinnedHistory()) => {
    const harness = await createPanel(context, PanelState, ipc, { history });
    harness.panel.mode = "quick";
    await flush();
    return harness;
  };

  await suite.test("常用排在前面但不占数字槽，数字从最近区当前页第一张开始", async (context) => {
    const { panel } = await quick(context);
    assert.deepEqual(panel.filtered.slice(0, 3).map((item) => item.id), [108, 109, 101]);
    assert.deepEqual(panel.handSource.map((item) => item.id), [101, 102, 103, 104, 105, 106, 107]);
    assert.equal(panel.fanItems.length, 7);
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste" }]);
  });

  await suite.test("面板开着时收藏/取消只换星标不挪位置，下次呼出才重排", async (context) => {
    const { panel } = await quick(context);
    panel.selected = 2;
    const before = panel.filtered.map((item) => item.id);
    ipc.emit("key", { session: 37, key: "Ctrl+D" });
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "setFavorite", id: 103, favorite: true }]);
    assert.deepEqual(panel.filtered.map((item) => item.id), before, "收藏后顺序不变");
    assert.equal(panel.currentItem.id, 103);
    assert.equal(panel.currentItem.favorite, true);
    assert.equal(panel.handSource[2].id, 103, "数字与当前页键帽保持一致");
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.currentItem.id, 104, "紧接着的方向键落在原来的下一条");
    panel.switchHandList("favorites");
    panel.selected = 0;
    ipc.emit("key", { session: 37, key: "Ctrl+D" });
    await flush();
    assert.equal(panel.filtered[0].id, 108, "取消收藏也留在原位");
    assert.equal(panel.filtered[0].favorite, false);
    const next = pinnedHistory();
    next.items[2] = favorite(next.items[2], 3);
    ipc.emit("shown", shown({ session: 38, history: next }));
    assert.deepEqual(panel.filtered.slice(0, 3).map((item) => item.id), [108, 109, 103], "下次呼出按新收藏排");
  });

  await suite.test("Delete 移到回收站并停在原位置，Ctrl+Z 撤销回来", async (context) => {
    const { panel } = await quick(context);
    const full = pinnedHistory();
    panel.selected = 1;
    ipc.emit("key", { session: 37, key: "Delete:down" });
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "deleteItem", id: 102 }], "普通条目按下就删");
    ipc.emit("key", { session: 37, key: "Delete" });
    await flush();
    assert.equal(ipc.calls.length, 1, "松开不再删第二条");
    assert.ok(panel.filtered.every((item) => item.id !== 102));
    assert.equal(panel.currentItem.id, 103);
    assert.equal(panel.undo.id, 102);
    assert.equal(panel.undo.session, 37);
    assert.equal(panel.undo.label, "合成条目");
    assert.equal(panel.undo.duration, 5000);
    ipc.store.history = full;
    ipc.store.undoable = 102;
    ipc.emit("key", { session: 37, key: "Ctrl+Z" });
    await flush(); await flush();
    assert.deepEqual(ipc.calls.at(-1), { command: "undoDelete" });
    assert.equal(panel.undo, null);
    assert.equal(panel.currentItem.id, 102);
    ipc.store.undoable = null;
    ipc.emit("key", { session: 37, key: "Ctrl+Z" });
    await flush();
    assert.match(panel.message, /没有可以撤销/);
  });

  await suite.test("收藏的要按住 Delete：按满才删，提前松开或移开选择都取消", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await quick(context);
    panel.switchHandList("favorites");
    panel.selected = 0;
    const pinned = panel.currentItem;
    assert.equal(pinned.favorite, true);
    ipc.emit("key", { session: 37, key: "Delete:down" });
    await flush();
    assert.deepEqual(ipc.calls, [], "按下只开始计时");
    assert.equal(panel.holdDelete.id, pinned.id);
    assert.equal(panel.holdDelete.phase, "holding");
    assert.equal(panel.holdDelete.duration, 600);
    assert.equal(panel.message, "", "不用红色提示行");
    context.mock.timers.tick(300);
    ipc.emit("key", { session: 37, key: "Delete" });
    await flush();
    assert.equal(panel.holdDelete.phase, "hint", "提前松开换成提示");
    context.mock.timers.tick(600);
    await flush();
    assert.deepEqual(ipc.calls, [], "提前松开不删");
    context.mock.timers.tick(1000);
    assert.equal(panel.holdDelete, null, "提示过一会儿消失");

    ipc.emit("key", { session: 37, key: "Delete:down" });
    ipc.emit("key", { session: 37, key: "ArrowDown" });
    assert.equal(panel.holdDelete, null, "按住时移开选择就取消");
    context.mock.timers.tick(700);
    await flush();
    assert.deepEqual(ipc.calls, []);

    panel.switchHandList("favorites");
    panel.selected = 0;
    ipc.emit("key", { session: 37, key: "Delete:down" });
    ipc.emit("key", { session: 37, key: "Delete:down" });
    context.mock.timers.tick(600);
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "deleteItem", id: pinned.id }], "按满才删，且只删一次");
    assert.equal(panel.holdDelete.phase, "holding", "拍底版之前进度保持铺满");
    context.mock.timers.tick(100);
    assert.equal(panel.holdDelete, null);
    assert.equal(panel.undo.id, pinned.id, "删掉后照常给撤销标签");
    ipc.emit("key", { session: 37, key: "Delete" });
    await flush();
    assert.equal(ipc.calls.length, 1, "按满后的松开什么都不做");
  });

  await suite.test("主线程忙、计时器晚到时，松开按实际按住的时长算：已满就删", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await quick(context);
    panel.switchHandList("favorites");
    panel.selected = 0;
    const pinned = panel.currentItem;
    ipc.emit("key", { session: 37, key: "Delete:down" });
    const now = Date.now;
    Date.now = () => now() + 700;
    try {
      ipc.emit("key", { session: 37, key: "Delete" });
    } finally {
      Date.now = now;
    }
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "deleteItem", id: pinned.id }]);
    context.mock.timers.tick(600);
    await flush();
    assert.equal(ipc.calls.length, 1, "晚到的计时器不再删第二次");
  });

  await suite.test("搜索模式：Delete 留给输入框，Ctrl+Delete 删当前结果，收藏的同样要按住", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await quick(context);
    panel.mode = "search";
    panel.selected = 3;
    ipc.emit("key", { session: 37, key: "Delete:down" });
    ipc.emit("key", { session: 37, key: "Delete" });
    await flush();
    assert.deepEqual(ipc.calls, [], "搜索模式的 Delete 留给输入框");
    panel.searchItems = pinnedHistory().items;
    const target = panel.currentItem;
    const event = keydown(panel, { key: "Delete", ctrlKey: true });
    await flush();
    assert.ok(event.defaultPrevented);
    assert.equal(target.favorite, false);
    assert.deepEqual(ipc.calls, [{ command: "deleteItem", id: target.id }], "搜索模式用 Ctrl+Delete 删除当前结果");
    ipc.calls.length = 0;
    panel.selected = 0;
    assert.equal(panel.currentItem.favorite, true);
    keydown(panel, { key: "Delete", ctrlKey: true });
    assert.equal(panel.holdDelete.keys, "Ctrl+Delete");
    keyup(panel, { key: "Delete", ctrlKey: true });
    assert.equal(panel.holdDelete.phase, "hint");
    context.mock.timers.tick(700);
    await flush();
    assert.deepEqual(ipc.calls, [], "提前松开不删");
  });

  await suite.test("新一次呼出清掉上一会话的撤销提示，Ctrl+Z 仍能撤销", async (context) => {
    const { panel } = await quick(context);
    panel.selected = 1;
    ipc.emit("key", { session: 37, key: "Delete:down" });
    await flush();
    assert.ok(panel.undo);
    ipc.emit("shown", shown({ session: 38, history: pinnedHistory() }));
    assert.equal(panel.undo, null);
    ipc.store.history = pinnedHistory();
    ipc.store.undoable = 102;
    ipc.calls.length = 0;
    ipc.emit("key", { session: 38, key: "Ctrl+Z" });
    await flush(); await flush();
    assert.deepEqual(ipc.calls, [{ command: "undoDelete" }]);
    assert.equal(panel.currentItem.id, 102);
  });

  await suite.test("按住 Ctrl/Shift 超过 300 ms 才换页脚提示，快速组合键不闪", async (context) => {
    const { panel } = await quick(context);
    ipc.emit("key", { session: 37, key: "Mod:Ctrl" });
    assert.equal(panel.heldModifier, "");
    ipc.emit("key", { session: 37, key: "Mod:" });
    await new Promise((resolve) => setTimeout(resolve, 350));
    assert.equal(panel.heldModifier, "", "快速松开不出提示");
    ipc.emit("key", { session: 37, key: "Mod:Shift" });
    await new Promise((resolve) => setTimeout(resolve, 350));
    assert.equal(panel.heldModifier, "shift");
    ipc.emit("key", { session: 37, key: "Mod:Ctrl+Shift" });
    assert.equal(panel.heldModifier, "", "两个都按着不显示单键提示");
    ipc.emit("key", { session: 37, key: "Mod:" });
    assert.equal(panel.heldModifier, "");
    assert.deepEqual(ipc.calls, [], "修饰键事件不触发任何取用");
  });

  await suite.test("粘贴成功不出声，只说需要用户知道的偏差", async (context) => {
    const { panel } = await quick(context);
    panel.message = "旧消息";
    ipc.emit("paste-result", pasteResult({ paste_result: "confirmed", reason: null, layer3: { conclusion: "confirmed" } }));
    assert.equal(panel.message, "");
    ipc.emit("paste-result", pasteResult({ paste_result: "sent", reason: null, layer2: { timed_out: true } }));
    assert.match(panel.message, /可能没收到/);
    ipc.emit("paste-result", pasteResult({ paste_result: "confirmed", reason: null, as_text: { error: "x" } }));
    assert.match(panel.message, /转文字失败/);
  });
});

test("鼠标点条目的行为按设置选择粘贴模式（M2-8）", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const auxclick = (options = {}) => click({ type: "auxclick", button: 1, ...options });
  const withSettings = async (context, interaction, harness = createPanel) => {
    const created = await harness(context, PanelState, ipc);
    // applySettings 会写 documentElement.dataset；替身 document 里补一个。
    document.documentElement = { dataset: {} };
    if (interaction !== undefined) created.panel.applySettings({ interaction });
    return created;
  };
  // 每次点击后清掉 busy（真实流程里由粘贴结果清除），以便连续点击。
  const pointer = (panel, index, event) => { panel.chooseByMouse(index, event); panel.busy = false; };
  const modes = () => ipc.calls.filter((call) => call.command === "pasteItem").map((call) => call.mode);

  await suite.test("默认：单击粘贴，中键纯文本粘贴", async (context) => {
    const { panel } = await withSettings(context);
    pointer(panel, 1, click());
    pointer(panel, 2, auxclick());
    assert.deepEqual(ipc.calls.map((call) => [call.id, call.mode]), [[102, "paste"], [103, "text"]]);
  });

  await suite.test("单击和中键各自跟随设置，包括只复制与纯文本", async (context) => {
    const { panel } = await withSettings(context, { placement: "mouse", click: "copy", middle_click: "paste" });
    pointer(panel, 1, click());
    pointer(panel, 1, auxclick());
    assert.deepEqual(modes(), ["copy", "paste"]);
    ipc.calls.length = 0;
    panel.applySettings({ interaction: { click: "text", middle_click: "copy" } });
    pointer(panel, 1, click());
    pointer(panel, 1, auxclick());
    assert.deepEqual(modes(), ["text", "copy"]);
  });

  await suite.test("不做额外操作：只选中该条，不发任何命令", async (context) => {
    const { panel } = await withSettings(context, { click: "none", middle_click: "none" });
    panel.selected = 0;
    panel.chooseByMouse(3, click());
    assert.equal(panel.selected, 3);
    panel.chooseByMouse(2, auxclick());
    assert.equal(panel.selected, 2);
    assert.deepEqual(ipc.calls, []);
  });

  await suite.test("无效或缺失的设置值回到默认", async (context) => {
    const { panel } = await withSettings(context, { click: "double", middle_click: 7 });
    assert.deepEqual({ ...panel.mouseActions }, { click: "paste", middle: "text" });
    panel.applySettings({});
    assert.deepEqual({ ...panel.mouseActions }, { click: "paste", middle: "text" });
  });

  await suite.test("键盘不受鼠标设置影响", async (context) => {
    const { panel } = await withSettings(context, { click: "none", middle_click: "none" });
    keydown(panel, { key: "Enter" });
    keyup(panel, { key: "Enter" });
    assert.deepEqual(modes(), ["paste"]);
  });

  await suite.test("Activated 搜索里中键同样走选择快照，鼠标事件类型 / 键位不符则拒绝", async (context) => {
    const { panel } = await withSettings(context, undefined, activatedSearch);
    panel.chooseByMouse(0, auxclick());
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "text", selection: { revision: 3, nonce: 1 } }]);
  });

  await suite.test("Activated 搜索里 auxclick 的键位必须是中键，非可信事件被忽略", async (context) => {
    const { panel } = await withSettings(context, undefined, activatedSearch);
    await panel.choose(0, "text", auxclick({ button: 2 }));
    await panel.choose(0, "text", auxclick({ isTrusted: false }));
    await panel.choose(0, "text", auxclick({ detail: 0 }));
    assert.deepEqual(ipc.calls, []);
  });
});

test("v10 手牌：分页、拈一叠、逐张放和数字搜索", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);

  await suite.test("扇面 → 摊开 → 扇面：同一批条目、顺序不变，选中项按规则保留", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.selected = 3;
    assert.equal(panel.pose, "fan");
    const order = panel.handSource.map((item) => item.id);
    assert.deepEqual(panel.fanItems.map((item) => item.id), order.slice(0, 7));
    ipc.emit("key", { session: 37, key: "ArrowDown" });
    assert.equal(panel.pose, "spread", "最近扇面 ↓ 摊开");
    assert.equal(panel.currentItem.id, 104, "↓ 摊开后选中的还是同一张");
    assert.deepEqual(panel.handSource.map((item) => item.id), order, "摊开不重排");
    ipc.emit("key", { session: 37, key: "ArrowDown" });
    assert.equal(panel.currentItem.id, 105, "摊开里 ↓ 逐条");
    ipc.emit("key", { session: 37, key: "Escape" });
    assert.equal(panel.pose, "fan");
    assert.equal(panel.currentItem.id, 105, "收回后原来选中的在前 7 张里就保持选中");
    assert.deepEqual(ipc.calls, []);
    ipc.emit("key", { session: 37, key: "PageDown" });
    assert.equal(panel.pose, "spread");
    assert.equal(panel.selected, 7, "PgDn 摊开到第 8 条");
    ipc.emit("key", { session: 37, key: "Home" });
    assert.equal(panel.pose, "fan");
    assert.equal(panel.selected, 0, "第 8 条以后收回时回到第 1 张");
    panel.selected = 6;
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.pose, "spread", "第 7 张再按 → 摊开");
    assert.equal(panel.selected, 7);
    panel.selected = 0;
    ipc.emit("key", { session: 37, key: "ArrowUp" });
    assert.equal(panel.pose, "fan", "第一条按 ↑ 收回");
    assert.deepEqual(panel.handSource.map((item) => item.id), order);
  });

  await suite.test("← → 在扇面、摊开、搜索三种状态都是上一张 / 下一张", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    const before = panel.selected;
    const right = keydown(panel, { key: "ArrowRight" });
    assert.equal(right.defaultPrevented, true, "搜索中 → 不移动输入光标");
    assert.equal(panel.selected, before + 1);
    keydown(panel, { key: "ArrowLeft" });
    assert.equal(panel.selected, before);
    panel.mode = "quick"; panel.selected = 2;
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.selected, 3, "扇面");
    panel.spreadOpen(3);
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    assert.equal(panel.selected, 4, "摊开");
    ipc.emit("key", { session: 37, key: "ArrowLeft" });
    assert.equal(panel.selected, 3);
    assert.equal(panel.pose, "spread");
  });

  await suite.test("常用扇面：↑ ↓ 都回到最近，PgDn 才摊开（超过 7 张时）", async (context) => {
    const list = items().map((item, index) => ({ ...item, favorite: true, favorite_position: index + 1 }));
    const history = { items: [...list, { ...items()[0], id: 300 }], total: 10, recording: true, last_recorded_ms: null, store_status: null };
    const { panel } = await createPanel(context, PanelState, ipc, { history });
    panel.mode = "quick"; panel.selected = 0;
    ipc.emit("key", { session: 37, key: "ArrowUp" });
    assert.equal(panel.handList, "favorites");
    ipc.emit("key", { session: 37, key: "ArrowDown" });
    assert.equal(panel.handList, "recent", "常用里 ↓ 只回到最近");
    assert.equal(panel.pose, "fan");
    ipc.emit("key", { session: 37, key: "ArrowUp" });
    ipc.emit("key", { session: 37, key: "PageDown" });
    assert.equal(panel.pose, "spread");
    assert.equal(panel.handList, "favorites");
    assert.equal(panel.selected, 7);
  });

  await suite.test("最近为空而常用有内容时落到常用", async (context) => {
    const list = items().slice(0, 3).map((item, index) => ({ ...item, favorite: true, favorite_position: index + 1 }));
    const { panel } = await createPanel(context, PanelState, ipc, { history: { items: list, total: 3, recording: true, last_recorded_ms: null, store_status: null } });
    assert.equal(panel.handList, "favorites");
  });

  await suite.test("Shift+Enter 始终只复制，Ctrl+Shift+Enter 才逐张放", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.selected = 1;
    ipc.emit("key", { session: 37, key: "Space" });
    ipc.emit("key", { session: 37, key: "Space" });
    assert.deepEqual(panel.stack.map((item) => item.id), [102, 103]);
    ipc.emit("key", { session: 37, key: "Shift+Enter" });
    assert.deepEqual(ipc.calls, [], "有已选时 Shift+Enter 不执行");
    assert.match(panel.notice, /已选 2 张，Shift\+↵ 不会丢掉它们/);
    assert.deepEqual(panel.stack.map((item) => item.id), [102, 103], "已选没有被悄悄丢掉");
    panel.stack = [];
    ipc.emit("key", { session: 37, key: "Shift+Enter" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 104, mode: "copy" }], "没有已选时只复制当前一张");
    panel.busy = false;
    panel.stack = items().slice(1, 3);
    ipc.emit("key", { session: 37, key: "Ctrl+Shift+Enter" });
    assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [102, 103], automatic: false, mode: "paste" });
    panel.busy = false;
    ipc.emit("key", { session: 37, key: "Enter" });
    assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [102, 103], automatic: true, mode: "paste" });
    panel.busy = false;
    ipc.emit("key", { session: 37, key: "Ctrl+Enter" });
    assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [102, 103], automatic: true, mode: "text" });
  });

  await suite.test("Backspace 放回最后一张，已拈条目不会重复进叠", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.selected = 0;
    ipc.emit("key", { session: 37, key: "Space" });
    ipc.emit("key", { session: 37, key: "ArrowLeft" });
    ipc.emit("key", { session: 37, key: "Space" });
    assert.deepEqual(panel.stack.map((item) => item.id), [101]);
    ipc.emit("key", { session: 37, key: "Backspace" });
    assert.equal(panel.stack.length, 0);
    assert.equal(panel.currentItem.id, 101);
  });

  await suite.test("搜索框的plain数字不取用，Ctrl+数字才取用", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    const plain = keydown(panel, { key: "1" });
    assert.equal(plain.defaultPrevented, false);
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "1", ctrlKey: true });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste", selection: { revision: 3, nonce: 1 } }]);
  });

  await suite.test("Activated 逐张放也等 Enter、Ctrl、Shift 全部真实松开", async (context) => {
    const { panel } = await activatedPanel(context, PanelState, ipc);
    panel.selected = 1;
    keydown(panel, { key: " ", code: "Space" });
    keyup(panel, { key: " ", code: "Space" });
    keydown(panel, { key: " ", code: "Space" });
    keyup(panel, { key: " ", code: "Space" });
    keydown(panel, { key: "Enter", ctrlKey: true, shiftKey: true });
    keyup(panel, { key: "Enter", ctrlKey: true, shiftKey: true });
    keyup(panel, { key: "Control", code: "ControlLeft", shiftKey: true });
    assert.deepEqual(ipc.calls, []);
    keyup(panel, { key: "Shift", code: "ShiftLeft" });
    assert.deepEqual(ipc.calls, [{ command: "startQueue", session: 37, ids: [102, 103], automatic: false, mode: "paste" }]);
    assert.deepEqual(panel.stack.map((item) => item.id), [102, 103]);
    // 松键提交路径同样在 IPC Promise continuation 之前接受入队事件。
    ipc.emit("queue", { session: 37, remaining: 2, total: 2, automatic: false, busy: false });
    assert.equal(panel.stack.length, 0);
    await flush();
  });

  await suite.test("被动提示仍保留当前那张牌，不回跳第一条", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.selected = 3;
    ipc.emit("paste-result", pasteResult({ hint_dwell_ms: 3000, hint_message: "只复制提示" }));
    assert.equal(panel.interaction, "passive");
    assert.equal(panel.currentItem.id, 104);
  });
});

test("v10 前端审查：冻结手牌、当前页数字、一叠和边界保护", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);

  await suite.test("搜索结果含常用时 Ctrl+数字取到的就是键帽上那一条；只认 1–7", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    // 搜索结果里 108、109 是常用：排在最前，自成一组；键帽从看得见的第一条开始数。
    panel.searchItems = items().map((item) => item.id >= 108 ? { ...item, favorite: true, favorite_position: item.id - 107 } : item);
    panel.selected = 0;
    assert.equal(panel.pose, "spread");
    const keycap = (digit) => panel.handSource[panel.spread.visible[digit - 1]].id;
    assert.equal(keycap(1), 108);
    for (const digit of [1, 2, 3, 7]) {
      const expected = keycap(digit);
      keydown(panel, { key: String(digit), ctrlKey: true });
      assert.equal(ipc.calls.at(-1)?.id, expected, `Ctrl+${digit} 等于键帽 ${digit} 那张`);
      panel.busy = false;
    }
    ipc.calls.length = 0;
    for (const key of ["8", "9", "0"]) {
      const event = keydown(panel, { key, ctrlKey: true });
      assert.equal(event.defaultPrevented, true);
    }
    assert.deepEqual(ipc.calls, [], "Ctrl+8/9/0 不取用");
    // 选中滚到下面后，键帽跟着看得见的行重排，数字仍对应眼前的条目。
    const many = Array.from({ length: 30 }, (_, index) => ({ ...items()[0], id: 401 + index }));
    panel.searchItems = many;
    panel.selected = 25;
    const first = panel.handSource[panel.spread.visible[0]].id;
    assert.ok(first > 401, "滚动后第一条可见行不是第 1 条");
    keydown(panel, { key: "1", ctrlKey: true });
    assert.equal(ipc.calls.at(-1)?.id, first);
  });

  await suite.test("多选 2 张 → 进入搜索 → Enter / Ctrl+Enter 发出队列", async (context) => {
    for (const action of [{ key: "Enter", mode: "paste" }, { key: "Enter", ctrlKey: true, mode: "text" }]) {
      await context.test(`普通 quick 搜索 ${action.ctrlKey ? "Ctrl+" : ""}Enter`, async (subcontext) => {
        const { panel } = await createPanel(subcontext, PanelState, ipc);
        panel.mode = "quick"; panel.selected = 0;
        ipc.emit("key", { session: 37, key: "Space" });
        ipc.emit("key", { session: 37, key: "Space" });
        assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
        panel.mode = "search"; panel.query = "别的词";
        panel.searchItems = items().slice(5);
        keydown(panel, action);
        assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [101, 102], automatic: true, mode: action.mode, selection: { revision: 3, nonce: 1 } });
        assert.doesNotMatch(panel.message, /无法确认/);
      });
      await context.test(`Activated 搜索 ${action.ctrlKey ? "Ctrl+" : ""}Enter（已选不在当前结果里）`, async (subcontext) => {
        const { panel } = await activatedSearch(subcontext, PanelState, ipc);
        panel.stack = [{ ...items()[0], id: 900 }, { ...items()[0], id: 901 }];
        keydown(panel, action); keyup(panel, action);
        if (action.ctrlKey) keyup(panel, { key: "Control", code: "ControlLeft" });
        assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [900, 901], automatic: true, mode: action.mode, selection: { revision: 3, nonce: 1 } });
      });
    }
  });

  await suite.test("面板可见时来了新复制：数字 1–7 对应的 id 不变，只给“新来 N 张”", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    const before = [1, 2, 3, 4, 5, 6, 7].map((digit) => panel.handSource[panel.digitTarget(digit)].id);
    ipc.store.history = { ...panel.history, items: [{ ...items()[0], id: 200 }, { ...items()[0], id: 201 }, ...items()] };
    ipc.emit("history-changed", { session: 37 });
    await flush(); await flush();
    assert.deepEqual([1, 2, 3, 4, 5, 6, 7].map((digit) => panel.handSource[panel.digitTarget(digit)].id), before);
    assert.equal(panel.arrivals, 2);
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls.at(-1), { command: "pasteItem", session: 37, id: before[0], mode: "paste" });
  });

  await suite.test("Esc 逐层退出：看全 → 搜索 → 已选（可撤销）→ 摊开 → 收起", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.stack = items().slice(0, 2);
    panel.navigate("Tab");
    assert.equal(panel.previewOpen, true, "搜索中 Tab 也能看全");
    keydown(panel, { key: "Escape" });
    assert.equal(panel.previewOpen, false);
    assert.equal(panel.query, "合成");
    keydown(panel, { key: "Escape" });
    assert.equal(panel.query, "", "第二下清空搜索");
    assert.equal(panel.pose, "fan", "并收回扇面");
    assert.equal(panel.stack.length, 2);
    keydown(panel, { key: "Escape" });
    assert.equal(panel.stack.length, 0, "第三下清空已选");
    assert.match(panel.notice, /Ctrl\+Z 恢复/);
    const undo = keydown(panel, { key: "z", ctrlKey: true });
    assert.equal(undo.defaultPrevented, true);
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102], "Ctrl+Z 恢复已选");
    panel.stack = [];
    keydown(panel, { key: "ArrowDown" });
    assert.equal(panel.pose, "spread");
    keydown(panel, { key: "Escape" });
    assert.equal(panel.pose, "fan", "第四层收回扇面");
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }], "最后才收起面板");
  });

  await suite.test("搜索框删空回到扇面后：数字直接取用、空格多选，字母仍是搜索", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.query = "";
    assert.equal(panel.pose, "fan");
    const space = keydown(panel, { key: " ", code: "Space" });
    assert.equal(space.defaultPrevented, true);
    assert.equal(panel.stack.length, 1);
    const letter = keydown(panel, { key: "a", code: "KeyA" });
    assert.equal(letter.defaultPrevented, false, "字母交给输入框");
    const digit = keydown(panel, { key: "3", code: "Digit3" });
    assert.equal(digit.defaultPrevented, true);
    assert.equal(ipc.calls.at(-1)?.id, panel.handSource[2].id);
  });

  await suite.test("鼠标悬停：只有真实 pointermove 之后、且不忙不在看全时才改选中", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick"; panel.selected = 0;
    panel.hoverSelect(3);
    assert.equal(panel.selected, 0, "没有 pointermove 时静止的光标不改选中");
    panel.pointerMoved();
    panel.hoverSelect(3);
    assert.equal(panel.selected, 3);
    ipc.emit("key", { session: 37, key: "ArrowRight" });
    panel.hoverSelect(1);
    assert.equal(panel.selected, 4, "键盘换牌后要重新移动鼠标");
    panel.pointerMoved(); panel.busy = true;
    panel.hoverSelect(1);
    assert.equal(panel.selected, 4, "busy 时不改");
    panel.busy = false; panel.previewOpen = true;
    panel.hoverSelect(1);
    assert.equal(panel.selected, 4, "看全打开时不改");
    panel.previewOpen = false;
    panel.spreadOpen(4);
    panel.hoverSelect(1);
    assert.equal(panel.selected, 4, "摊开里悬停只加底色");
  });

  await suite.test("滚轮：扇面向下摊开，摊开里逐条，第一条再向上收回", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick"; panel.selected = 2;
    panel.wheel(-1);
    assert.equal(panel.pose, "fan");
    panel.wheel(1);
    assert.equal(panel.pose, "spread");
    assert.equal(panel.selected, 2);
    panel.wheel(1);
    assert.equal(panel.selected, 3);
    panel.selected = 0;
    panel.wheel(-1);
    assert.equal(panel.pose, "fan");
  });

  await suite.test("新复制和复用置顶不重排冻结手牌，下次呼出才采用新顺序", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    const before = panel.handSource.map((item) => item.id);
    const added = { ...items()[0], id: 100 };
    const next = [added, ...items()];
    ipc.store.history = { ...panel.history, items: next };
    ipc.emit("history-changed", { session: 37 });
    await flush(); await flush();
    assert.deepEqual(panel.handSource.map((item) => item.id), before);
    const reused = [next[5], ...next.filter((item) => item.id !== next[5].id)];
    ipc.store.history = { ...panel.history, items: reused };
    ipc.emit("history-changed", { session: 37 });
    await flush(); await flush();
    assert.deepEqual(panel.handSource.map((item) => item.id), before);
    ipc.emit("shown", shown({ session: 38, history: { ...panel.history, items: next } }));
    assert.equal(panel.handSource[0].id, 100);
  });

  await suite.test("一叠跨隐藏和呼出保留，Backspace 退回及队列确认入队才清空", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.selected = 0;
    panel.takeCurrent(); panel.takeCurrent();
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
    ipc.emit("hidden", { session: 37 });
    assert.equal(panel.stack.length, 2);
    ipc.emit("shown", shown({ session: 38 }));
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
    panel.untake();
    assert.deepEqual(panel.stack.map((item) => item.id), [101]);
    const starting = panel.startStackQueue(false, "paste");
    assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 38, ids: [101], automatic: false, mode: "paste" });
    assert.equal(panel.stack.length, 1, "IPC resolve 之前不能先清 stack");
    // 事件故意先于 startQueue 的 Promise continuation 到达，回归早到事件被漏掉的竞态。
    ipc.emit("queue", { session: 38, remaining: 1, total: 1, automatic: false, busy: false });
    assert.equal(panel.stack.length, 0);
    await starting;
  });

  await suite.test("旧会话或参数不匹配的队列事件不能清栈，真正确认仍可随后到达", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.stack = items().slice(0, 2);
    const starting = panel.startStackQueue(true, "paste");
    assert.equal(ipc.calls.at(-1)?.command, "startQueue");
    for (const event of [
      { session: 36, remaining: 2, total: 2, automatic: true, busy: false },
      { session: 37, remaining: 1, total: 1, automatic: true, busy: false },
      { session: 37, remaining: 2, total: 2, automatic: false, busy: false },
    ]) {
      ipc.emit("queue", event);
      assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
    }
    ipc.emit("queue", { session: 37, remaining: 2, total: 2, automatic: true, busy: true });
    assert.equal(panel.stack.length, 0);
    await starting;
  });

  await suite.test("入队确认只消费提交时的快照，提交后变化的一叠必须保留", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.stack = items().slice(0, 2);
    const starting = panel.startStackQueue(false, "paste");
    assert.equal(ipc.calls.at(-1)?.command, "startQueue");
    panel.stack = [...panel.stack, items()[2]];
    ipc.emit("queue", { session: 37, remaining: 2, total: 2, automatic: false, busy: false });
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102, 103]);
    // 后续进度事件没有待确认快照，也不能把后来的一叠清掉。
    ipc.emit("queue", { session: 37, remaining: 1, total: 2, automatic: false, busy: true });
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102, 103]);
    await starting;
  });

  await suite.test("队列 IPC 失败保留一叠，之后的无关确认不能补清", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.stack = items().slice(0, 2);
    ipc.rejectNextQueue();
    await panel.startStackQueue(true, "paste");
    await flush();
    assert.equal(ipc.calls.at(-1)?.command, "startQueue");
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
    assert.equal(panel.busy, false);
    ipc.emit("queue", { session: 37, remaining: 2, total: 2, automatic: true, busy: false });
    assert.deepEqual(panel.stack.map((item) => item.id), [101, 102]);
  });

  await suite.test("搜索一叠接受普通、Ctrl、Ctrl+Shift Enter，拒绝 Shift+Enter 启动队列", async (context) => {
    for (const action of [
      { key: "Enter", automatic: true, mode: "paste" },
      { key: "Enter", ctrlKey: true, automatic: true, mode: "text" },
      { key: "Enter", ctrlKey: true, shiftKey: true, automatic: false, mode: "paste" },
    ]) {
      await context.test(`${action.ctrlKey ? "Ctrl+" : ""}${action.shiftKey ? "Shift+" : ""}Enter`, async (subcontext) => {
        const { panel } = await activatedSearch(subcontext, PanelState, ipc);
        panel.stack = items().slice(0, 2);
        keydown(panel, action); keyup(panel, action);
        if (action.ctrlKey) keyup(panel, { key: "Control", code: "ControlLeft", shiftKey: action.shiftKey });
        if (action.shiftKey) keyup(panel, { key: "Shift", code: "ShiftLeft" });
        assert.deepEqual(ipc.calls.at(-1), { command: "startQueue", session: 37, ids: [101, 102], automatic: action.automatic, mode: action.mode, selection: { revision: 3, nonce: 1 } });
      });
    }
    await context.test("Shift+Enter", async (subcontext) => {
      const { panel } = await activatedSearch(subcontext, PanelState, ipc);
      panel.stack = items().slice(0, 2); panel.selected = 2;
      const action = { key: "Enter", shiftKey: true };
      keydown(panel, action); keyup(panel, action);
      keyup(panel, { key: "Shift", code: "ShiftLeft" });
      assert.deepEqual(ipc.calls, [], "有已选时 Shift+Enter 不执行，也不启动队列");
      assert.match(panel.notice, /不会丢掉它们/);
      assert.equal(panel.stack.length, 2);
    });
  });

  await suite.test("首尾边界：扇面 PgUp 不动，摊开首条 PgUp、末条 PgDn 不动", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick"; panel.selected = 0;
    ipc.emit("key", { session: 37, key: "PageUp" });
    assert.equal(panel.selected, 0);
    assert.equal(panel.pose, "fan");
    ipc.emit("key", { session: 37, key: "PageDown" });
    assert.equal(panel.pose, "spread");
    assert.equal(panel.selected, 7);
    ipc.emit("key", { session: 37, key: "PageDown" });
    assert.equal(panel.selected, 8, "翻一屏但不越过最后一条");
    ipc.emit("key", { session: 37, key: "PageDown" });
    assert.equal(panel.selected, 8, "末条 PgDn 不动");
    ipc.emit("key", { session: 37, key: "End" });
    assert.equal(panel.selected, 8);
    ipc.emit("key", { session: 37, key: "PageUp" });
    ipc.emit("key", { session: 37, key: "PageUp" });
    assert.equal(panel.selected, 0);
    ipc.emit("key", { session: 37, key: "PageUp" });
    assert.equal(panel.selected, 0, "首条 PgUp 不动");
    assert.equal(panel.pose, "spread", "PgUp 不收回");
    // 不足 7 张时扇面 PgDn 没有第 8 条可去，不动。
    panel.collapse();
    panel.history = { ...panel.history, items: items().slice(0, 5) };
    ipc.emit("shown", shown({ session: 38, history: { ...panel.history, items: items().slice(0, 5) } }));
    ipc.emit("key", { session: 38, key: "PageDown" });
    assert.equal(panel.pose, "fan");
  });

  await suite.test("已选的牌不响应数字和单击单独取用；删除时同步移出已选", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick"; panel.selected = 0;
    panel.takeCurrent(); panel.selected = 0;
    await panel.choose(0, "paste");
    panel.chooseByMouse(0, click());
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls, []);
    assert.match(panel.notice, /已在已选里/);
    ipc.emit("key", { session: 37, key: "Delete:down" });
    await flush();
    assert.deepEqual(ipc.calls, [{ command: "deleteItem", id: 101 }]);
    assert.equal(panel.stack.length, 0, "删掉的牌从已选里移除");
  });

  await suite.test("start_queue 返回的中文原因原样显示，不当成通用失败", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.stack = items().slice(0, 2);
    ipc.rejectNextQueue("逐张放不支持“只复制”模式");
    await panel.startStackQueue(false, "paste");
    await flush();
    assert.equal(panel.message, "逐张放不支持“只复制”模式");
    assert.equal(panel.busy, false);
    assert.equal(panel.stack.length, 2);
  });

  await suite.test("避让外形：扇面 / 摊开各自固定，不随选中变化；拖牌时不带；没有手牌画面时为 null", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick"; panel.selected = 0;
    const fan = { left: 28, top: 27, width: 929, height: 502 };
    assert.deepEqual({ ...panel.avoidOutline }, fan);
    for (const index of [3, 6]) { panel.selected = index; assert.deepEqual({ ...panel.avoidOutline }, fan, `拈起第 ${index + 1} 张时外形不变`); }
    panel.spreadOpen(0);
    assert.deepEqual({ ...panel.avoidOutline }, { left: 96, top: 0, width: 648, height: 540 });
    panel.selected = 8;
    assert.deepEqual({ ...panel.avoidOutline }, { left: 96, top: 0, width: 648, height: 540 });
    panel.dragging = true;
    assert.equal(panel.avoidOutline, undefined, "拖牌期间不带 avoid");
    panel.dragging = false;
    ipc.emit("hidden", { session: 37 });
    assert.equal(panel.avoidOutline, null);
  });

  await suite.test("逐张放异常停止：小指示停留显示原因，没放出去的下次呼出放回已选并提示", async (context) => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.stack = [];
    ipc.emit("hidden", { session: 37, queue: true });
    const stop = { session: 37, remaining: 0, total: 4, automatic: false, busy: false, error: "两分钟没有放牌，逐张放已停止；请重新选择剩余条目。", stopped: "idle_timeout", remaining_ids: [103, 999, 101] };
    ipc.emit("queue", stop);
    ipc.emit("hidden", { session: 37 });
    assert.equal(panel.queue?.error, stop.error, "收起后小指示仍显示原因");
    context.mock.timers.tick(3300);
    assert.equal(panel.queue, null, "约 3 秒后消失");
    ipc.emit("shown", shown({ session: 38 }));
    assert.deepEqual(panel.stack.map((item) => item.id), [103, 101], "按原顺序放回，过滤已不存在的");
    assert.match(panel.notice, /上次还剩 2 张没粘贴，已放回已选/);
  });

  for (const stopped of ["done", "cancelled", "escape"]) {
    await suite.test(`逐张放正常结束（${stopped}）不放回、不提示`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      panel.stack = [];
      ipc.emit("queue", { session: 37, remaining: 0, total: 2, automatic: false, busy: false, error: null, stopped, remaining_ids: [101] });
      assert.equal(panel.queue, null);
      ipc.emit("hidden", { session: 37 });
      ipc.emit("shown", shown({ session: 38 }));
      assert.equal(panel.stack.length, 0);
      assert.equal(panel.notice, "");
    });
  }

  await suite.test("已选跨呼出保留时，呼出给轻提示", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.stack = items().slice(0, 2);
    ipc.emit("hidden", { session: 37 });
    ipc.emit("shown", shown({ session: 38 }));
    assert.match(panel.notice, /还有 2 张已选/);
  });

  await suite.test("收起后清掉手牌画面，下一次 panel:shown 前不留旧牌", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.mode = "quick";
    panel.spreadOpen(3);
    assert.ok(panel.fanItems.length > 0);
    ipc.emit("hidden", { session: 37 });
    assert.equal(panel.visible, false);
    assert.equal(panel.history.items.length, 0);
    assert.equal(panel.fanItems.length, 0);
    assert.equal(panel.handSource.length, 0);
    assert.equal(panel.pose, "fan");
    assert.equal(panel.query, "");
    ipc.emit("shown", shown({ session: 38, history: { items: items().slice(0, 3), total: 3, recording: true, last_recorded_ms: null, store_status: null } }));
    assert.deepEqual(panel.fanItems.map((item) => item.id), [101, 102, 103]);
  });

  await suite.test("已选第 33 张在前端拦住并给出明确提示", async (context) => {
    const many = Array.from({ length: 40 }, (_, index) => ({ ...items()[0], id: 101 + index }));
    const history = { items: many, total: many.length, recording: true, last_recorded_ms: null, store_status: null };
    const { panel } = await createPanel(context, PanelState, ipc, { history });
    panel.mode = "quick";
    panel.spreadOpen(0);
    for (let index = 0; index < 32; index++) panel.takeCurrent();
    assert.equal(panel.stack.length, 32);
    panel.selected = 32; panel.takeCurrent();
    assert.equal(panel.stack.length, 32);
    assert.match(panel.notice, /最多选 32 张/);
    assert.deepEqual(ipc.calls, []);
  });

  await suite.test("搜索 Esc 先清空搜索词，空搜索词时再按才收起", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc);
    panel.query = "待清空";
    keydown(panel, { key: "Escape" });
    assert.equal(panel.query, "");
    assert.equal(panel.mode, "search");
    assert.deepEqual(ipc.calls, []);
    keydown(panel, { key: "Escape" });
    assert.deepEqual(ipc.calls, [{ command: "hidePanel", session: 37 }]);
  });
});

test("九种牌面判定与网址拆分（纯函数）", async (suite) => {
  const { cards, cleanup } = await compilePanel();
  suite.after(cleanup);
  const text = (preview) => ({ kind: "文本", preview, file_names: [] });
  const cases = [
    ["short", text("周四下午三点过设计稿")],
    ["short", text("收到，谢谢！")],
    ["long", text("本周进展\n手牌改成每手七张，牌摆得更平。")],
    ["long", text("好的，那我先把初稿发你，有空帮忙看看，晚点再对一下细节。")],
    ["link", text("https://github.com/eiaao/nianlai/pull/31")],
    ["link", text("www.zhihu.com/question/20297180")],
    ["code", text("const hand = await openHand({\n  near: caret,\n});")],
    ["code", text("git pull --rebase")],
    ["image", { kind: "图片", preview: "图片 1280×720", file_names: [] }],
    ["color", text("#C8432B")],
    ["color", text("rgb(58, 110, 165)")],
    ["color", text("hsl(9, 65%, 48%)")],
    ["file", { kind: "文件", preview: "", file_names: ["设计稿-v10.pdf", "图标源文件.zip"] }],
    ["mail", text("hugh@nianlai.app")],
    ["number", text("138 0000 1234")],
    ["number", text("6222 0210 0112 3456 789")],
    ["number", text("91310000MA1234567X")],
    ["number", text("2026-1108-0031")],
    ["short", text("")],
    ["long", { kind: "富文本", preview: "第一行\n第二行", file_names: [] }],
  ];
  for (const [kind, item] of cases) {
    await suite.test(`${kind}：${item.preview || item.file_names.join("、") || "（空）"}`, () => {
      assert.equal(cards.cardKind(item), kind);
    });
  }
  await suite.test("不是颜色、网址、邮箱、号码的近似文本", () => {
    for (const value of ["#C8432", "rgb(1,2)", "github", "a@b", "12345", "第 3 版", "hello world"]) {
      assert.ok(["short", "long"].includes(cards.cardKind(text(value))), value);
    }
  });
  await suite.test("相似网址：末两段墨色、前缀淡色，彼此可区分", () => {
    const parts = [
      "https://github.com/eiaao/nianlai/pull/31",
      "https://github.com/eiaao/nianlai/pull/32",
      "https://github.com/eiaao/nianlai/issues/31",
      "https://github.com/eiaao/nianlai/pull/31/files",
      "https://github.com/tauri-apps/tauri/issues/31",
    ].map((url) => cards.linkParts(url));
    assert.deepEqual(parts.map((part) => part.host), Array(5).fill("github.com"));
    assert.deepEqual(parts.map((part) => part.tail), ["pull/31", "pull/32", "issues/31", "31/files", "issues/31"]);
    assert.deepEqual(parts.map((part) => part.prefix), ["/eiaao/nianlai/", "/eiaao/nianlai/", "/eiaao/nianlai/", "/eiaao/nianlai/pull/", "/tauri-apps/tauri/"]);
    assert.equal(new Set(parts.map((part) => part.prefix + part.tail)).size, 5);
    assert.deepEqual(cards.linkParts("https://www.figma.com/"), { href: "https://www.figma.com/", host: "figma.com", prefix: "", tail: "" });
    assert.equal(cards.linkParts("https://a.com/x?q=1 y"), null);
  });
  await suite.test("避让外形的数值与推导", () => {
    assert.equal(cards.HAND_TOP, 285);
    assert.deepEqual(cards.fanOutline(), { left: 112 - 84, top: 285 - 30 - 212 - 16, width: 828 + 104 + 9 + 16 - 28, height: 285 + 244 - 27 });
    assert.deepEqual(cards.fanOutline(), { left: 28, top: 27, width: 929, height: 502 });
    assert.deepEqual(cards.spreadOutline(), { left: 96, top: 0, width: 648, height: 540 });
    assert.deepEqual(cards.outlineFor("spread"), cards.spreadOutline());
    // 扇面外形必须盖住拈起最大高度的第 1 张、牌堆和常用露头。
    const lifted = cards.fanBox(0, 0, cards.LIFT_MAX_H);
    const fan = cards.fanOutline();
    assert.ok(fan.top <= lifted.y && fan.left <= lifted.x);
    assert.ok(fan.left + fan.width >= cards.DECK_X + cards.DECK_W);
    assert.ok(fan.top <= cards.backBox(3).y);
  });
  await suite.test("颜色值换算：rgb 输入附 HEX，HSL 正确", () => {
    const color = cards.colorValue("rgb(200, 67, 43)");
    assert.equal(color.hex, "#C8432B");
    assert.deepEqual(color.hsl, [9, 65, 48]);
    assert.deepEqual(cards.colorValue("#fff").rgb, [255, 255, 255]);
  });
  await suite.test("拈起高度随内容：短内容不加高，最大 212", () => {
    assert.equal(cards.liftHeight(text("短")), 176);
    const tall = cards.liftHeight(text("x\n".repeat(20)));
    assert.ok(tall > 176 && tall <= 212, "长文加高，但不超过 212");
    assert.equal(cards.liftHeight({ kind: "图片", preview: "图片 1280×720", file_names: [] }), 212);
    const box = cards.fanBox(6, 6, 200);
    assert.equal(box.x + box.w, cards.ORIGIN_X + cards.HAND_W, "第 7 张右对齐，不越出手牌右边界");
    assert.equal(cards.fanBox(0, 0, 200).x, cards.ORIGIN_X, "第 1 张左对齐");
    assert.equal(box.w, 248);
  });
  await suite.test("摊开版面：选中项总在可视区，顶边不切半截，看得见的行连续", () => {
    const many = Array.from({ length: 40 }, (_, index) => ({ ...text(`第 ${index} 条`), id: index, copied_ms: 0 }));
    let scroll = 0;
    for (const selected of [0, 5, 12, 30, 39, 20, 3]) {
      const layout = cards.spreadLayout(many, selected, scroll, () => "更早");
      scroll = layout.scroll;
      assert.ok(layout.visible.includes(selected), `选中 ${selected} 可见`);
      for (let index = 1; index < layout.visible.length; index++) assert.equal(layout.visible[index], layout.visible[index - 1] + 1);
      const blocks = [...layout.heads.map((head) => ({ y: head.y, h: cards.HEAD_H })), ...layout.rows];
      assert.ok(blocks.every((block) => !(block.y < scroll - 0.5 && block.y + block.h > scroll + 0.5)), `顶边不切半截（选中 ${selected}）`);
    }
  });
  await suite.test("时间分组", () => {
    const now = new Date(2026, 10, 12, 14, 32).getTime();
    assert.equal(cards.timeGroup(now - 5 * 60_000, now), "刚才");
    assert.equal(cards.timeGroup(new Date(2026, 10, 12, 9, 0).getTime(), now), "今天上午");
    assert.equal(cards.timeGroup(new Date(2026, 10, 12, 12, 30).getTime(), now), "今天下午");
    assert.equal(cards.timeGroup(new Date(2026, 10, 11, 20, 0).getTime(), now), "昨天");
    assert.equal(cards.timeGroup(new Date(2026, 10, 10, 20, 0).getTime(), now), "这周早些时候");
    assert.equal(cards.timeGroup(new Date(2026, 10, 1).getTime(), now), "更早");
    assert.equal(cards.relativeTime(new Date(2026, 10, 11, 14, 51).getTime(), now), "昨天 14:51");
  });
});
