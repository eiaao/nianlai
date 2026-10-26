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

async function compilePanel() {
  const temporaryBase = resolve(tmpdir());
  const temporary = mkdtempSync(join(temporaryBase, "nianlai-panel-key-repeat-"));
  const ipcPath = join(temporary, "ipc.mjs");
  const compiledPath = join(temporary, "state.svelte.mjs");
  const cleanup = () => {
    assert.equal(dirname(temporary), temporaryBase);
    assert.ok(basename(temporary).startsWith("nianlai-panel-key-repeat-"));
    // 只删除本次创建的两个文件和空目录，不递归清理任何路径。
    for (const path of [compiledPath, ipcPath]) if (existsSync(path)) unlinkSync(path);
    rmdirSync(temporary);
    assert.equal(existsSync(temporary), false);
  };
  try {
    writeFileSync(ipcPath, `
export const calls = [];
let rejectNext = false;
let pendingPaste = null;
const handlers = new Map();
export function reset() { calls.length = 0; rejectNext = false; pendingPaste = null; handlers.clear(); }
export function rejectNextPaste() { rejectNext = true; }
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
export async function retryActivation(session) { calls.push({ command: "retryActivation", session }); }
export async function startSearch(session) { calls.push({ command: "startSearch", session }); }
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
    ]);
    const compiled = compileModule(stripped, { filename: "state.svelte.js", generate: "client", dev: false }).js.code
      .replace(/(from\s+)(["'])(svelte\/internal\/client|svelte|\.\/ipc)\2/g,
        (_, prefix, _quote, specifier) => prefix + JSON.stringify(imports.get(specifier)));
    writeFileSync(compiledPath, compiled);
    const { PanelState } = await import(pathToFileURL(compiledPath).href);
    const ipc = await import(pathToFileURL(ipcPath).href);
    return { PanelState, ipc, cleanup };
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

function keydown(panel, options) {
  const event = {
    type: "keydown", isTrusted: true, timeStamp: performance.now(),
    key: options.key, keyCode: options.key === "Enter" ? 13 : 0,
    ctrlKey: false, altKey: false, metaKey: false, shiftKey: false,
    repeat: false, isComposing: false, target: panel.input, defaultPrevented: false,
    ...options,
    preventDefault() { this.defaultPrevented = true; },
  };
  panel.keydown(event);
  return event;
}

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
    document: { activeElement: null },
  })) {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
    restoreGlobals.push(() => { if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name]; });
  }
  ipc.reset();
  const panel = new PanelState();
  panel.input = { focus() { document.activeElement = this; }, blur() { if (document.activeElement === this) document.activeElement = null; } };
  const disconnect = panel.connect();
  context.after(() => { disconnect(); restoreGlobals.forEach((restore) => restore()); });
  await flush();
  ipc.emit("shown", shown(options));
  if (!options.interaction || options.interaction === "quick") panel.mode = "search";
  panel.searchItems = items();
  panel.selected = 1;
  assert.equal(panel.busy, false);
  return {
    panel, advance: (milliseconds) => { now += milliseconds; },
    runFrame: async () => {
      const current = [...frames];
      frames.clear();
      current.forEach(([, callback]) => callback(now));
      await flush();
    },
  };
}

test("搜索提交键的首次按下、自动重复和组合输入保护", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const expectedPaste = (action, nonce = 1) => ({ command: "pasteItem", session: 37, id: 101 + action.index, mode: action.mode, selection: { revision: 3, nonce } });

  for (const action of submissions) {
    await suite.test(`${action.name} 新按下保留原提交模式`, async (context) => {
      const { panel } = await createPanel(context, PanelState, ipc);
      const event = keydown(panel, action);
      assert.equal(event.defaultPrevented, true);
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

  await suite.test("已激活列表仍是 quick，数字由 panel:key 取用且不授予搜索许可", async (context) => {
    const { panel } = await createPanel(context, PanelState, ipc, { interaction: "activation_pending" });
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    assert.equal(panel.mode, "quick");
    assert.equal(panel.filtered.length, 9);
    keydown(panel, { key: "1" });
    assert.equal(panel.mode, "quick");
    assert.deepEqual(ipc.calls, []);
    ipc.emit("key", { session: 37, key: "1" });
    assert.deepEqual(ipc.calls, [{ command: "pasteItem", session: 37, id: 101, mode: "paste" }]);
  });

  await suite.test("activated 可显式进入搜索，重复 activated 不能重置搜索，DOM 选择仍需快照", async (context) => {
    const { panel, runFrame } = await createPanel(context, PanelState, ipc, { interaction: "activation_pending" });
    ipc.emit("interaction", { session: 37, interaction: "activated" });
    panel.searchPointer({ preventDefault() {} });
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
