// Task 13：直接挂载生产 Panel/HandPanel/HandCard/Settings/About/HotkeyEditor 的真实组件回归。
// Tauri core/event/app/window 全部由内存替身提供；不触发任何真实系统动作。
import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createServer as createNetServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { cleanupHeadlessEdge } from "./headlessEdgeCleanup.mjs";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const root = fileURLToPath(new URL("../", import.meta.url));
const temporaryBase = resolve(tmpdir());
const shotDirectory = resolve(root, "internal/design/v11-around/shots/implementation");
mkdirSync(shotDirectory, { recursive: true });
let temporary;
let vite;
let edge;
let cdp;
let baseUrl;
let navigation = 0;
const browserErrors = [];
const edgeStderr = [];
const shots = [];

async function freePort() {
  const server = createNetServer();
  await new Promise((resolve, reject) => server.once("error", reject).listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}
async function waitFor(predicate, description, timeout = 20000) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < timeout) {
    if (browserErrors.length) throw new Error(`浏览器异常：${browserErrors.join("\n")}`);
    try { const value = await predicate(); if (value) return value; }
    catch (error) { lastError = error; }
    await new Promise((resolve) => setTimeout(resolve, 30));
  }
  throw new Error(`等待超时：${description}${lastError ? `；${lastError.message}` : ""}`);
}
class Cdp {
  constructor(url) { this.ws = new WebSocket(url); this.nextId = 0; this.pending = new Map(); }
  async open() {
    await new Promise((resolve, reject) => {
      this.ws.addEventListener("open", resolve, { once: true });
      this.ws.addEventListener("error", reject, { once: true });
    });
    this.ws.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      if (message.id) {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id);
        if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
        else pending.resolve(message.result);
      } else if (message.method === "Runtime.exceptionThrown") {
        browserErrors.push(message.params.exceptionDetails?.exception?.description ?? message.params.exceptionDetails?.text ?? "浏览器异常");
      }
    });
    this.ws.addEventListener("close", () => { for (const pending of this.pending.values()) pending.reject(new Error("CDP 已关闭")); this.pending.clear(); });
  }
  send(method, params = {}) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => { this.pending.set(id, { resolve, reject }); this.ws.send(JSON.stringify({ id, method, params })); });
  }
  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text ?? "浏览器表达式失败");
    return result.result?.value;
  }
  close() { this.ws.close(); }
}
function edgePath() {
  const found = [process.env.EDGE_PATH, "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"].filter(Boolean).find((path) => existsSync(path));
  if (!found) throw new Error("找不到 Edge");
  return found;
}

before(async () => {
  temporary = mkdtempSync(join(temporaryBase, "nianlai-task13-"));
  const vitePort = await freePort();
  const debugPort = await freePort();
  const ipcPath = resolve(root, "tools/task13Ipc.mjs").replace(/\\/g, "/");
  vite = await createServer({
    configFile: false, root, plugins: [svelte()], clearScreen: false, logLevel: "error",
    resolve: { alias: [
      { find: "@tauri-apps/api/core", replacement: ipcPath },
      { find: "@tauri-apps/api/event", replacement: ipcPath },
      { find: "@tauri-apps/api/app", replacement: ipcPath },
      { find: "@tauri-apps/api/window", replacement: ipcPath },
    ] },
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { host: "127.0.0.1", port: vitePort, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/internal/**", "**/target/**"] } },
  });
  await vite.listen();
  baseUrl = `http://127.0.0.1:${vitePort}/tools/task13Test.html`;
  edge = spawn(edgePath(), ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--disable-component-update", "--disable-sync", `--remote-debugging-port=${debugPort}`, `--user-data-dir=${join(temporary, "edge-profile")}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"], windowsHide: true });
  edge.stderr.setEncoding("utf8");
  edge.stderr.on("data", (chunk) => edgeStderr.push(chunk));
  const target = await waitFor(async () => {
    const pages = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
    return pages.find((page) => page.type === "page");
  }, "Edge 调试目标");
  cdp = new Cdp(target.webSocketDebuggerUrl);
  await cdp.open();
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
});
after(async () => {
  if (cdp) {
    await Promise.race([cdp.send("Browser.close").catch(() => {}), new Promise((resolve) => setTimeout(resolve, 3000))]);
    cdp.close();
  }
  await vite?.close();
  await cleanupHeadlessEdge({ edge, temporary, temporaryBase, prefix: "nianlai-task13-", label: "Task13" });
  const stderr = edgeStderr.join("").trimEnd();
  if (stderr) console.error(`\n[Task13 Edge stderr]\n${stderr}\n[/Task13 Edge stderr]`);
});
afterEach(() => { try { assert.deepEqual(browserErrors, []); } finally { browserErrors.length = 0; } });

const settle = () => cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
const api = (expression) => cdp.evaluate(`window.__task13.${expression}`);
const calls = (command) => api(`invokeCalls(${command ? JSON.stringify(command) : ""})`);
const guideIds = () => cdp.evaluate(`[...document.querySelectorAll("[data-guide-id]")].map((node) => node.dataset.guideId)`);
const cardIds = () => cdp.evaluate(`[...document.querySelectorAll(".card[data-card-id]")].map((node) => Number(node.dataset.cardId))`);
const text = (selector = "body") => cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.textContent ?? ""`);
const click = (selector) => cdp.evaluate(`(() => { const node = document.querySelector(${JSON.stringify(selector)}); if (!node) throw new Error("找不到 " + ${JSON.stringify(selector)}); node.click(); })()`);
async function open(view, query = "", hash = "") {
  const id = ++navigation;
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: view === "panel" ? 960 : 1120, height: view === "panel" ? 540 : 680, deviceScaleFactor: 1, mobile: false });
  await cdp.send("Page.navigate", { url: `${baseUrl}?view=${view}&nav=${id}${query ? `&${query}` : ""}#${hash}` });
  await waitFor(() => cdp.evaluate(`location.search.includes("nav=${id}") && !!window.__task13`), `${view} 挂载`);
  if (view === "settings" || view === "pair") await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-loading='false']") !== null`), "设置加载");
  await settle();
}
async function openPanel(query = "", overrides = {}, view = "panel") {
  await open(view, query);
  await waitFor(async () => (await calls("frontend_ready")).length === 1, "面板订阅与预热完成");
  const payload = await api(`shown(${JSON.stringify(overrides)})`);
  await cdp.send("Page.bringToFront");
  await api(`emit("panel:shown", ${JSON.stringify(payload)})`);
  await settle();
  return payload;
}
async function panelKey(key, session) {
  await api(`emit("panel:key", { session: ${session}, key: ${JSON.stringify(key)} })`);
  await settle();
}
const settleModifierHint = () => cdp.evaluate("new Promise((resolve) => setTimeout(resolve, 340))");
async function assertGuideFooter(pose) {
  const hint = await text(".hint");
  for (const key of pose === "fan" ? ["←", "→"] : ["↑", "↓"]) assert.match(hint, new RegExp(key));
  for (const key of ["Delete", "Tab", "Esc"]) assert.match(hint, new RegExp(key));
  assert.doesNotMatch(hint, /只复制|不粘贴|纯文本|设为收藏|多选|一起粘贴|逐张放/);
}
async function spreadT4ErrorVisibility() {
  const state = await cdp.evaluate(`(() => {
    const card = document.querySelector('[data-guide-id="t4"]');
    const row = card?.querySelector('.row');
    const alert = row?.querySelector('[role="alert"]');
    if (!card || !row || !alert) return null;
    let effectiveOpacity = 1;
    for (let node = alert; node; node = node.parentElement) effectiveOpacity *= Number(getComputedStyle(node).opacity);
    const style = getComputedStyle(alert);
    return {
      pose: document.querySelector('.panel-shell')?.dataset.pose,
      rowOpacity: Number(getComputedStyle(row).opacity),
      fanOpacity: Number(getComputedStyle(card.querySelector('.fan')).opacity),
      effectiveOpacity,
      rendered: style.display !== 'none' && style.visibility === 'visible' && alert.getClientRects().length > 0,
      checked: row.querySelector('[data-guide-autostart]')?.checked,
      text: alert.textContent,
    };
  })()`);
  assert.equal(state?.pose, "spread");
  assert.equal(state.rowOpacity, 1, "错误必须位于实际可见的摊开行，不是透明扇面");
  assert.equal(state.fanOpacity, 0);
  assert.equal(state.effectiveOpacity, 1, "错误及全部祖先的有效透明度必须非透明");
  assert.equal(state.rendered, true);
  assert.equal(state.checked, false, "写失败后开关保留回读/请求前的真实 false");
  return state;
}
function guideRequest(page) {
  return new URLSearchParams(String(page).split("?")[1] ?? "").get("request");
}
async function resolveSave(id, silentResponse) {
  if (silentResponse) await api(`resolveSaveSilently(${id}, ${JSON.stringify(silentResponse)})`);
  else await api(`resolveSave(${id})`);
  await settle();
}
async function change(selector, checked) {
  await cdp.evaluate(`(() => { const node = document.querySelector(${JSON.stringify(selector)}); if (!node) throw new Error("找不到控件"); node.checked = ${checked}; node.dispatchEvent(new Event("change", { bubbles: true })); })()`);
  await settle();
}
async function capture(name) {
  await cdp.evaluate("document.fonts.ready");
  await settle();
  const { data } = await cdp.send("Page.captureScreenshot", { format: "png", fromSurface: true, captureBeyondViewport: false });
  const bytes = Buffer.from(data, "base64");
  assert.equal(bytes.subarray(1, 4).toString(), "PNG");
  writeFileSync(resolve(shotDirectory, name), bytes);
  shots.push(name);
}

test("激活式（管理员前台）面板：搜索框拿到焦点后保持挂载，打字能进搜索框", async () => {
  await openPanel("two=1", { interaction: "activated" });
  await waitFor(async () => (await cdp.evaluate("document.activeElement?.matches?.('.search-box input') === true")) === true, "搜索框持有焦点");
  // 准备完成后不能换成另一个输入框：焦点仍在同一个输入框上，提示层消失。
  await settle();
  assert.equal(await cdp.evaluate("document.activeElement?.matches?.('.search-box input') === true"), true);
  assert.equal(await cdp.evaluate("document.querySelectorAll('.search-box input').length"), 1);
  assert.equal(await cdp.evaluate("document.querySelector('.activation') === null"), true);
  await cdp.send("Input.insertText", { text: "abc" });
  await settle();
  assert.equal(await cdp.evaluate("document.querySelector('.search-box input').value"), "abc");
});

test("新手牌只插在最近第一张真实牌之后，空库从第一张开始，dismissed/搜索/常用均隔离", async () => {
  let shown = await openPanel("two=1");
  assert.deepEqual(await cardIds(), [101, -1, -2, -3, -4, 102]);
  assert.deepEqual(await guideIds(), ["t1", "t2", "t3", "t4"]);
  assert.equal((await api("history()")).total, 2);
  assert.match(await text(), /新手牌/);
  assert.match(await text(), /Delete 扔掉/);

  await api(`emit("panel:mode", { session: ${shown.session}, mode: "search" })`);
  await settle();
  await cdp.evaluate(`(() => { const input = document.querySelector('.search-box input'); input.value = "真实"; input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: "实" })); })()`);
  await waitFor(async () => (await calls("search_history")).length > 0, "真实搜索");
  await waitFor(async () => (await guideIds()).length === 0, "搜索视图不包含新手牌");
  assert.deepEqual(await guideIds(), []);
  assert.ok((await cardIds()).every((id) => id > 0));

  await openPanel("favorite=1");
  assert.deepEqual(await guideIds(), [], "只有常用真实牌时不把新手牌混进常用");
  await openPanel("empty=1");
  assert.deepEqual(await cardIds(), [-1, -2, -3, -4]);
  await openPanel("dismissed=t2,t4");
  assert.deepEqual(await guideIds(), ["t1", "t3"]);
});

test("Quick 数字/Enter/左键/中键/copy/text/多选/stack/queue/favorite/delete/Ctrl+Z 对新手牌均为零内容 IPC", async () => {
  const shown = await openPanel();
  await api("clearInvokeCalls()");
  await click(`[data-guide-id="t1"]`);
  assert.match(await text(".statusbar"), /新手牌是说明，贴不出去/);
  await cdp.evaluate(`document.querySelector('[data-guide-id="t1"]').dispatchEvent(new MouseEvent("auxclick", { button: 1, detail: 1, bubbles: true, cancelable: true }))`);
  await settle();
  for (const key of ["2", "Enter", "Shift+Enter", "Ctrl+Enter", "Ctrl+D", "Space", "Ctrl+Shift+Enter", "Tab", "Ctrl+C", "Ctrl+Z"]) await panelKey(key, shown.session);
  const forbidden = new Set(["paste_item", "start_queue", "set_favorite", "delete_item", "undo_delete", "get_full_text", "get_image", "get_file_paths", "copy_selection", "get_history", "get_favorites", "search_history"]);
  const leaked = (await calls()).filter((call) => forbidden.has(call.command));
  assert.deepEqual(leaked, []);
});

test("新手牌 Ctrl+Z 在已有真实删除时也不撤销；真实牌原撤销保持", async () => {
  const shown = await openPanel();
  await panelKey("Delete:down", shown.session);
  await waitFor(async () => (await calls("delete_item")).length === 1, "先制造一条真实删除");
  assert.equal(await api("deletedCount()"), 1);
  await api("clearInvokeCalls()");
  await panelKey("Ctrl+Z", shown.session);
  const forbidden = (await calls()).filter((call) => ["undo_delete", "get_history", "get_favorites"].includes(call.command));
  assert.deepEqual(forbidden, []);
  assert.equal(await api("deletedCount()"), 1, "真实已删除牌不得被新手牌撤销恢复");
  assert.equal((await api("history()")).total, 0);
  assert.match(await text(".statusbar"), /新手牌是说明，贴不出去/);

  const realShown = await openPanel("dealt=0&two=1");
  await panelKey("Delete:down", realShown.session);
  await waitFor(async () => (await calls("delete_item")).length === 1, "真实牌对照删除");
  await api("clearInvokeCalls()");
  await panelKey("Ctrl+Z", realShown.session);
  await waitFor(async () => (await calls("undo_delete")).length === 1, "真实牌仍可撤销");
  assert.equal((await calls("get_history")).length, 1);
  assert.equal((await api("history()")).total, 2);
});

test("Delete 只部分保存 dismissed；保存前留牌、重复按不并发、失败留牌，旧响应不覆盖外部重看状态", async () => {
  const shown = await openPanel("manualSaves=1");
  await panelKey("ArrowRight", shown.session);
  await panelKey("Delete:down", shown.session);
  await waitFor(async () => (await api("pendingSaves()")).length === 1, "新手牌删除保存");
  assert.deepEqual(await guideIds(), ["t1", "t2", "t3", "t4"]);
  assert.deepEqual((await api("saveCalls()"))[0].update, { guide: { dismissed: ["t1"] } });
  await panelKey("Delete", shown.session);
  await panelKey("Delete:down", shown.session);
  assert.equal((await api("saveCalls()")).length, 1);
  await api(`rejectSave(1, "磁盘不让写")`);
  await settle();
  assert.deepEqual(await guideIds(), ["t1", "t2", "t3", "t4"]);
  assert.match(await text(".statusbar"), /没保存上，新手牌还在，再试一次/);
  assert.equal((await calls("delete_item")).length, 0);

  await panelKey("Delete:down", shown.session);
  await waitFor(async () => (await api("pendingSaves()")).length === 1, "第二次删除保存");
  const stale = await api("settings()");
  stale.guide.dismissed = ["t1"];
  await api(`externalGuide({ cards_dealt: false })`);
  await waitFor(async () => (await guideIds()).length === 0, "外部重看设置先移除新手牌");
  assert.deepEqual(await guideIds(), []);
  await resolveSave(2, stale);
  assert.deepEqual(await guideIds(), [], "外部 cards_dealt=false 不得被旧删除响应覆盖");
});

test("挂起期间漏掉通知也能从下一次 panel:shown.guide 恢复，cards_dealt=false 不新增自启/内容读取", async () => {
  let shown = await openPanel("dealt=0");
  assert.deepEqual(await guideIds(), []);
  assert.equal((await calls("get_autostart")).length, 0);
  const settingsReads = (await calls("get_settings")).length;
  await api(`silentlyGuide({ cards_dealt: true })`);
  await api(`emit("panel:hidden", { session: ${shown.session} })`);
  shown = await api("shown()");
  await api(`emit("panel:shown", ${JSON.stringify(shown)})`);
  await settle();
  assert.deepEqual(await guideIds(), ["t1", "t2", "t3", "t4"]);
  assert.equal((await calls("get_settings")).length, settingsReads);
});

test("t3 深链新建/复用都进入生产 HotkeyEditor；Esc/失败/普通录制无信号，成功信号绑定 request", async () => {
  await open("settings", "", "general/hotkey?request=req-new");
  await waitFor(() => cdp.evaluate(`document.querySelector(".recorder")?.classList.contains("recording")`), "新建深链自动录制");
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", bubbles: true, cancelable: true }))`);
  await settle();
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length, 0);
  await api(`emit("settings:navigate", "about")`);
  await settle();
  await api(`emit("settings:navigate", "general/hotkey?request=req-reuse")`);
  await waitFor(() => cdp.evaluate(`document.querySelector(".recorder")?.classList.contains("recording")`), "复用深链自动录制");
  await api(`hotkeyValidationError("组合冲突")`);
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", code: "KeyK", keyCode: 75, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await settle();
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length, 0);
  await api(`hotkeyValidationError(null)`);
  await api("failNextSave()");
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", code: "KeyK", keyCode: 75, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await calls("set_settings")).length > 0, "热键保存失败路径");
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length, 0);
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", code: "KeyK", keyCode: 75, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length === 1, "深链成功信号");
  assert.deepEqual((await api("targetedEvents()")).at(-1).payload, { request: "req-reuse" });
  await api(`emit("settings:navigate", "general/hotkey?request=req-same")`);
  await waitFor(() => cdp.evaluate(`document.querySelector(".recorder")?.classList.contains("recording")`), "同一热键确认的深链录制");
  const savesBeforeSame = (await calls("set_settings")).length;
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", code: "KeyK", keyCode: 75, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length === 2, "同一有效热键确认也发信号");
  assert.equal((await calls("set_settings")).length, savesBeforeSame, "同一热键不重复保存");
  assert.deepEqual((await api("targetedEvents()")).at(-1).payload, { request: "req-same" });
  const before = (await api("targetedEvents()")).length;
  await click(".recorder");
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "j", code: "KeyJ", keyCode: 74, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await calls("set_settings")).length > savesBeforeSame, "普通手动录制保存");
  await waitFor(() => cdp.evaluate(`!document.querySelector(".recorder")?.classList.contains("recording")`), "普通录制结束");
  assert.equal((await api("targetedEvents()")).length, before);
  const savesBeforeManualSame = (await calls("set_settings")).length;
  await click(".recorder");
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "j", code: "KeyJ", keyCode: 74, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await settle();
  assert.equal((await calls("set_settings")).length, savesBeforeManualSame, "普通手动确认同键也不重复保存");
  assert.equal((await api("targetedEvents()")).length, before, "普通手动确认同键零成功信号");
});

test("t3 panel pending 同时校验 session/request；打开失败、hidden、新 shown 和旧/空 request 均失效", async () => {
  let shown = await openPanel();
  await api(`emit("guide:hotkey-saved", {})`);
  assert.equal((await calls("hide_panel")).length, 0);
  await api("failOpenSettings()");
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  await settle();
  await api(`emit("guide:hotkey-saved", {})`);
  assert.equal((await calls("hide_panel")).length, 0);
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  const oldRequest = guideRequest((await calls("open_settings")).at(-1).args.page);
  await api(`emit("panel:hidden", { session: ${shown.session} })`);
  shown = await api("shown()");
  await api(`emit("panel:shown", ${JSON.stringify(shown)})`);
  await api(`emit("guide:hotkey-saved", { request: ${JSON.stringify(oldRequest)} })`);
  assert.equal((await calls("hide_panel")).length, 0);
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  const request = guideRequest((await calls("open_settings")).at(-1).args.page);
  assert.equal(typeof request, "string");
  await api(`emit("guide:hotkey-saved", {})`);
  await api(`emit("guide:hotkey-saved", { request: "旧request" })`);
  assert.equal((await calls("hide_panel")).length, 0);
  await api(`emit("guide:hotkey-saved", { request: ${JSON.stringify(request)} })`);
  await waitFor(async () => (await calls("hide_panel")).length === 1, "当前 session/request 成功后收起");
  assert.equal((await calls("hide_panel"))[0].args.session, shown.session);
});

test("t3 旧保存 continuation 晚于 hidden→新 shown→第二次深链时不得 hide 新 session", async () => {
  const shownA = await openPanel("manualSaves=1", {}, "pair");
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  await waitFor(() => cdp.evaluate(`document.querySelector(".recorder")?.classList.contains("recording")`), "第一次深链录制");
  const requestA = guideRequest((await calls("open_settings")).at(-1).args.page);
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", code: "KeyK", keyCode: 75, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await api("pendingSaves()")).length === 1, "第一次热键保存保持 pending");

  await api(`emit("panel:hidden", { session: ${shownA.session} })`);
  const shownB = await api("shown()");
  await api(`emit("panel:shown", ${JSON.stringify(shownB)})`);
  await settle();
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  await waitFor(async () => (await calls("open_settings")).length === 2, "第二次深链");
  const requestB = guideRequest((await calls("open_settings")).at(-1).args.page);
  await resolveSave(1);
  assert.equal((await calls("hide_panel")).length, 0, "第一次旧 continuation 不得收起 B");
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length, 0, "旧 continuation 不得发成功信号");
  assert.equal(typeof requestA, "string");
  assert.equal(typeof requestB, "string");
  assert.notEqual(requestA, requestB, "每次深链都有独立不透明 request");

  await api("useManualSaves(false)");
  await waitFor(() => cdp.evaluate(`document.querySelector(".recorder")?.classList.contains("recording")`), "第二次深链独立录制不被旧 busy 卡住");
  await cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", { key: "j", code: "KeyJ", keyCode: 74, ctrlKey: true, bubbles: true, cancelable: true }))`);
  await waitFor(async () => (await api("targetedEvents()")).filter((event) => event.name === "guide:hotkey-saved").length === 1, "第二次独立成功");
  assert.deepEqual((await api("targetedEvents()")).at(-1).payload, { request: requestB });
  await waitFor(async () => (await calls("hide_panel")).length === 1, "只有第二次成功才收起 B");
  assert.equal((await calls("hide_panel"))[0].args.session, shownB.session);
});

test("t4 只在实际显示时读真实 autostart；写后读、失败回读、回读失败和陈旧响应均不乐观勾选", async () => {
  let shown = await openPanel("dealt=0");
  assert.equal((await calls("get_autostart")).length, 0);
  shown = await openPanel();
  await waitFor(async () => (await calls("get_autostart")).length === 1, "t4 实际显示读取");
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, true);
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]')?.checked === true`), "写后读开启");
  await api(`autostartScenario({ writeError: "写失败", readErrors: 1 })`);
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, false);
  await waitFor(async () => (await calls("set_autostart")).length === 2, "写失败回读失败");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]').checked`), true);
  assert.match(await text(`[data-guide-id="t4"]`), /写失败/);

  await openPanel("manualSaves=1");
  await waitFor(async () => (await calls("get_autostart")).length === 1, "新场景 t4 读取");
  await api(`deferAutostart({ set: true })`);
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, true);
  await waitFor(async () => (await api("pendingAutostart()")).set === 1, "deferred 自启写");
  const current = await cdp.evaluate(`document.querySelector("[data-guide-id='t4'] [data-guide-autostart]").checked`);
  await api(`emit("panel:hidden", { session: ${shown.session} })`);
  await api("resolveAutostartSet(true)");
  await settle();
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-id='t4'] [data-guide-autostart]")`), null);
  assert.equal(current, false, "保存前不乐观勾选");
});

test("t4 写失败可回读、重开后的陈旧响应与卸载都不会改写真实状态", async () => {
  await openPanel();
  await waitFor(async () => (await calls("get_autostart")).length === 1, "失败回读场景初次读取");
  await api(`autostartScenario({ writeError: "写失败但可回读" })`);
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, true);
  await waitFor(async () => (await calls("get_autostart")).length === 2, "写失败后成功回读");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]').checked`), false);
  assert.match(await text(`[data-guide-id="t4"]`), /写失败但可回读/);

  const stale = await openPanel();
  await waitFor(async () => (await calls("get_autostart")).length === 1, "陈旧响应场景初次读取");
  await api(`deferAutostart({ set: true })`);
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, true);
  await waitFor(async () => (await api("pendingAutostart()")).set === 1, "陈旧自启写已进入");
  await api(`emit("panel:hidden", { session: ${stale.session} })`);
  const reopened = await api("shown()");
  await api(`emit("panel:shown", ${JSON.stringify(reopened)})`);
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]')?.disabled === false`), "重开后真实状态已读");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]').checked`), false);
  await api("resolveAutostartSet(true)");
  await settle();
  assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-id="t4"] [data-guide-autostart]').checked`), false, "旧 true 响应不得覆盖重开后的 false");

  await openPanel();
  await waitFor(async () => (await calls("get_autostart")).length === 1, "卸载场景初次读取");
  await api(`deferAutostart({ set: true })`);
  await change(`[data-guide-id="t4"] [data-guide-autostart]`, true);
  await waitFor(async () => (await api("pendingAutostart()")).set === 1, "卸载前自启写已进入");
  await api("destroy()");
  await api("resolveAutostartSet(true)");
  await settle();
});

test("About 两动作、三进行中 phase、保存/通知 pending 防重及 open 失败跨重挂恢复", async () => {
  for (const phase of ["completed", "skipped"]) {
    await open("settings", `phase=${phase}&manualSaves=1&dismissed=t2`, "about");
    await click(`[data-guide-action="replay"]`);
    await click(`[data-guide-action="replay"]`);
    assert.equal((await calls("open_guide")).length, 0);
    assert.equal((await api("saveCalls()")).length, 1);
    assert.deepEqual((await api("saveCalls()"))[0].update, { guide: { version: 1, phase: "await_copy", baseline_ms: null, baseline_total: 0, sample_id: null, cards_dealt: false } });
    await resolveSave(1);
    assert.equal((await calls("open_guide")).length, 1);
    assert.deepEqual((await api("settings()")).guide.dismissed, ["t2"]);

    await open("settings", `phase=${phase}&manualSaves=1&dismissed=t2`, "about");
    await click(`[data-guide-action="deal"]`);
    await click(`[data-guide-action="deal"]`);
    assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 0);
    assert.equal((await api("saveCalls()")).length, 1, "deal 保存 pending 快速双击只保存一次");
    assert.deepEqual((await api("saveCalls()"))[0].update, { guide: { cards_dealt: true, dismissed: [] } });
    await resolveSave(1);
    assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 1);
    assert.equal((await api("settings()")).guide.phase, phase);
  }
  for (const phase of ["await_copy", "await_show", "await_paste"]) {
    await open("settings", `phase=${phase}`, "about");
    for (const action of ["replay", "deal"]) {
      assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-action="${action}"]').disabled`), true);
      await click(`[data-guide-action="${action}"]`);
    }
    assert.match(await text(".about-page"), /首次启动结束后可以再看一次或再发新手牌。/);
    assert.equal((await calls("set_settings")).length, 0, `${phase} 零保存`);
    assert.equal((await calls("open_guide")).length, 0);
    assert.equal((await api("targetedEvents()")).filter((event) => event.name.startsWith("guide:")).length, 0);
  }

  await open("settings", "phase=completed", "about");
  await api("deferGuideNotification()");
  await click(`[data-guide-action="deal"]`);
  await click(`[data-guide-action="deal"]`);
  await waitFor(async () => (await api("pendingGuideNotifications()")) === 1, "deal 通知 pending");
  assert.equal((await calls("set_settings")).length, 1, "通知 pending 快速双击不重复保存");
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 1, "通知 pending 快速双击只通知一次");
  await api("resolveGuideNotification()");
  await settle();
  assert.match(await text(".about-page"), /新手牌已重新发出/);

  await open("settings", "phase=completed", "about");
  await api("failNextSave()");
  await click(`[data-guide-action="replay"]`);
  await settle();
  assert.equal((await calls("open_guide")).length, 0);
  assert.match(await text(".about-page"), /模拟设置保存失败/);
  await api("failOpenGuide()");
  await click(`[data-guide-action="replay"]`);
  await waitFor(async () => (await calls("open_guide")).length === 1, "保存成功后 open 失败");
  assert.match(await text(".about-page"), /设置已保存，但窗口没打开，请再试一次/);
  const savesBeforeRetry = (await calls("set_settings")).length;
  await api(`emit("settings:navigate", "general")`);
  await waitFor(() => cdp.evaluate(`!!document.querySelector(".general-page")`), "离开 About");
  await api(`emit("settings:navigate", "about")`);
  await waitFor(() => cdp.evaluate(`!!document.querySelector('[data-guide-action="open"]')`), "返回 About 仍有独立恢复入口");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-guide-action="open"]').disabled`), false);
  await api("deferOpenGuide()");
  await click(`[data-guide-action="open"]`);
  await click(`[data-guide-action="open"]`);
  await waitFor(async () => (await api("pendingOpenGuide()")) === 1, "重试 open pending 防重");
  assert.equal((await calls("open_guide")).length, 2);
  await api("resolveOpenGuide()");
  await settle();
  assert.equal((await calls("set_settings")).length, savesBeforeRetry, "恢复只重试 open，不重复写 phase");
  assert.equal((await calls("open_guide")).length, 2);
  assert.equal(await cdp.evaluate(`!!document.querySelector('[data-guide-action="open"]')`), false);
  assert.match(await text(".about-page"), /首次启动已重新打开/);

  await open("settings", "phase=completed", "about");
  await api("failNotify()");
  await click(`[data-guide-action="deal"]`);
  await waitFor(async () => (await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length === 1, "保存成功后 notify 失败");
  assert.match(await text(".about-page"), /新手牌已保存，下次呼出会看到/);

  await open("settings", "phase=completed", "about");
  await api("failNextSave()");
  await click(`[data-guide-action="deal"]`);
  await settle();
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 0, "deal 保存失败不得通知");
  assert.match(await text(".about-page"), /模拟设置保存失败/);
});

test("About deal 通知 pending 跨 general/about 重挂仍防重", async () => {
  await open("settings", "phase=completed", "about");
  await api("deferGuideNotification()");
  await click(`[data-guide-action="deal"]`);
  await waitFor(async () => (await api("pendingGuideNotifications()")) === 1, "第一次 deal 保存完成且通知 pending");
  assert.equal((await calls("set_settings")).length, 1);
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 1);
  await cdp.evaluate(`window.__task13FirstAbout = document.querySelector(".about-page")`);

  await api(`emit("settings:navigate", "general")`);
  await waitFor(() => cdp.evaluate(`!!document.querySelector(".general-page")`), "deal 通知 pending 时离开 About");
  await api(`emit("settings:navigate", "about")`);
  await waitFor(() => cdp.evaluate(`!!document.querySelector('[data-guide-action="deal"]')`), "deal 通知 pending 时返回 About");
  assert.equal(await cdp.evaluate(`window.__task13FirstAbout?.isConnected === false && window.__task13FirstAbout !== document.querySelector(".about-page")`), true, "生产导航确实销毁并重挂 About");
  await cdp.evaluate(`delete window.__task13FirstAbout`);

  await click(`[data-guide-action="deal"]`);
  await settle();
  const pendingState = {
    saves: (await calls("set_settings")).length,
    notifications: (await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length,
    pending: await api("pendingGuideNotifications()"),
    dealDisabled: await cdp.evaluate(`document.querySelector('[data-guide-action="deal"]').disabled`),
    replayDisabled: await cdp.evaluate(`document.querySelector('[data-guide-action="replay"]').disabled`),
  };
  assert.deepEqual(pendingState, { saves: 1, notifications: 1, pending: 1, dealDisabled: true, replayDisabled: true });

  await api("resolveGuideNotification()");
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-guide-action="deal"]').disabled === false && document.querySelector('[data-guide-action="replay"]').disabled === false`), "第一次通知完成后两个动作恢复可用");
  assert.equal((await calls("set_settings")).length, 1);
  assert.equal((await api("targetedEvents()")).filter((event) => event.name === "guide:cards-dealt").length, 1);
  assert.equal(await api("pendingGuideNotifications()"), 0);
});

test("fan/spread × default/Shift/Ctrl 页脚只提示真实可用动作，spread t4 显示失败并留最终帧", async () => {
  let shown = await openPanel();
  await panelKey("ArrowRight", shown.session);
  await assertGuideFooter("fan");
  await panelKey("Mod:Shift", shown.session);
  await settleModifierHint();
  await assertGuideFooter("fan");
  await panelKey("Mod:Ctrl", shown.session);
  await settleModifierHint();
  await assertGuideFooter("fan");
  await panelKey("Mod:", shown.session);
  await panelKey("ArrowDown", shown.session);
  await assertGuideFooter("spread");
  await panelKey("Mod:Shift", shown.session);
  await settleModifierHint();
  await assertGuideFooter("spread");
  await panelKey("Mod:Ctrl", shown.session);
  await settleModifierHint();
  await assertGuideFooter("spread");

  shown = await openPanel("dealt=0");
  assert.match(await text(".hint"), /粘贴/);
  assert.match(await text(".hint"), /多选/);
  await panelKey("Mod:Shift", shown.session);
  await settleModifierHint();
  assert.match(await text(".hint"), /只复制这一张/);
  await panelKey("Mod:Ctrl", shown.session);
  await settleModifierHint();
  assert.match(await text(".hint"), /纯文本/);
  assert.match(await text(".hint"), /设为收藏/);

  shown = await openPanel();
  await panelKey("Space", shown.session);
  assert.match(await text(".hint"), /一起粘贴/);
  assert.match(await text(".hint"), /逐张放/);

  shown = await openPanel();
  for (let index = 0; index < 4; index += 1) await panelKey("ArrowRight", shown.session);
  await panelKey("ArrowDown", shown.session);
  await waitFor(async () => (await calls("get_autostart")).length === 1, "spread t4 初次读取");
  await waitFor(() => cdp.evaluate(`(() => {
    const card = document.querySelector('[data-guide-id="t4"]');
    return Number(getComputedStyle(card.querySelector('.row')).opacity) === 1
      && Number(getComputedStyle(card.querySelector('.fan')).opacity) === 0;
  })()`), "spread t4 行面完成淡入、扇面完成淡出");

  await api(`autostartScenario({ writeError: "摊开写失败" })`);
  await change(`[data-guide-id="t4"] .row [data-guide-autostart]`, true);
  await waitFor(async () => (await calls("get_autostart")).length === 2, "spread t4 写失败后成功回读");
  await waitFor(async () => (await text(`[data-guide-id="t4"] .row [role="alert"]`)).includes("摊开写失败"), "spread 行内写失败原因");
  assert.match((await spreadT4ErrorVisibility()).text, /摊开写失败/);
  await capture("task13-spread-t4-error.png");

  await api(`autostartScenario({ writeError: "摊开写失败且回读失败", readErrors: 1 })`);
  await change(`[data-guide-id="t4"] .row [data-guide-autostart]`, true);
  await waitFor(async () => (await calls("get_autostart")).length === 3, "spread t4 写失败且回读失败");
  await waitFor(async () => (await text(`[data-guide-id="t4"] .row [role="alert"]`)).includes("摊开写失败且回读失败"), "spread 回读失败仍显示写失败原因");
  assert.match((await spreadT4ErrorVisibility()).text, /摊开写失败且回读失败/);
});

test("Mac 真实组件按准确键名渲染静态提示，后端英文错误保持原文", async () => {
  const userAgent = await cdp.evaluate("navigator.userAgent");
  await cdp.send("Emulation.setUserAgentOverride", { userAgent: "Macintosh" });
  try {
    const shown = await openPanel();
    assert.match(await text(), /⌫ 扔掉/);
    assert.match(await text('[data-guide-id="t3"]'), /⌃`/);
    await api(`emit("panel:paste-result", ${JSON.stringify({ session: shown.session, paste_result: "copied_only", reason: "mac_accessibility_required", hint_dwell_ms: 3000 })})`);
    await settle();
    assert.match(await text(), /按 ⌘V 贴上。打开辅助功能，就能直接贴进去/);
    const fresh = await openPanel();
    await api(`emit("panel:error", ${JSON.stringify({ session: fresh.session, message: "Error: Return Home end" })})`);
    await settle();
    assert.match(await text(), /Error: Return Home end/);
  } finally {
    await cdp.send("Emulation.setUserAgentOverride", { userAgent });
  }
});

test("输出真实 Svelte 最终帧：真实第一张+四张新手牌、t3、t4、About 可用与失败", async () => {
  await openPanel();
  await capture("task13-fan-five-cards.png");
  await click(`[data-guide-id="t3"] [data-guide-hotkey]`);
  assert.equal(typeof guideRequest((await calls("open_settings")).at(-1).args.page), "string");
  let shown = await openPanel();
  for (let index = 0; index < 3; index += 1) await panelKey("ArrowRight", shown.session);
  await capture("task13-t3.png");
  shown = await openPanel();
  for (let index = 0; index < 4; index += 1) await panelKey("ArrowRight", shown.session);
  await capture("task13-t4.png");
  await open("settings", "phase=completed", "about");
  await capture("task13-about-ready.png");
  await api("failNextSave()");
  await click(`[data-guide-action="deal"]`);
  await settle();
  await capture("task13-about-failed.png");
  assert.equal(shots.length, 6);
});
