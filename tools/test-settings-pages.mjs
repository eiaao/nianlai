// 仓库根目录运行：node tools/test-settings-pages.mjs
// Vite 编译并挂载真实 Svelte 设置组件；Edge 只访问本机临时服务器，IPC 全部是内存替身。
import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createServer as createNetServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { cleanupHeadlessEdge } from "./headlessEdgeCleanup.mjs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const root = fileURLToPath(new URL("../", import.meta.url));
const temporaryBase = resolve(tmpdir());
let temporary;
let vite;
let edge;
let cdp;
let pageUrl;
let navigationId = 0;
let scenarioId = 0;
const executionContexts = [];
const browserErrors = [];
const edgeStderr = [];
const shotDirectory = resolve(root, "internal/design/v11-around/shots/implementation");
const shotManifest = [];
mkdirSync(shotDirectory, { recursive: true });

async function freePort() {
  const server = createNetServer();
  await new Promise((resolve, reject) => server.once("error", reject).listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function waitFor(predicate, description, timeout = 15000) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < timeout) {
    if (browserErrors.length) throw new Error(`浏览器未捕获异常：${browserErrors.join("\n")}`);
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) { lastError = error; }
    await new Promise((resolve) => setTimeout(resolve, 30));
  }
  throw new Error(`等待超时：${description}${lastError ? `；最后错误：${lastError.message}` : ""}`);
}

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.nextId = 0;
    this.pending = new Map();
  }
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
      } else if (message.method === "Runtime.executionContextCreated") {
        executionContexts.push(message.params.context);
      }
    });
    this.ws.addEventListener("close", () => {
      for (const pending of this.pending.values()) pending.reject(new Error("CDP 连接已关闭"));
      this.pending.clear();
    });
  }
  send(method, params = {}) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) {
      throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text ?? "浏览器表达式失败");
    }
    return result.result?.value;
  }
  close() { this.ws.close(); }
}

function edgePath() {
  const candidates = [
    process.env.EDGE_PATH,
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
  ].filter(Boolean);
  const found = candidates.find((path) => existsSync(path));
  if (!found) throw new Error("找不到 Edge；可用 EDGE_PATH 指定 msedge.exe");
  return found;
}

before(async () => {
  temporary = mkdtempSync(join(temporaryBase, "nianlai-settings-pages-"));
  const vitePort = await freePort();
  const debugPort = await freePort();
  const ipcPath = resolve(root, "tools/settingsPagesIpc.mjs").replace(/\\/g, "/");
  vite = await createServer({
    configFile: false,
    root,
    plugins: [svelte()],
    clearScreen: false,
    logLevel: "error",
    resolve: { alias: [
      { find: "@tauri-apps/api/core", replacement: ipcPath },
      { find: "@tauri-apps/api/event", replacement: ipcPath },
      { find: "@tauri-apps/api/app", replacement: ipcPath },
    ] },
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { host: "127.0.0.1", port: vitePort, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/internal/**", "**/target/**"] } },
  });
  await vite.listen();
  pageUrl = `http://127.0.0.1:${vitePort}/tools/settingsPagesTest.html`;
  edge = spawn(edgePath(), [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--disable-component-update", "--disable-sync",
    `--remote-debugging-port=${debugPort}`, `--user-data-dir=${join(temporary, "edge-profile")}`,
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"], windowsHide: true });
  edge.stderr.setEncoding("utf8");
  edge.stderr.on("data", (chunk) => { edgeStderr.push(chunk); });
  const target = await waitFor(async () => {
    const response = await fetch(`http://127.0.0.1:${debugPort}/json/list`);
    const pages = await response.json();
    return pages.find((page) => page.type === "page");
  }, "无头 Edge 调试目标");
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
  await cleanupHeadlessEdge({ edge, temporary, temporaryBase, prefix: "nianlai-settings-pages-", label: "SettingsPages" });
  const stderr = edgeStderr.join("").trimEnd();
  if (stderr) console.error(`\n[无头 Edge 非阻断 stderr 原文]\n${stderr}\n[/无头 Edge stderr]\n`);
});

function assertNoBrowserErrors() {
  assert.deepEqual(browserErrors, [], "任何场景都不能留下未捕获浏览器异常");
}
afterEach(() => {
  try { assertNoBrowserErrors(); }
  finally { browserErrors.length = 0; }
});

async function openSettings(hash = "", query = "", reuseId = null) {
  const id = reuseId ?? ++scenarioId;
  const navigation = ++navigationId;
  const knownContexts = new Set(executionContexts.map((context) => context.id));
  assertNoBrowserErrors();
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1120, height: 680, deviceScaleFactor: 1, mobile: false });
  await cdp.send("Emulation.setEmulatedMedia", { features: [] });
  const target = `${pageUrl}?test=${id}&nav=${navigation}${query ? `&${query}` : ""}#${hash}`;
  const { frameId } = await cdp.send("Page.navigate", { url: target });
  await waitFor(() => cdp.evaluate(`location.search.includes("test=${id}") && location.search.includes("nav=${navigation}") && !!window.__settingsPagesTest && !!document.querySelector("[data-settings-page]")`), `设置壳 ${hash || "general"} / 导航 ${navigation}`);
  await waitFor(() => cdp.evaluate(`!document.querySelector("[data-settings-loading='true']")`), "设置加载结束");
  const contextId = await waitFor(() => {
    const context = executionContexts.find((candidate) => !knownContexts.has(candidate.id)
      && candidate.auxData?.isDefault && candidate.auxData?.frameId === frameId);
    return context?.id;
  }, `导航 ${navigation} 的新 execution context`);
  assertNoBrowserErrors();
  return { id, navigation, contextId };
}

async function navigate(target) {
  return cdp.evaluate(`window.__settingsPagesTest.navigate(${JSON.stringify(target)})`);
}

async function emitNavigate(target) {
  await cdp.evaluate(`window.__settingsPagesTest.emit("settings:navigate", ${JSON.stringify(target)})`);
  await cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
}

async function clickPage(page) {
  await cdp.evaluate(`document.querySelector("button[data-page='${page}']").click()`);
  await cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
}

async function change(selector, value) {
  await cdp.evaluate(`(() => {
    const control = document.querySelector(${JSON.stringify(selector)});
    if (!control) throw new Error("找不到控件 " + ${JSON.stringify(selector)});
    if (control.type === "checkbox") control.checked = ${value ? "true" : "false"};
    else control.value = ${JSON.stringify(String(value))};
    control.dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
}

async function fill(selector, value) {
  await cdp.evaluate(`(() => {
    const control = document.querySelector(${JSON.stringify(selector)});
    if (!control) throw new Error("找不到输入控件 " + ${JSON.stringify(selector)});
    control.focus();
    control.value = ${JSON.stringify(String(value))};
    control.dispatchEvent(new Event("input", { bubbles: true }));
  })()`);
}
const blur = (selector) => cdp.evaluate(`(() => {
  const control = document.querySelector(${JSON.stringify(selector)});
  if (!control) throw new Error("找不到失焦控件 " + ${JSON.stringify(selector)});
  control.blur();
  // 无头 Edge 有时只改 activeElement 而不发焦点事件；补发同一种真实 DOM 事件驱动生产处理器。
  control.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
})()`);
const keyOn = (selector, init) => cdp.evaluate(`(() => {
  const control = document.querySelector(${JSON.stringify(selector)});
  if (!control) throw new Error("找不到按键目标 " + ${JSON.stringify(selector)});
  control.focus();
  control.dispatchEvent(new KeyboardEvent("keydown", ${JSON.stringify({ bubbles: true, cancelable: true, ...init })}));
})()`);
async function nativeKey(selector, init) {
  await cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.focus()`);
  await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", ...init });
  await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...init });
}
const TAB_KEY = { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9, nativeVirtualKeyCode: 9 };
const ENTER_KEY = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13, text: "\r", unmodifiedText: "\r" };
const SPACE_KEY = { key: " ", code: "Space", windowsVirtualKeyCode: 32, nativeVirtualKeyCode: 32, text: " ", unmodifiedText: " " };
const ESCAPE_KEY = { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 };
async function keyPair(init) {
  await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", ...init });
  await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...init });
}
async function tabUntil(expression, label, limit = 80) {
  for (let index = 0; index < limit; index += 1) {
    if (await cdp.evaluate(expression)) return;
    await keyPair(TAB_KEY);
  }
  const active = await cdp.evaluate(`document.activeElement?.outerHTML ?? ""`);
  assert.fail(`真实 Tab 顺序未到达 ${label}；最后焦点=${active}`);
}
const favoriteIds = () => cdp.evaluate(`[...document.querySelectorAll("[data-favorite-card]")].map((node) => Number(node.dataset.id))`);
const favoriteSaveMark = (id) => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="${id}"] [data-save-mark]')?.outerHTML ?? ""`);

const calls = () => cdp.evaluate("window.__settingsPagesTest.saveCalls()");
const resolveSave = (id, response) => cdp.evaluate(`window.__settingsPagesTest.resolveSave(${id}, ${JSON.stringify(response)})`);
const rejectSave = (id, reason) => cdp.evaluate(`window.__settingsPagesTest.rejectSave(${id}, ${JSON.stringify(reason)})`);
const settleDom = () => cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
const theme = () => cdp.evaluate("document.querySelector('[data-setting=theme]').value");
const motion = () => cdp.evaluate("document.querySelector('[data-setting=motion]').value");
const mark = (key) => cdp.evaluate(`document.querySelector('[data-save-row="${key}"] [data-save-mark]')?.outerHTML ?? ""`);
const ipcCalls = (command) => cdp.evaluate(`window.__settingsPagesTest.invokeCalls(${JSON.stringify(command)})`);
const press = (init) => cdp.evaluate(`window.dispatchEvent(new KeyboardEvent("keydown", ${JSON.stringify({ bubbles: true, cancelable: true, ...init })}))`);
const click = (selector) => cdp.evaluate(`(() => {
  const node = document.querySelector(${JSON.stringify(selector)});
  if (!node) throw new Error("找不到点击目标 " + ${JSON.stringify(selector)});
  node.click();
})()`);
const clickButton = (text, scope = "document") => cdp.evaluate(`(() => {
  const root = ${scope};
  const node = [...root.querySelectorAll("button")].find((button) => button.textContent.trim() === ${JSON.stringify(text)});
  if (!node) throw new Error("找不到按钮 " + ${JSON.stringify(text)});
  node.click();
})()`);
const switchFor = (label) => `(() => {
  const rows = [...document.querySelectorAll("label.setting")];
  const row = rows.find((node) => node.querySelector("strong")?.textContent === ${JSON.stringify(label)})
    ?? rows.find((node) => node.textContent.includes(${JSON.stringify(label)}));
  if (!row) throw new Error("找不到设置行 " + ${JSON.stringify(label)});
  return row.querySelector("input[type=checkbox]");
})()`;
const changeRow = (label, value) => cdp.evaluate(`(() => {
  const rows = [...document.querySelectorAll("label.setting")];
  const row = rows.find((node) => node.querySelector("strong")?.textContent === ${JSON.stringify(label)})
    ?? rows.find((node) => node.textContent.includes(${JSON.stringify(label)}));
  if (!row) throw new Error("找不到设置行 " + ${JSON.stringify(label)});
  const control = row.querySelector("input, select");
  if (control.type === "checkbox") control.checked = ${value ? "true" : "false"};
  else control.value = ${JSON.stringify(String(value))};
  control.dispatchEvent(new Event("change", { bubbles: true }));
})()`);
const controlFor = (label) => `(() => {
  const rows = [...document.querySelectorAll("label.setting")];
  const row = rows.find((node) => node.querySelector("strong")?.textContent === ${JSON.stringify(label)})
    ?? rows.find((node) => node.textContent.includes(${JSON.stringify(label)}));
  if (!row) throw new Error("找不到设置行 " + ${JSON.stringify(label)});
  return row.querySelector("input, select");
})()`;
const clickSettingButton = (rowLabel, buttonLabel) => cdp.evaluate(`(() => {
  const row = [...document.querySelectorAll(".setting")].find((node) => node.textContent.includes(${JSON.stringify(rowLabel)}));
  const button = row && [...row.querySelectorAll("button")].find((node) => node.textContent.trim() === ${JSON.stringify(buttonLabel)});
  if (!button) throw new Error("找不到 " + ${JSON.stringify(rowLabel)} + " 的按钮 " + ${JSON.stringify(buttonLabel)});
  button.click();
})()`);

async function mousePoint(selector) {
  return cdp.evaluate(`(() => {
    const node = document.querySelector(${JSON.stringify(selector)});
    if (!node) throw new Error("找不到原生鼠标目标 " + ${JSON.stringify(selector)});
    node.scrollIntoView({ block: "center", inline: "center" });
    const rect = node.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  })()`);
}
async function nativeClick(selector) {
  const point = await mousePoint(selector);
  await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", buttons: 1, clickCount: 1 });
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", buttons: 0, clickCount: 1 });
}
async function nativeDrag(sourceSelector, targetSelector, release = true) {
  const from = await mousePoint(sourceSelector);
  const to = await mousePoint(targetSelector);
  await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", ...from, button: "left", buttons: 1, clickCount: 1 });
  for (let step = 1; step <= 8; step += 1) {
    const point = { x: from.x + (to.x - from.x) * step / 8, y: from.y + (to.y - from.y) * step / 8 };
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", ...point, button: "left", buttons: 1 });
  }
  if (release) await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...to, button: "left", buttons: 0, clickCount: 1 });
  return { from, to };
}

const PAGE_PROOFS = {
  general: { selector: `[data-setting="placement"] [role="radio"]`, count: 3 },
  fav: { selector: `[data-favorite-card] [data-favorite-front]`, text: "测试常用" },
  privacy: { selector: `form.add input[type="text"]` },
  data: { selector: `.usage .meter` },
  habits: { selector: `[data-setting="click"] option`, count: 4 },
  about: { selector: `.about-page`, text: "复制过的，信手拈来" },
};
const DATA_PROOFS = {
  trash: { selector: `ul[aria-label="回收站"]`, text: "回收站是空的" },
  snapshots: { selector: `.setting select option[value="5120"]` },
  images: { selector: `.picker .toolbar select`, count: 2 },
  export: { selector: `button.primary`, text: "导出" },
  import: { selector: `button.primary`, text: "选择…" },
};
async function assertComponentProof(proof, label) {
  await waitFor(() => cdp.evaluate(`(() => {
    const nodes = [...document.querySelectorAll(${JSON.stringify(proof.selector)})];
    const countOk = ${proof.count === undefined ? "nodes.length > 0" : `nodes.length === ${proof.count}`};
    const textOk = ${proof.text === undefined ? "true" : `nodes.map((node) => node.textContent).join(" ").includes(${JSON.stringify(proof.text)})`};
    return countOk && textOk;
  })()`), `${label} 的真实组件与控件`, 3000);
}
const assertPageProof = (page) => assertComponentProof(PAGE_PROOFS[page], `页面 ${page}`);

const MAIN_PAGES = ["general", "fav", "privacy", "data", "habits", "about"];
const VISUAL_STATES = [
  { id: "light", theme: "light", motion: "spring", followSystem: false, systemDark: false, systemReduce: false },
  { id: "dark", theme: "dark", motion: "spring", followSystem: false, systemDark: false, systemReduce: false },
  { id: "reduced", theme: "light", motion: "none", followSystem: false, systemDark: false, systemReduce: false },
];
const SYSTEM_REDUCED = { id: "system-reduced", theme: "light", motion: "spring", followSystem: true, systemDark: false, systemReduce: true };
const SYSTEM_DARK = { id: "system-dark", theme: "system", motion: "spring", followSystem: false, systemDark: true, systemReduce: false };
const BANNED_VISIBLE = ["收藏", "快照", "历史库", "数据库", "校验", "落盘", "焦点", "前台", "后端", "日志", "条目", "取用", "槽位", "令牌", "Token", "样机", "初始化", "监听", "采集", "注册 / 注销", "引用", "富文本"];

async function setViewport(width, height) {
  await cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
  await settleDom();
}

async function configureVisualState(state) {
  const features = [];
  if (state.systemDark) features.push({ name: "prefers-color-scheme", value: "dark" });
  if (state.systemReduce) features.push({ name: "prefers-reduced-motion", value: "reduce" });
  await cdp.send("Emulation.setEmulatedMedia", { features });
  await cdp.evaluate(`window.__settingsPagesTest.save("general.appearance", {
    theme: ${JSON.stringify(state.theme)},
    feedback: { motion: ${JSON.stringify(state.motion)}, follow_system: ${state.followSystem} }
  }, "外观已保存并应用")`);
  await waitFor(() => cdp.evaluate(`document.documentElement.dataset.theme === ${JSON.stringify(state.theme)}
    && document.documentElement.dataset.motion === ${JSON.stringify(state.motion)}
    && document.documentElement.dataset.reduceMotion === ${JSON.stringify(state.systemReduce && state.followSystem ? "true" : "false")}`), `外观 ${state.id}`);
  await settleDom();
}

async function settleVisual() {
  await cdp.evaluate("document.fonts.ready");
  await settleDom();
  await waitFor(() => cdp.evaluate(`document.getAnimations().every((animation) => animation.playState !== "running")`), "真实布局与动画落位", 4000);
}

async function finishBackupInspection(label) {
  for (let index = 0; index < 8; index += 1) {
    if (await cdp.evaluate(`!!document.querySelector(".choices, .bad")`)) return;
    await cdp.evaluate("window.__settingsPagesTest.tickClock(400)");
    await settleDom();
  }
  assert.fail(`${label} 受控计时后仍未到 ready/failed`);
}

async function openVisualScene(page, state, query = "trash=10&storage=1&snapshots=1&recent=1") {
  await openSettings(page, query);
  await configureVisualState(state);
  await assertPageProof(page);
  if (page === "data") {
    await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan]")?.dataset.loaded === "true"`), "数据页真实回收站与占用");
    await waitFor(() => cdp.evaluate(`(() => {
      const expected = window.__settingsPagesTest.storage().trashed.count.toLocaleString("zh-CN");
      const rolling = document.querySelector(".trash-count .rolling");
      return rolling?.getAttribute("aria-label") === expected && document.querySelector(".trash-count")?.textContent.trim() === expected;
    })()`), "回收站汇总数字与真实占用一致", 4000);
  }
  if (page === "about") await waitFor(() => cdp.evaluate(`document.querySelector("[data-about-version]")?.textContent.includes("0.0.0-test")`), "关于页真实版本");
  await settleVisual();
}

async function assertVisualAudit(label, { reduced = false } = {}) {
  const result = await cdp.evaluate(`(() => {
    const allowed = new Set(["12px", "13px", "15px", "20px"]);
    const visible = (node) => {
      if (node.closest("[inert]")) return false;
      if (node.closest("details:not([open])") && !node.closest("summary")) return false;
      const style = getComputedStyle(node);
      return style.display !== "none" && style.visibility !== "hidden" && Number(style.opacity) > 0 && node.getClientRects().length > 0;
    };
    const name = (node) => {
      const id = node.id ? "#" + node.id : "";
      const classes = typeof node.className === "string" ? node.className.trim().split(/\\s+/).filter(Boolean).slice(0, 2).join(".") : "";
      return node.tagName.toLowerCase() + id + (classes ? "." + classes : "");
    };
    const fontViolations = [];
    const visibleText = [];
    for (const node of document.querySelectorAll("body *")) {
      if (!visible(node)) continue;
      const directText = [...node.childNodes].some((child) => child.nodeType === Node.TEXT_NODE && child.textContent.trim());
      const controlText = node.matches("input, textarea") ? (node.value || node.placeholder || "")
        : node.matches("select") ? [...node.selectedOptions].map((option) => option.textContent.trim()).join(" ") : "";
      if (!directText && !controlText) continue;
      const text = (directText ? [...node.childNodes].filter((child) => child.nodeType === Node.TEXT_NODE).map((child) => child.textContent.trim()).join(" ") : controlText).trim();
      if (!text) continue;
      visibleText.push(text);
      const fontSize = getComputedStyle(node).fontSize;
      if (!allowed.has(fontSize)) fontViolations.push({ node: name(node), fontSize, text: text.slice(0, 70) });
    }
    const content = document.querySelector(".content");
    const settings = document.querySelector(".settings");
    const overflow = [];
    if (document.documentElement.scrollWidth > innerWidth + 1) overflow.push({ node: "document", width: document.documentElement.scrollWidth, viewport: innerWidth });
    if (settings && settings.scrollWidth > settings.clientWidth + 1) overflow.push({ node: "settings", width: settings.scrollWidth, client: settings.clientWidth });
    if (content && content.scrollWidth > content.clientWidth + 1) overflow.push({ node: "content", width: content.scrollWidth, client: content.clientWidth });
    const reachability = [];
    for (const node of document.querySelectorAll("button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, a[href], [role='radio']:not([aria-disabled='true']), [tabindex='0']")) {
      if (!visible(node)) continue;
      const modal = document.querySelector("dialog[open]");
      if (modal && !modal.contains(node)) continue;
      node.scrollIntoView({ block: "center", inline: "center" });
      const rect = node.getBoundingClientRect();
      const outside = rect.left < -1 || rect.right > innerWidth + 1 || rect.top < -1 || rect.bottom > innerHeight + 1 || rect.width <= 0 || rect.height <= 0;
      const x = rect.left + rect.width / 2;
      const y = rect.top + rect.height / 2;
      const hit = !outside && x >= 0 && x < innerWidth && y >= 0 && y < innerHeight ? document.elementFromPoint(x, y) : null;
      const hitOk = !!hit && (hit === node || node.contains(hit) || hit.contains(node));
      if (outside || !hitOk) {
        reachability.push({ node: name(node), left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom,
          viewport: { width: innerWidth, height: innerHeight }, hit: hit ? name(hit) : null });
      }
    }
    if (content) content.scrollTop = 0;
    document.querySelector("[data-settings-notes]")?.scrollTo({ top: 0 });
    document.querySelector(".license-scroll")?.scrollTo({ top: 0 });
    const motionViolations = [];
    if (${reduced ? "true" : "false"}) {
      for (const node of document.querySelectorAll("body *")) {
        if (!visible(node)) continue;
        const style = getComputedStyle(node);
        const durations = style.transitionDuration.split(",").map((value) => value.trim());
        if (style.animationName !== "none" || durations.some((value) => value !== "0s")) {
          motionViolations.push({ node: name(node), animation: style.animationName, transition: style.transitionDuration, transform: style.transform });
        }
      }
    }
    const banned = ${JSON.stringify(BANNED_VISIBLE)}.filter((word) => visibleText.join("\\n").includes(word));
    return { fontViolations, overflow, reachability, motionViolations, banned };
  })()`);
  const failures = Object.entries(result).filter(([, values]) => values.length);
  assert.deepEqual(failures, [], `${label} 的计算样式/可达性/禁用词：${JSON.stringify(failures)}`);
  await settleDom();
}

async function assertDarkSurfaces(label, required = []) {
  const surfaces = await cdp.evaluate(`(() => {
    const read = (selector) => {
      const node = document.querySelector(selector);
      if (!node) return null;
      const style = getComputedStyle(node);
      return { background: style.backgroundColor, color: style.color };
    };
    return {
      nav: read(".nav"), main: read(".main"), favorite: read(".favorite-front"),
      favoriteBack: read(".favorite-back"), trash: read(".trash-card"), about: read(".about-rows"),
      dialog: read("[data-license-dialog]"), seal: read(".favorite-seal"),
    };
  })()`);
  const expected = {
    nav: "rgb(62, 56, 48)", main: "rgb(43, 39, 35)", favorite: "rgb(55, 50, 44)",
    favoriteBack: "rgb(55, 50, 44)", trash: "rgb(55, 50, 44)", about: "rgb(55, 50, 44)",
    dialog: "rgb(43, 39, 35)", seal: "rgb(217, 86, 60)",
  };
  for (const key of required) assert.ok(surfaces[key], `${label} 必须实际挂载 ${key} 特殊表面，不能因缺失而跳过`);
  for (const [key, value] of Object.entries(expected)) {
    if (surfaces[key]) assert.equal(surfaces[key].background, value, `${label} ${key} 必须来自 --hand-* 深色语义而非旧灰绿`);
  }
}

async function captureShot(name, metadata) {
  await settleVisual();
  const { data } = await cdp.send("Page.captureScreenshot", { format: "png", fromSurface: true, captureBeyondViewport: false });
  const bytes = Buffer.from(data, "base64");
  assert.equal(bytes.subarray(1, 4).toString(), "PNG", `${name} 必须是真实 PNG`);
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  assert.deepEqual([width, height], [1120, 680], `${name} 固定窗口尺寸`);
  const file = resolve(shotDirectory, name.endsWith(".png") ? name : `${name}.png`);
  writeFileSync(file, bytes);
  shotManifest.push({ name, file: `internal/design/v11-around/shots/implementation/${name.endsWith(".png") ? name : `${name}.png`}`, width, height, ...metadata });
  writeFileSync(resolve(shotDirectory, "manifest.json"), JSON.stringify(shotManifest, null, 2) + "\n", "utf8");
}

const PROTO_KEYS = ["toString", "constructor", "__proto__", "hasOwnProperty"];

test("生产导航解析：六页、四个别名、五个深链、未知/空 hash 与原型键", async () => {
  await openSettings();
  const expected = new Map([
    ...["general", "fav", "privacy", "data", "habits", "about"].map((page) => [page, { page, storageView: "main" }]),
    ["record", { page: "privacy", storageView: "main" }],
    ["storage", { page: "data", storageView: "main" }],
    ["organize", { page: "fav", storageView: "main" }],
    ["interaction", { page: "habits", storageView: "main" }],
    ...["trash", "snapshots", "images", "export", "import"].map((view) => [view, { page: "data", storageView: view }]),
    ["unknown", { page: "general", storageView: "main" }],
    ["", { page: "general", storageView: "main" }],
    ...PROTO_KEYS.map((target) => [target, { page: "general", storageView: "main" }]),
  ]);
  for (const [target, result] of expected) assert.deepEqual(await navigate(target), result, target || "空 hash");
});

test("原型键初始 hash 与 settings:navigate 都回通用且无异常", async () => {
  for (const target of PROTO_KEYS) {
    await openSettings(target);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), "general", target);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), "main", target);
    await assertPageProof("general");
    await emitNavigate(target);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), "general", `${target} 事件`);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), "main", `${target} 事件`);
    await assertPageProof("general");
    assertNoBrowserErrors();
  }
});

test("真实壳渲染六页、aria-current 与真实关于页", async () => {
  await openSettings();
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("button[data-page]")].map((button) => button.dataset.page)`),
    ["general", "fav", "privacy", "data", "habits", "about"]);
  for (const page of ["general", "fav", "privacy", "data", "habits", "about"]) {
    await clickPage(page);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), page);
    assert.equal(await cdp.evaluate(`document.querySelector("button[data-page='${page}']").getAttribute("aria-current")`), "page");
    assert.equal(await cdp.evaluate(`document.querySelectorAll("button[data-page][aria-current='page']").length`), 1);
    await assertPageProof(page);
  }
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /后续版本开放/);
});

test("旧 settings:navigate 事件与五个数据子页仍挂载真实组件", async () => {
  const aliases = { record: "privacy", storage: "data", organize: "fav", interaction: "habits" };
  for (const [legacy, page] of Object.entries(aliases)) {
    await openSettings(legacy);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), page);
    await assertPageProof(page);
  }
  for (const [view, proof] of Object.entries(DATA_PROOFS)) {
    await openSettings(view);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), "data");
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), view);
    await assertComponentProof(proof, `数据子页 ${view}`);
  }
  await openSettings();
  await emitNavigate("record");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.page`), "privacy");
  await assertPageProof("privacy");
  await emitNavigate("trash");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), "trash");
  await assertComponentProof(DATA_PROOFS.trash, "事件导航回收站");
  await emitNavigate("general");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), "main");
  await assertPageProof("general");
});

test("同一行连续改动不被 pending 丢弃，IPC 严格按调用顺序执行", async () => {
  await openSettings();
  await cdp.evaluate("window.__settingsPagesTest.useManualSaves()");
  await change('[data-setting="theme"]', "light");
  await change('[data-setting="theme"]', "dark");
  assert.deepEqual((await calls()).map((call) => call.update.theme), ["light"], "第二次必须等第一次完成");
  const [first] = await calls();
  await resolveSave(first.id);
  await waitFor(async () => (await calls()).length === 2, "同一行第二次保存");
  assert.deepEqual((await calls()).map((call) => call.update.theme), ["light", "dark"]);
  const [, second] = await calls();
  await resolveSave(second.id);
  await settleDom();
  assert.equal(await theme(), "dark");
});

test("不同行并行且乱序响应不回退主题、不丢动效更新", async () => {
  await openSettings();
  const defaults = await cdp.evaluate("window.__settingsPagesTest.defaults()");
  await cdp.evaluate("window.__settingsPagesTest.useManualSaves()");
  await change('[data-setting="theme"]', "light");
  await change('[data-setting="motion"]', "soft");
  const [themeCall, motionCall] = await calls();
  assert.equal((await calls()).length, 2, "另一行不能被全局 saving 锁住");
  await resolveSave(motionCall.id, { ...defaults, feedback: { motion: "soft", follow_system: false } });
  await settleDom();
  assert.equal(await theme(), "light", "动效响应不能清掉仍在等待的主题更新");
  assert.equal(await motion(), "soft");
  await resolveSave(themeCall.id, { ...defaults, theme: "light", feedback: { motion: "spring", follow_system: false } });
  await settleDom();
  assert.equal(await theme(), "light");
  assert.equal(await motion(), "soft", "较旧主题响应不能把已成功的动效回退成 spring");
});

test("失败行返回真实错误且无成功标记，其他行仍能保存", async () => {
  await openSettings();
  await cdp.evaluate("window.__settingsPagesTest.useManualSaves()");
  await cdp.evaluate(`window.__failedSave = window.__settingsPagesTest.save("general.theme", { theme: "dark" }, "主题已保存并应用"); true`);
  await change('[data-setting="motion"]', "none");
  const [failed, other] = await calls();
  await rejectSave(failed.id, "磁盘满了，腾点地方再试");
  assert.equal(await cdp.evaluate("window.__failedSave"), null, "失败保存必须返回 null");
  await settleDom();
  assert.match(await mark("general.theme"), /磁盘满了，腾点地方再试/);
  assert.match(await mark("general.theme"), /data-phase="failed"/);
  assert.doesNotMatch(await mark("general.theme"), /已保存/);
  assert.equal(await theme(), "system", "失败更新不能留在已应用视图");
  await resolveSave(other.id);
  await settleDom();
  assert.equal(await motion(), "none");
  assert.match(await mark("general.motion"), /已保存/);
});

test("成功标记 1.5 秒消失，连续保存重置计时且旧计时器不清新标记", async () => {
  await openSettings();
  await cdp.evaluate("window.__settingsPagesTest.useManualSaves(); window.__settingsPagesTest.installClock()");
  await change('[data-setting="motion"]', "soft");
  let [call] = await calls();
  await resolveSave(call.id);
  await settleDom();
  assert.match(await mark("general.motion"), /✓ 已保存/);
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1000)");
  await change('[data-setting="motion"]', "none");
  [, call] = await calls();
  await resolveSave(call.id);
  await settleDom();
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  assert.match(await mark("general.motion"), /✓ 已保存/, "第一次的旧计时器不能清掉第二次标记");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(999)");
  assert.match(await mark("general.motion"), /✓ 已保存/);
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1)");
  assert.doesNotMatch(await mark("general.motion"), /✓ 已保存/);
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.saveCalls().map((call) => call.settled)"), [true, true]);
});

test("加载失败显示可读提示，不伪造保存成功", async () => {
  await openSettings("", "loadError=1");
  assert.match(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /没读到你的设置，先按默认的显示。关掉窗口再开试试/);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-save-mark][data-phase='saved']").length`), 0);
  assert.equal(await motion(), "spring");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="follow-system"]').checked`), false);
});

test("系统深色与显式深色的兼容令牌都解析到同一纸墨印体系", async () => {
  await openSettings();
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "dark" }] });
  const readTokens = () => cdp.evaluate(`(() => {
    const style = getComputedStyle(document.documentElement);
    const names = ["--settings-bg", "--settings-nav", "--settings-card", "--settings-text", "--settings-muted", "--settings-line", "--settings-accent", "--settings-selected", "--settings-field", "--hand-paper", "--hand-card", "--hand-card-back", "--hand-ink", "--hand-ink-2", "--hand-line-strong", "--hand-seal"];
    return Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name).trim()]));
  })()`);
  const systemDark = await readTokens();
  await cdp.evaluate(`window.__settingsPagesTest.save("general.theme", { theme: "dark" }, "主题已保存并应用")`);
  await settleDom();
  assert.equal(await cdp.evaluate(`document.documentElement.dataset.theme`), "dark");
  const explicitDark = await readTokens();
  assert.deepEqual(systemDark, explicitDark, "system dark 与 explicit dark 的计算令牌必须一致");
  const pairs = [
    ["--settings-bg", "--hand-paper"], ["--settings-nav", "--hand-card-back"],
    ["--settings-card", "--hand-card"], ["--settings-text", "--hand-ink"],
    ["--settings-muted", "--hand-ink-2"], ["--settings-line", "--hand-line-strong"],
    ["--settings-accent", "--hand-seal"], ["--settings-selected", "--hand-card-back"],
    ["--settings-field", "--hand-card"],
  ];
  for (const tokens of [systemDark, explicitDark]) {
    for (const [compatible, hand] of pairs) assert.equal(tokens[compatible], tokens[hand], `${compatible} 应等于 ${hand}`);
    assert.equal(tokens["--hand-seal"].toLowerCase(), "#d9563c", "深色朱砂令牌不能被旧灰绿色覆盖");
  }
});

test("窄窗口折叠右侧说明栏，六页导航与功能控件仍可用", async () => {
  await openSettings();
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 860, height: 560, deviceScaleFactor: 1, mobile: false });
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-settings-notes]")).display`), "none");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("button[data-page]").length`), 6);
  assert.ok(await cdp.evaluate(`document.querySelector('[data-setting="theme"]').getBoundingClientRect().width > 0`));
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1120, height: 680, deviceScaleFactor: 1, mobile: false });
  assert.notEqual(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-settings-notes]")).display`), "none");
});

test("通用页真实六类控件保留 placement 三值，逐行保存并重读同一字段", async () => {
  let scene = await openSettings();
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="hotkey"] .recorder').length`), 1);
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll('[data-setting="placement"] [role="radio"]')].map((node) => node.dataset.value)`),
    ["caret", "mouse", "center"]);
  for (const selector of ['[data-setting="theme"]', '[data-setting="motion"]', '[data-setting="follow-system"]', '[data-setting="autostart"]']) {
    assert.equal(await cdp.evaluate(`document.querySelectorAll(${JSON.stringify(selector)}).length`), 1, selector);
  }
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="placement"] option[value="bottom"]').length`), 0);
  for (const placement of ["mouse", "center", "caret"]) {
    await click(`[data-setting="placement"] [data-value="${placement}"]`);
    await waitFor(async () => (await calls()).some((call) => call.update.interaction?.placement === placement && call.settled), `位置 ${placement} 保存`);
    assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="placement"] [data-value="${placement}"]').getAttribute("aria-checked")`), "true");
    assert.match(await mark("general.placement"), /✓ 已保存/);
    const previous = scene;
    const reads = await cdp.evaluate("window.__settingsPagesTest.settingsReadCount()");
    scene = await openSettings("", "", scene.id);
    assert.ok(scene.navigation > previous.navigation, "每次重读必须使用新 URL 序号");
    assert.notEqual(scene.contextId, previous.contextId, "每次重读必须是新 execution context");
    assert.equal(await cdp.evaluate("window.__settingsPagesTest.settingsReadCount()"), reads + 1, "新文档必须重新调用 get_settings");
    await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="placement"] [data-value="${placement}"]').getAttribute("aria-checked") === "true"`), `整页重读位置 ${placement}`);
    assert.equal(await cdp.evaluate("window.__settingsPagesTest.settings().interaction.placement"), placement);
  }
  await clickPage("habits");
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-settings-page][data-page="habits"] [data-setting="placement"]').length`), 0);
});

test("操作习惯承接单击、中键全部选项和 reuse_to_top，旧位置不再重复控件", async () => {
  await openSettings("habits");
  for (const key of ["click", "middle-click"]) {
    assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll('[data-setting="${key}"] option')].map((node) => node.value)`),
      ["paste", "copy", "text", "none"]);
  }
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="reuse-to-top"]').length`), 1);
  assert.match(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /常用牌的顺序不受影响/);
  await change('[data-setting="click"]', "copy");
  await change('[data-setting="middle-click"]', "none");
  await change('[data-setting="reuse-to-top"]', false);
  await waitFor(async () => (await calls()).length === 3 && (await calls()).every((call) => call.settled), "三项鼠标与历史设置保存");
  assert.deepEqual((await calls()).map((call) => call.update), [
    { interaction: { click: "copy" } },
    { interaction: { middle_click: "none" } },
    { history: { reuse_to_top: false } },
  ]);
  for (const key of ["habits.click", "habits.middle-click", "habits.reuse-to-top"]) assert.match(await mark(key), /✓ 已保存/);
  await clickPage("fav");
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="reuse-to-top"]').length`), 0);
  await clickPage("general");
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="placement"] [role="radio"]').length`), 3);
});

test("迷你预览读取真实设置，两个试试入口只演示预览且两类减少动画直接落位", async () => {
  await openSettings();
  const preview = (expression) => cdp.evaluate(`(() => { const node = document.querySelector("[data-mini-preview]"); return ${expression}; })()`);
  const initialCards = await cdp.evaluate(`[...document.querySelectorAll("[data-mini-preview] .mini-card")].map((node) => {
    const style = getComputedStyle(node);
    const rect = node.getBoundingClientRect();
    return { opacity: style.opacity, visibility: style.visibility, width: rect.width, height: rect.height };
  })`);
  assert.equal(initialCards.length, 5);
  for (const card of initialCards) {
    assert.equal(card.opacity, "1", "首次预览的牌必须可见");
    assert.equal(card.visibility, "visible");
    assert.ok(card.width > 0 && card.height > 0);
  }
  assert.doesNotMatch(await preview("node.textContent"), /按[\s\S]*试一下/, "不能引导用户按真实全局呼出键演示");
  assert.match(await preview("node.textContent"), /用按钮[\s\S]*不会打开真实面板/);
  const notes = await cdp.evaluate(`document.querySelector("[data-settings-notes]").textContent`);
  assert.doesNotMatch(notes, /迷你手牌|只有手牌一种布局/);
  assert.match(notes, /预览里的面板/);
  assert.equal(await preview("node.dataset.placement"), "caret");
  assert.equal(await preview("node.dataset.theme"), "system");
  assert.equal(await preview("node.dataset.motion"), "spring");
  assert.equal(await preview("node.dataset.reducedMotion"), "false");
  assert.match(await preview("node.textContent"), /Ctrl/);
  const savesBefore = (await calls()).length;
  const panelBefore = (await ipcCalls()).filter((call) => ["hide_panel", "open_settings", "frontend_ready", "set_hit_regions", "pause_hotkey"].includes(call.command)).length;
  await click('[data-action="demonstrate"]');
  await waitFor(() => preview("node.dataset.demoCount === '1'"), "预览内演示");
  assert.equal(await preview("node.dataset.demoState"), "spread");
  await click('[data-try="preview"]');
  await waitFor(() => preview("node.dataset.demoCount === '2'"), "右侧演示入口");
  assert.equal((await calls()).length, savesBefore, "演示不写设置");
  assert.equal((await ipcCalls()).filter((call) => ["hide_panel", "open_settings", "frontend_ready", "set_hit_regions", "pause_hotkey"].includes(call.command)).length,
    panelBefore, "演示不触真实面板 IPC，也不暂停既有呼出键");

  await click('[data-setting="placement"] [data-value="center"]');
  await change('[data-setting="theme"]', "dark");
  await change('[data-setting="motion"]', "none");
  await waitFor(() => preview("node.dataset.placement === 'center' && node.dataset.theme === 'dark' && node.dataset.motion === 'none' && node.dataset.reducedMotion === 'true'"), "预览跟随真实设置");
  await click('[data-action="demonstrate"]');
  await settleDom();
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-mini-preview] .mini-card")).animationName`), "none", "应用内关闭动效直接落位");

  await change('[data-setting="motion"]', "spring");
  await change('[data-setting="follow-system"]', true);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  await waitFor(() => preview("node.dataset.reducedMotion === 'true'"), "跟随系统减少动画");
  await click('[data-action="demonstrate"]');
  await settleDom();
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-mini-preview] .mini-card")).animationName`), "none", "系统减少动画直接落位");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.settings().feedback.motion"), "spring", "预览不把演示状态持久化");
});

test("autostart true/false 成功回读，不写 settings.json，聚焦与重进页面同步外部状态", async () => {
  await openSettings();
  await waitFor(async () => (await ipcCalls("get_autostart")).length > 0, "初次读取开机启动");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked`), false);
  await change('[data-setting="autostart"]', true);
  await waitFor(async () => (await mark("general.autostart")).includes("✓ 已保存"), "开启成功标记");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked`), true);
  assert.equal((await ipcCalls("set_autostart")).at(-1).args.enabled, true);
  assert.equal(await cdp.evaluate("Object.hasOwn(window.__settingsPagesTest.settings(), 'autostart')"), false);
  await change('[data-setting="autostart"]', false);
  await waitFor(async () => (await ipcCalls("set_autostart")).at(-1)?.args.enabled === false, "关闭请求");
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === false`), "关闭回读");

  const focusReads = (await ipcCalls("get_autostart")).length;
  await cdp.evaluate("window.__settingsPagesTest.trayToggleAutostart()");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await waitFor(async () => (await ipcCalls("get_autostart")).length > focusReads
    && await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === true`), "托盘动作后聚焦真实 get 读回开启");
  const reentryReads = (await ipcCalls("get_autostart")).length;
  await cdp.evaluate("window.__settingsPagesTest.setAutostartExternal(false)");
  await clickPage("habits");
  await clickPage("general");
  await waitFor(async () => (await ipcCalls("get_autostart")).length > reentryReads
    && await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === false`), "重进页面真实 get 读回外部关闭");
});

test("autostart 写失败与回读失败无成功标记，外部实际状态覆盖请求值", async () => {
  await openSettings();
  await waitFor(async () => (await ipcCalls("get_autostart")).length > 0, "初次自启读取");
  await cdp.evaluate(`window.__settingsPagesTest.autostartScenario({ writeError: "模拟写开机启动失败" })`);
  await change('[data-setting="autostart"]', true);
  await waitFor(async () => (await mark("general.autostart")).includes('data-phase="failed"'), "写失败就地提示");
  assert.doesNotMatch(await mark("general.autostart"), /✓ 已保存/);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked`), false);

  await cdp.evaluate(`window.__settingsPagesTest.autostartScenario({ readErrors: 1 })`);
  await change('[data-setting="autostart"]', true);
  await waitFor(async () => (await ipcCalls("set_autostart")).length === 2, "第二次自启写入");
  await waitFor(async () => (await ipcCalls("get_autostart")).length >= 2
    && await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === true`), "失败后重新读取实际已写入状态");
  await waitFor(async () => (await mark("general.autostart")).includes('data-phase="failed"'), "回读失败就地提示");
  assert.doesNotMatch(await mark("general.autostart"), /✓ 已保存/);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked`), true);

  await openSettings();
  await waitFor(async () => (await ipcCalls("get_autostart")).length > 0, "新场景初次读取");
  await cdp.evaluate(`window.__settingsPagesTest.autostartScenario({ externalOnWrite: false })`);
  await change('[data-setting="autostart"]', true);
  await waitFor(async () => (await ipcCalls("set_autostart")).length === 1, "外部覆盖请求场景");
  await settleDom();
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked`), false, "请求 true 不得覆盖外部 false");
});

test("autostart 保存途中多次聚焦会在结束后补读，晚到外部状态覆盖旧成功且后续错误可恢复", async () => {
  await openSettings();
  await waitFor(async () => (await ipcCalls("get_autostart")).length === 1, "初次自启读取完成");
  await cdp.evaluate(`window.__settingsPagesTest.autostartDeferred({ set: true, get: true })`);
  await change('[data-setting="autostart"]', true);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().set === 1"), "deferred set 已开始");
  await cdp.evaluate("window.dispatchEvent(new Event('focus')); window.dispatchEvent(new Event('focus'))");
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartSet(true)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get === 1"), "busy 结束后只补读一次");
  assert.equal((await ipcCalls("get_autostart")).length, 2, "保存途中的重复 focus 不能各自发 get");
  await cdp.evaluate("window.__settingsPagesTest.setAutostartExternal(false)");
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartGet()");
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === false`), "晚于 set 回读的外部 false 成为最终 UI");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get"), 0);

  // 后续补读失败要就地报错，下一次聚焦成功后清除旧错误并采用最新值。
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get === 1"), "后续 deferred get");
  await cdp.evaluate(`window.__settingsPagesTest.autostartScenario({ readErrors: 1 })`);
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartGet()");
  await waitFor(async () => (await mark("general.autostart")).includes('data-phase="failed"'), "后续读取失败标记");
  await cdp.evaluate("window.__settingsPagesTest.setAutostartExternal(true)");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get === 1"), "错误后的聚焦补读");
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartGet()");
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === true`), "后续聚焦读到最新 true");
  assert.match(await mark("general.autostart"), /data-phase="idle"/);
});

test("autostart 写失败途中有聚焦时，补读取代旧失败结果并采用最新插件状态", async () => {
  await openSettings();
  await waitFor(async () => (await ipcCalls("get_autostart")).length === 1, "失败场景初次读取");
  await cdp.evaluate(`window.__settingsPagesTest.autostartDeferred({ set: true, get: true })`);
  await change('[data-setting="autostart"]', true);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().set === 1"), "失败 deferred set 已开始");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await cdp.evaluate(`window.__settingsPagesTest.rejectAutostartSet("模拟 deferred 写失败")`);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get === 1"), "失败分支原有回读");
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartGet()");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingAutostart().get === 1"), "失败结束后仍补做聚焦回读");
  await cdp.evaluate("window.__settingsPagesTest.setAutostartExternal(true)");
  await cdp.evaluate("window.__settingsPagesTest.resolveAutostartGet()");
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="autostart"]').checked === true`), "最新外部 true 覆盖失败时的 false");
  assert.match(await mark("general.autostart"), /data-phase="idle"/);
  assert.doesNotMatch(await mark("general.autostart"), /✓ 已保存/);
});

test("呼出键沿用真实录制路径，保存刷新预览，占用失败与 Esc 不落设置", async () => {
  await openSettings();
  await click('[data-setting="hotkey"] .recorder');
  await waitFor(async () => (await ipcCalls("pause_hotkey")).some((call) => call.args.paused === true), "录制前暂停呼出键");
  await press({ key: "F11", code: "F11", keyCode: 123, ctrlKey: true, shiftKey: true });
  await waitFor(async () => (await calls()).some((call) => call.update.hotkey && call.settled), "新呼出键保存");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.settings().hotkey"), "Ctrl+Shift+F11");
  await waitFor(async () => (await ipcCalls("pause_hotkey")).some((call) => call.args.paused === false), "录制后恢复呼出键");
  assert.match(await cdp.evaluate(`document.querySelector('[data-setting="hotkey"] .recorder').textContent`), /F11/);
  assert.match(await cdp.evaluate(`document.querySelector("[data-mini-preview]").textContent`), /F11/);

  await openSettings();
  await cdp.evaluate(`window.__settingsPagesTest.hotkeyScenario({ validationError: "这个组合被别的程序占了，换一个。先点托盘图标也能打开" })`);
  await click('[data-setting="hotkey"] .recorder');
  await press({ key: "F10", code: "F10", keyCode: 122, ctrlKey: true, shiftKey: true });
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-setting="hotkey"] [role="alert"]')?.textContent.includes("被别的程序占了")`), "占用错误");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-setting="hotkey"] .recorder').classList.contains("recording")`), true);
  assert.equal((await calls()).filter((call) => call.update.hotkey).length, 0);
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-save-row="general.hotkey"] [data-phase="saved"]').length`), 0);
  await press({ key: "Escape", code: "Escape", keyCode: 27 });
  await waitFor(() => cdp.evaluate(`!document.querySelector('[data-setting="hotkey"] .recorder').classList.contains("recording")`), "Esc 取消录制");
  assert.equal((await calls()).filter((call) => call.update.hotkey).length, 0);
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.settings().hotkey"), "Ctrl+`");
});

test("常用牌真实牌阵呈现多种内容、藏印与只读正面，空状态不说收藏", async () => {
  await openSettings("fav");
  await waitFor(async () => (await favoriteIds()).length === 4, "四张真实常用牌");
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104]);
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-favorite-card]")].map((node) => node.dataset.kind)`),
    ["short", "link", "image", "file"]);
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-favorite-front] .favorite-position")].map((node) => node.textContent)`),
    ["1", "2", "3", "4"]);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-favorite-grid]").length`), 1);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-favorite-front] input, [data-favorite-front] textarea").length`), 0);
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[data-setting="reuse-to-top"]').length`), 0);
  const seals = await cdp.evaluate(`[...document.querySelectorAll(".favorite-seal")].map((node) => {
    const style = getComputedStyle(node);
    return { text: node.textContent, width: style.width, height: style.height, radius: style.borderRadius };
  })`);
  assert.equal(seals.length, 4);
  for (const seal of seals) assert.deepEqual(seal, { text: "藏", width: "18px", height: "18px", radius: "4px" });
  const copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.match(copy, /呼出后按 ↑ 就是这一手。排好顺序，手就能记住。/);
  assert.match(copy, /拖动换位置，数字跟着位置走/);
  assert.match(copy, /点一张翻到背面，写标题和备注/);
  assert.doesNotMatch(copy, /收藏/);

  await openSettings("fav", "favEmpty=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-favorite-empty]")?.textContent.includes("还没有常用的。呼出后选一张，按 Ctrl+D")`), "真实空状态");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-favorite-card]").length`), 0);
});

test("常用牌鼠标与 Enter/Space 翻背、焦点往返，两类减少动画都无 3D 过渡", async () => {
  await openSettings("fav");
  const front = (id) => `[data-favorite-card][data-id="${id}"] [data-favorite-front]`;
  const flipped = (id, value = true) => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="${id}"]')?.dataset.flipped === "${value}"`);
  await click(front(101));
  await waitFor(() => flipped(101), "鼠标点击翻背");
  await waitFor(() => cdp.evaluate(`document.activeElement?.dataset.favoriteField === "title"`), "翻背后焦点进入标题");
  await click('[data-favorite-card][data-id="101"] [data-action="close-card"]');
  await waitFor(() => flipped(101, false), "返回正面");
  await waitFor(() => cdp.evaluate(`document.activeElement?.hasAttribute("data-favorite-front") && document.activeElement.closest("[data-favorite-card]")?.dataset.id === "101"`), "正面焦点不丢");

  await keyOn(front(102), { key: "Enter", code: "Enter" });
  await waitFor(() => flipped(102), "Enter 翻背");
  await click('[data-favorite-card][data-id="102"] [data-action="close-card"]');
  await waitFor(() => flipped(102, false), "Enter 牌返回");
  await keyOn(front(103), { key: " ", code: "Space" });
  await waitFor(() => flipped(103), "Space 翻背");
  await click('[data-favorite-card][data-id="103"] [data-action="close-card"]');
  await waitFor(() => flipped(103, false), "Space 牌返回");

  await cdp.evaluate(`window.__settingsPagesTest.save("general.motion", { feedback: { motion: "none" } }, "关闭动效")`);
  await settleDom();
  assert.equal(await cdp.evaluate(`document.documentElement.dataset.motion`), "none");
  await click(front(101));
  await waitFor(() => flipped(101), "应用内关闭动效仍可编辑");
  const withoutMotion = await cdp.evaluate(`(() => {
    const card = document.querySelector('[data-favorite-card][data-id="101"]');
    const style = getComputedStyle(card.querySelector(".favorite-flipper"));
    return { transform: style.transform, durations: style.transitionDuration.split(","), title: document.activeElement?.dataset.favoriteField };
  })()`);
  assert.equal(withoutMotion.transform, "none");
  assert.ok(withoutMotion.durations.every((value) => value.trim() === "0s"));
  assert.equal(withoutMotion.title, "title");
  await click('[data-favorite-card][data-id="101"] [data-action="close-card"]');
  await waitFor(() => flipped(101, false), "关闭动效返回");

  await cdp.evaluate(`window.__settingsPagesTest.save("general.motion", { feedback: { motion: "spring", follow_system: true } }, "跟随系统")`);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  await waitFor(() => cdp.evaluate(`document.documentElement.dataset.reduceMotion === "true"`), "系统减少动画标记");
  await click(front(102));
  await waitFor(() => flipped(102), "跟随系统减少动画仍可编辑");
  const systemReduced = await cdp.evaluate(`(() => {
    const card = document.querySelector('[data-favorite-card][data-id="102"]');
    const style = getComputedStyle(card.querySelector(".favorite-flipper"));
    return { transform: style.transform, duration: style.transitionDuration, editable: !card.querySelector('[data-favorite-field="title"]').disabled };
  })()`);
  assert.equal(systemReduced.transform, "none");
  assert.equal(systemReduced.duration, "0s");
  assert.equal(systemReduced.editable, true);
});

test("常用牌标题备注停顿与失焦保存、连续输入不丢、200/2000 上限和重读持久化", async () => {
  const scene = await openSettings("fav");
  await cdp.evaluate("window.__settingsPagesTest.installClock()");
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`!!document.querySelector('[data-favorite-field="title"]')`), "牌背编辑器");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').maxLength`), 200);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="note"]').maxLength`), 2000);

  await fill('[data-favorite-field="title"]', "第一段");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(300)");
  await fill('[data-favorite-field="title"]', "第一段第二段");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(499)");
  assert.equal((await ipcCalls("set_favorite_label")).length, 0, "停顿不足不能提前写");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1)");
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 1 && (await favoriteSaveMark(101)).includes('data-phase="saved"'), "停顿保存成功");
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.title, "第一段第二段", "只写最新完整草稿");
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.note, "");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1499)");
  assert.match(await favoriteSaveMark(101), /✓ 已保存/);
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1)");
  assert.match(await favoriteSaveMark(101), /data-phase="idle"/);

  await fill('[data-favorite-field="title"]', "失焦标题");
  await fill('[data-favorite-field="note"]', "失焦备注");
  await blur('[data-favorite-field="note"]');
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 2, "失焦立即保存");
  assert.deepEqual((await ipcCalls("set_favorite_label"))[1].args, { id: 101, title: "失焦标题", note: "失焦备注" });
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1000)");
  assert.equal((await ipcCalls("set_favorite_label")).length, 2, "失焦与旧计时器不能重复写");

  const longTitle = "题".repeat(220);
  const longNote = "注".repeat(2100);
  await fill('[data-favorite-field="title"]', longTitle);
  await fill('[data-favorite-field="note"]', longNote);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').value.length`), 200);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="note"]').value.length`), 2000);
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 3, "上限草稿保存");
  assert.equal((await ipcCalls("set_favorite_label"))[2].args.title.length, 200);
  assert.equal((await ipcCalls("set_favorite_label"))[2].args.note.length, 2000);

  await openSettings("fav", "", scene.id);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-field="note"]')?.value.length === 2000`), "整页重读 2000 字备注");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').value`), "题".repeat(200));
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="note"]').value`), "注".repeat(2000));

  await cdp.evaluate("window.__settingsPagesTest.installClock()");
  const writesBeforeClose = (await ipcCalls("set_favorite_label")).length;
  await fill('[data-favorite-field="title"]', "返回前最后一段");
  await click('[data-action="close-card"]');
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === writesBeforeClose + 1
    && await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "false"`), "返回前先保存最后草稿");
  assert.equal((await ipcCalls("set_favorite_label")).at(-1).args.title, "返回前最后一段");
});

test("常用牌同一张牌在途保存与失焦串行，旧响应不覆盖最后一段且不重复写", async () => {
  await openSettings("fav");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ label: true })`);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-field="title"]', "在途第一段");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 1"), "第一段在途");
  await fill('[data-favorite-field="title"]', "失焦最后一段");
  await blur('[data-favorite-field="title"]');
  await settleDom();
  assert.equal((await ipcCalls("set_favorite_label")).length, 1, "同牌第二个请求必须等首个完成");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 2, "首个完成后才写最后一段");
  assert.deepEqual((await ipcCalls("set_favorite_label"))[1].args, { id: 101, title: "失焦最后一段", note: "" });
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').value`), "失焦最后一段", "旧响应不能回退草稿");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await favoriteSaveMark(101)).includes('data-phase="saved"'), "最后一段保存成功");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  assert.equal((await ipcCalls("set_favorite_label")).length, 2, "旧停顿计时器与失焦不能造成第三次重复写");

  await fill('[data-favorite-card][data-id="101"] [data-favorite-field="title"]', "排序前先存");
  await keyOn('[data-favorite-card][data-id="102"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 1"), "排序前先等待草稿");
  assert.equal((await ipcCalls("reorder_favorites")).length, 0, "草稿未完成不能发送新顺序");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await ipcCalls("reorder_favorites")).length === 1, "草稿完成后保存顺序");
  assert.equal((await ipcCalls("set_favorite_label"))[2].args.title, "排序前先存");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[0].args.ids, [101, 103, 102, 104]);
});

test("常用牌保存失败保留草稿且可重试，不同牌延迟保存互不阻塞或回退", async () => {
  await openSettings("fav");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteScenario({ labelErrors: 1 })`);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-field="title"]', "失败草稿");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(async () => (await favoriteSaveMark(101)).includes('data-phase="failed"'), "标题失败标记");
  assert.match(await favoriteSaveMark(101), /标题没保存上，再试一次/);
  assert.doesNotMatch(await favoriteSaveMark(101), /✓ 已保存/);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').value`), "失败草稿");
  await click('[data-favorite-card][data-id="101"] [data-action="retry-label"]');
  await waitFor(async () => (await favoriteSaveMark(101)).includes('data-phase="saved"'), "失败后重试成功");
  assert.equal((await ipcCalls("set_favorite_label")).length, 2);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-field="title"]').value`), "失败草稿");

  await openSettings("fav");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ label: true })`);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-card][data-id="101"] [data-favorite-field="title"]', "第一张独立");
  await click('[data-favorite-card][data-id="102"] [data-favorite-front]');
  await fill('[data-favorite-card][data-id="102"] [data-favorite-field="title"]', "第二张独立");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 2"), "两张牌各自发起保存");
  assert.deepEqual((await ipcCalls("set_favorite_label")).map((call) => call.args.id), [101, 102]);
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(102)");
  await waitFor(async () => (await favoriteSaveMark(102)).includes('data-phase="saved"'), "第二张先完成");
  assert.match(await favoriteSaveMark(101), /data-phase="saving"/);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"] [data-favorite-field="title"]').value`), "第一张独立");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await favoriteSaveMark(101)).includes('data-phase="saved"'), "第一张后完成");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="102"] [data-favorite-field="title"]').value`), "第二张独立");
});

test("常用牌 Alt+上下使用完整真实 ID 顺序、数字与焦点跟随，失败回滚并提示", async () => {
  await openSettings("fav");
  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(async () => (await ipcCalls("reorder_favorites")).length === 1, "Alt 下移动保存");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[0].args.ids, [102, 101, 103, 104]);
  await waitFor(async () => JSON.stringify(await favoriteIds()) === JSON.stringify([102, 101, 103, 104]), "界面完整顺序");
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-favorite-front] .favorite-position")].map((node) => node.textContent)`), ["1", "2", "3", "4"]);
  assert.equal(await cdp.evaluate(`document.activeElement.closest("[data-favorite-card]")?.dataset.id`), "101");
  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowUp", code: "ArrowUp", altKey: true });
  await waitFor(async () => (await ipcCalls("reorder_favorites")).length === 2, "Alt 上移保存");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[1].args.ids, [101, 102, 103, 104]);

  await cdp.evaluate(`window.__settingsPagesTest.favoriteScenario({ reorderErrors: 1 })`);
  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-order-status]")?.textContent.includes("顺序没保存上，再拖一次")`), "顺序失败提示");
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104], "失败不能假装新顺序已持久化");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [101, 102, 103, 104]);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-order-status][data-phase="saved"]')`), null);
});

test("Task14 常用牌 focus 旧读晚于排序响应也不能回滚真实 DOM 与后端顺序", async () => {
  await openSettings("fav", "favPair=1");
  await waitFor(async () => (await favoriteIds()).length === 2, "初始 [A,B]");
  assert.deepEqual(await favoriteIds(), [101, 102]);
  await cdp.evaluate(`window.__settingsPagesTest.favoriteDeferred({ order: true, read: true })`);
  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "deferred reorder([B,A]) 在途");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await settleDom();
  const earlyReads = await cdp.evaluate("window.__settingsPagesTest.favoriteReadSnapshots()");
  if (earlyReads.length) {
    assert.deepEqual(earlyReads[0].items.map((item) => item.id), [101, 102], "未修路径确实在写完成前取得旧 [A,B]");
  }
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().read === 1"), "focus 读取待完成");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteRead()");
  await settleDom();
  assert.deepEqual(await favoriteIds(), [102, 101], "旧读不能覆盖排序响应");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [102, 101]);
});

test("Task14 常用牌撤销与 focus 共用结构顺序，旧读不能回滚恢复后的完整顺序", async () => {
  await openSettings("fav", "favPair=1");
  await waitFor(async () => (await favoriteIds()).length === 2, "初始两张");
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="101"] [data-action="remove-favorite"]');
  await waitFor(async () => JSON.stringify(await favoriteIds()) === JSON.stringify([102]), "先移出 A");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteDeferred({ order: true, read: true })`);
  await click('[data-action="undo-favorite"]');
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "撤销恢复原顺序在途");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await settleDom();
  const earlyReads = await cdp.evaluate("window.__settingsPagesTest.favoriteReadSnapshots()");
  if (earlyReads.length) {
    assert.deepEqual(earlyReads[0].items.map((item) => item.id), [102, 101], "旧读捕获恢复常用后的末尾顺序");
  }
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().read === 1"), "撤销后的 focus 读取");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteRead()");
  await settleDom();
  assert.deepEqual(await favoriteIds(), [101, 102]);
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [101, 102]);
});

test("Task14 常用牌排队后的读取错误属于实际执行顺序，后续 focus 成功可恢复", async () => {
  await openSettings("fav", "favPair=1");
  await waitFor(async () => (await favoriteIds()).length === 2, "初始两张");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteDeferred({ order: true, read: true })`);
  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "排序在途");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteScenario({ readErrors: 1 })`);
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await settleDom();
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().read === 1"), "实际执行到 focus 读取");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteRead()");
  await settleDom();
  assert.match(await cdp.evaluate(`document.querySelector(".load-error")?.textContent ?? ""`), /模拟常用读取失败/);
  assert.deepEqual(await favoriteIds(), [102, 101], "读取失败不能把已保存顺序回滚");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [102, 101]);

  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().read === 1"), "后续 focus 成功读取");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteRead()");
  await settleDom();
  assert.equal(await cdp.evaluate(`document.querySelector(".load-error")`), null);
  assert.deepEqual(await favoriteIds(), [102, 101]);
});

test("常用牌真实指针拖动保存完整 ID 列表并更新数字", async () => {
  const scene = await openSettings("fav");
  await cdp.evaluate(`Element.prototype.setPointerCapture = function () {}; Element.prototype.releasePointerCapture = function () {};`);
  const points = await cdp.evaluate(`(() => {
    const center = (id) => {
      const rect = document.querySelector('[data-favorite-card][data-id="' + id + '"] [data-favorite-front]').getBoundingClientRect();
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    };
    return { from: center(101), to: center(103) };
  })()`);
  await cdp.evaluate(`(() => {
    const source = document.querySelector('[data-favorite-card][data-id="101"] [data-favorite-front]');
    const from = ${JSON.stringify(points.from)};
    source.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, button: 0, buttons: 1, pointerId: 41, clientX: from.x, clientY: from.y }));
  })()`);
  for (let step = 1; step <= 6; step += 1) {
    const point = { x: points.from.x + (points.to.x - points.from.x) * step / 6, y: points.from.y + (points.to.y - points.from.y) * step / 6 };
    await cdp.evaluate(`window.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, cancelable: true, button: 0, buttons: 1, pointerId: 41, clientX: ${point.x}, clientY: ${point.y} }))`);
  }
  await cdp.evaluate(`window.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, cancelable: true, button: 0, buttons: 0, pointerId: 41, clientX: ${points.to.x}, clientY: ${points.to.y} }))`);
  await waitFor(async () => (await ipcCalls("reorder_favorites")).some((call) => JSON.stringify(call.args.ids) === JSON.stringify([102, 103, 101, 104])), "拖动完整顺序 IPC");
  assert.deepEqual(await favoriteIds(), [102, 103, 101, 104]);
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-favorite-front] .favorite-position")].map((node) => node.textContent)`), ["1", "2", "3", "4"]);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped`), "false", "拖动不能被当成点击翻背");
  await openSettings("fav", "", scene.id);
  await waitFor(async () => (await favoriteIds()).length === 4, "拖动后的整页重读");
  assert.deepEqual(await favoriteIds(), [102, 103, 101, 104], "新顺序重开后仍保留");
});

test("常用牌移出先存草稿和完整顺序，撤销严格先恢复常用再恢复原顺序", async () => {
  const scene = await openSettings("fav");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ label: true, order: true, favorite: true })`);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-field="title"]', "移出前标题");
  await fill('[data-favorite-field="note"]', "移出前备注");
  await click('[data-favorite-card][data-id="101"] [data-action="remove-favorite"]');
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 1"), "先等待标题备注写入");
  assert.equal((await ipcCalls("reorder_favorites")).length, 0, "草稿未完成不能先排顺序");
  assert.equal((await ipcCalls("set_favorite")).length, 0, "草稿未完成不能取消常用");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "再保存移出前完整顺序");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[0].args.ids, [101, 102, 103, 104]);
  assert.equal((await ipcCalls("set_favorite")).length, 0);
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().favorite === 1"), "最后取消常用");
  assert.deepEqual((await ipcCalls("set_favorite"))[0].args, { id: 101, favorite: false });
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteWrite(101)");
  await waitFor(async () => (await favoriteIds()).length === 3 && await cdp.evaluate(`document.querySelector("[data-favorite-undo]")?.textContent.includes("已移出常用")`), "就地移出与撤销");
  assert.deepEqual(await favoriteIds(), [102, 103, 104]);
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.title, "移出前标题");
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.note, "移出前备注");

  await click('[data-action="undo-favorite"]');
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().favorite === 1"), "撤销先恢复常用");
  assert.deepEqual((await ipcCalls("set_favorite"))[1].args, { id: 101, favorite: true });
  assert.equal((await ipcCalls("reorder_favorites")).length, 1, "恢复常用前不能恢复顺序");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteWrite(101)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "再恢复原完整顺序");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[1].args.ids, [101, 102, 103, 104]);
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(async () => (await favoriteIds()).length === 4, "撤销完成");
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104]);
  assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped`), "false", "撤销回来后正反面状态一致");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [101, 102, 103, 104]);
  assert.deepEqual((await ipcCalls()).filter((call) => ["delete_item", "trash_items", "purge_items"].includes(call.command)), [], "移出撤销不能碰删除语义");
  await openSettings("fav", "", scene.id);
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104]);
  assert.match(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"] [data-favorite-front]').textContent`), /测试常用/, "历史正文不被编辑或删除");
});

test("常用牌撤销失败保留重试和真实状态，不伪造成原顺序或已恢复", async () => {
  await openSettings("fav");
  await click('[data-favorite-card][data-id="102"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="102"] [data-action="remove-favorite"]');
  await waitFor(async () => (await favoriteIds()).length === 3, "先移出第二张");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteScenario({ reorderErrors: 1 })`);
  await click('[data-action="undo-favorite"]');
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-favorite-undo]")?.textContent.includes("撤销没完成，再试一次")`), "恢复顺序失败提示");
  assert.deepEqual(await favoriteIds(), [101, 103, 104, 102], "常用已恢复但顺序失败时必须显示真实末尾位置");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [101, 103, 104, 102]);
  assert.equal(await cdp.evaluate(`document.querySelector("[data-favorite-undo]")?.hasAttribute("hidden")`), false);
  await click('[data-action="undo-favorite"]');
  await waitFor(async () => JSON.stringify(await favoriteIds()) === JSON.stringify([101, 102, 103, 104]), "重试恢复原顺序");

  await click('[data-favorite-card][data-id="104"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="104"] [data-action="remove-favorite"]');
  await waitFor(async () => (await favoriteIds()).length === 3, "再移出第四张");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteScenario({ favoriteErrors: 1 })`);
  const ordersBefore = (await ipcCalls("reorder_favorites")).length;
  await click('[data-action="undo-favorite"]');
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-favorite-undo]")?.textContent.includes("撤销没完成，再试一次")`), "恢复常用失败提示");
  assert.deepEqual(await favoriteIds(), [101, 102, 103], "恢复常用失败不能伪造卡片");
  assert.equal((await ipcCalls("reorder_favorites")).length, ordersBefore, "恢复常用失败不能继续写顺序");
  await click('[data-action="undo-favorite"]');
  await waitFor(async () => (await favoriteIds()).length === 4, "恢复常用失败可重试");
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104]);
  assert.deepEqual((await ipcCalls()).filter((call) => ["delete_item", "trash_items", "purge_items"].includes(call.command)), []);
});

test("常用牌修复 移出后 Alt、拖动、再次移出与 focus 都继续可用", async () => {
  await openSettings("fav");
  await click('[data-favorite-card][data-id="102"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="102"] [data-action="remove-favorite"]');
  await waitFor(async () => (await favoriteIds()).length === 3, "先移出第二张");
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await settleDom();
  assertNoBrowserErrors();

  await keyOn('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "ArrowDown", code: "ArrowDown", altKey: true });
  await waitFor(async () => (await ipcCalls("reorder_favorites")).length === 2, "移出后 Alt 排序");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[1].args.ids, [103, 101, 104]);
  assert.deepEqual(await favoriteIds(), [103, 101, 104]);

  await nativeDrag('[data-favorite-card][data-id="103"] [data-favorite-front]', '[data-favorite-card][data-id="104"] [data-favorite-front]');
  await waitFor(async () => (await ipcCalls("reorder_favorites")).length === 3, "移出后再次拖动");
  assert.deepEqual((await ipcCalls("reorder_favorites"))[2].args.ids, [101, 104, 103]);
  assert.deepEqual(await favoriteIds(), [101, 104, 103]);

  await nativeClick('[data-favorite-card][data-id="104"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="104"]').dataset.flipped === "true"`), "再次翻背");
  await click('[data-favorite-card][data-id="104"] [data-action="remove-favorite"]');
  await waitFor(async () => JSON.stringify(await favoriteIds()) === JSON.stringify([101, 103]), "再次移出成功");
  assert.equal((await ipcCalls("set_favorite")).at(-1).args.id, 104);
  await cdp.evaluate("window.dispatchEvent(new Event('focus'))");
  await settleDom();
  assertNoBrowserErrors();
});

test("常用牌修复 settings:navigate 销毁保存未发起与在途草稿", async () => {
  await openSettings("fav");
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-field="title"]', "导航销毁前最后一段");
  await emitNavigate("general");
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 1, "销毁时同步捕获并发起最新草稿", 2500);
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.title, "导航销毁前最后一段");
  assert.equal(await cdp.evaluate(`window.__settingsPagesTest.favorites().find((item) => item.id === 101).title`), "导航销毁前最后一段");

  await emitNavigate("fav");
  await waitFor(async () => (await favoriteIds()).length === 4, "回到常用页");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ label: true })`);
  await click('[data-favorite-card][data-id="102"] [data-favorite-front]');
  await fill('[data-favorite-card][data-id="102"] [data-favorite-field="title"]', "销毁时在途草稿");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 1"), "销毁前请求已在途");
  await emitNavigate("general");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(102)");
  await settleDom();
  assert.equal(await cdp.evaluate(`window.__settingsPagesTest.favorites().find((item) => item.id === 102).title`), "销毁时在途草稿");
  assertNoBrowserErrors();
});

test("常用牌修复 三 revision 合并实际快照且不产生后续假失败", async () => {
  await openSettings("fav");
  await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ label: true })`);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await fill('[data-favorite-field="title"]', "第一段");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().label === 1"), "revision A 在途");
  await fill('[data-favorite-field="title"]', "第二段");
  await blur('[data-favorite-field="title"]');
  await fill('[data-favorite-field="note"]', "第三段备注");
  await blur('[data-favorite-field="note"]');
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 2, "只补写一次最新 revision C");
  assert.deepEqual((await ipcCalls("set_favorite_label"))[1].args, { id: 101, title: "第二段", note: "第三段备注" });
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteLabel(101)");
  await waitFor(async () => (await favoriteSaveMark(101)).includes('data-phase="saved"'), "最新 revision 已成功");
  await settleDom();
  assert.equal((await ipcCalls("set_favorite_label")).length, 2, "不能把 revision C 再写第三次");
  await cdp.evaluate(`window.__settingsPagesTest.favoriteScenario({ labelErrors: 1 })`);
  await blur('[data-favorite-field="note"]');
  await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
  await settleDom();
  assert.equal((await ipcCalls("set_favorite_label")).length, 2, "无新编辑不能制造第三次失败请求");
  assert.doesNotMatch(await favoriteSaveMark(101), /data-phase="failed"/, "已保存的最新值不能显示假失败");
});

test("常用牌修复 移出在 reorder 与 setFavorite 在途窗口锁定晚到编辑", async () => {
  for (const phase of ["order", "favorite"]) {
    await openSettings("fav");
    await cdp.evaluate(`window.__settingsPagesTest.installClock(); window.__settingsPagesTest.favoriteDeferred({ order: true, favorite: true })`);
    await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
    await click('[data-favorite-card][data-id="101"] [data-action="remove-favorite"]');
    if (phase === "favorite") {
      await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "先进入顺序窗口");
      await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
    }
    await waitFor(() => cdp.evaluate(`window.__settingsPagesTest.pendingFavorites().${phase} === 1`), `移出 ${phase} 在途`);
    const disabled = await cdp.evaluate(`({
      title: document.querySelector('[data-favorite-field="title"]').disabled,
      note: document.querySelector('[data-favorite-field="note"]').disabled,
      remove: document.querySelector('[data-action="remove-favorite"]').disabled,
    })`);
    await fill('[data-favorite-field="title"]', `${phase} 在途晚到编辑`);
    await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
    assert.deepEqual(disabled, { title: true, note: true, remove: true }, `${phase} 窗口必须从点击起锁定`);
    assert.equal((await ipcCalls("set_favorite_label")).length, 0, `${phase} 窗口不能写入晚到草稿`);
    if (phase === "order") await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
    await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().favorite === 1"), "进入取消常用窗口");
    await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteWrite(101)");
    await waitFor(async () => (await favoriteIds()).length === 3, `${phase} 窗口移出完成`);
  }
});

test("常用牌修复 拖动中禁止成员变更且顺序失败按真实成员重读", async () => {
  await openSettings("fav");
  await click('[data-favorite-card][data-id="102"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="102"] [data-action="remove-favorite"]');
  await waitFor(async () => (await favoriteIds()).length === 3, "准备已移出一张的撤销快照");
  await click('[data-favorite-card][data-id="103"] [data-favorite-front]');
  await cdp.evaluate(`window.__settingsPagesTest.favoriteDeferred({ order: true })`);
  const { to } = await nativeDrag('[data-favorite-card][data-id="101"] [data-favorite-front]', '[data-favorite-card][data-id="104"] [data-favorite-front]', false);
  await click('[data-favorite-card][data-id="103"] [data-action="remove-favorite"]');
  await click('[data-action="undo-favorite"]');
  assert.equal((await ipcCalls("set_favorite")).length, 1, "按住拖动时不能再移出或恢复常用");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order"), 0, "按住拖动时成员操作不能抢顺序队列");
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...to, button: "left", buttons: 0, clickCount: 1 });
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "拖动顺序请求在途");
  await cdp.evaluate(`window.__settingsPagesTest.rejectFavoriteOrder("模拟拖动顺序失败")`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-order-status]")?.textContent.includes("顺序没保存上，再拖一次")`), "拖动失败提示");
  assert.deepEqual(await favoriteIds(), [101, 103, 104], "失败回滚不能把已移出的 102 画回");
  assert.deepEqual(await cdp.evaluate("window.__settingsPagesTest.favorites().map((item) => item.id)"), [101, 103, 104]);

  await click('[data-action="undo-favorite"]');
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingFavorites().order === 1"), "失败后可重新撤销");
  await cdp.evaluate("window.__settingsPagesTest.resolveFavoriteOrder()");
  await waitFor(async () => (await favoriteIds()).length === 4, "撤销恢复成员和顺序");
  assert.deepEqual(await favoriteIds(), [101, 102, 103, 104]);
});

test("常用牌修复 最后一张移出后焦点落在撤销", async () => {
  await openSettings("fav", "favSingle=1");
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await click('[data-favorite-card][data-id="101"] [data-action="remove-favorite"]');
  await waitFor(() => cdp.evaluate(`!!document.querySelector("[data-favorite-empty]")`), "最后一张后的空状态");
  assert.equal(await cdp.evaluate(`document.activeElement?.dataset.action`), "undo-favorite");
  assert.match(await cdp.evaluate(`document.querySelector("[data-favorite-empty]").textContent`), /还没有常用的/);
});

test("常用牌修复 mock 标题清理只折 CR/LF 并保留连续普通空格", async () => {
  await openSettings("fav");
  const title = "  A  B\r\nC  ";
  const note = "  注\n第二行  ";
  const saved = await cdp.evaluate(`import("/src/lib/ipc.ts").then((ipc) => ipc.setFavoriteLabel(101, ${JSON.stringify(title)}, ${JSON.stringify(note)}))`);
  assert.equal(saved.title, "A  B  C");
  assert.equal(saved.note, "注\n第二行");
  assert.equal(await cdp.evaluate(`window.__settingsPagesTest.favorites().find((item) => item.id === 101).title`), "A  B  C");
});

test("常用牌修复 CDP 原生点击焦点保存与 forced-colors 计算样式", async () => {
  await openSettings("fav");
  await nativeClick('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true" && document.activeElement?.dataset.favoriteField === "title"`), "原生无位移点击翻背并聚焦");
  await cdp.send("Input.insertText", { text: "原生标题" });
  await cdp.evaluate("new Promise((resolve) => setTimeout(resolve, 400))");
  await nativeClick('[data-favorite-card][data-id="101"] [data-favorite-field="note"]');
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 1, "原生焦点离开标题触发保存");
  assert.equal((await ipcCalls("set_favorite_label"))[0].args.title, "原生标题");
  assert.equal(await cdp.evaluate(`document.activeElement?.dataset.favoriteField`), "note");
  await cdp.send("Input.insertText", { text: "原生备注" });
  await nativeClick('[data-favorite-card][data-id="101"] [data-action="close-card"]');
  await waitFor(async () => (await ipcCalls("set_favorite_label")).length === 2
    && await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "false"`), "原生返回前保存备注");
  assert.equal((await ipcCalls("set_favorite_label"))[1].args.note, "原生备注");
  assert.equal(await cdp.evaluate(`document.activeElement?.hasAttribute("data-favorite-front")`), true);

  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  assert.equal(await cdp.evaluate(`matchMedia("(forced-colors: active)").matches`), true);
  const styles = await cdp.evaluate(`(() => {
    const card = document.querySelector('[data-favorite-card][data-id="101"]');
    const front = getComputedStyle(card.querySelector("[data-favorite-front]"));
    const seal = getComputedStyle(card.querySelector(".favorite-seal"));
    const input = getComputedStyle(card.querySelector('[data-favorite-field="title"]'));
    return { border: front.borderTopStyle, width: front.borderTopWidth, shadow: front.boxShadow,
      sealBackground: seal.backgroundColor, sealColor: seal.color, inputBorder: input.borderTopStyle };
  })()`);
  assert.equal(styles.border, "solid");
  assert.equal(styles.width, "1px");
  assert.equal(styles.shadow, "none");
  assert.notEqual(styles.sealBackground, styles.sealColor);
  assert.equal(styles.inputBorder, "solid");
});

test("Task8 修复 记录与隐私保留全部控件，等待时间先在开启时操作再验证关闭禁用", async () => {
  await openSettings("privacy");
  const copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.match(copy, /记录与隐私/);
  assert.match(copy, /你复制的东西，哪些记、哪些不记。不记的，不会留下任何痕迹。/);
  assert.match(copy, /带格式的文字/);
  assert.doesNotMatch(copy, /富文本/);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("details.capture-more").length`), 1);
  await cdp.evaluate(`document.querySelector("details.capture-more").open = true`);
  await changeRow("一条文字最长多少", "256");
  assert.equal(await cdp.evaluate(`${controlFor("等多久再记")}.disabled`), false, "临时识别开启时等待时间必须可操作");
  await changeRow("等多久再记", "1500");
  await changeRow("别的工具临时借用剪贴板时不记", false);
  await waitFor(() => cdp.evaluate(`${controlFor("等多久再记")}.disabled`), "关闭临时识别后等待时间禁用");
  await waitFor(async () => (await calls()).length === 3 && (await calls()).every((call) => call.settled), "了解更多三项保存");
  assert.deepEqual((await calls()).map((call) => call.update.capture), [
    { max_text_kb: 256 }, { transient_ms: 1500 }, { transient: false },
  ]);
  for (const label of ["文字", "带格式的文字", "图片", "文件"]) {
    assert.ok(await cdp.evaluate(`[...document.querySelectorAll("label.setting")].some((row) => row.querySelector("strong")?.textContent === ${JSON.stringify(label)} && row.querySelector("input[role=switch]"))`), label);
  }
  assert.ok(await cdp.evaluate(`${switchFor("登录凭证（JWT）")} !== null`));
});

test("暂停、下一次复制不记、排除程序建议与默认名单继续走真实 IPC", async () => {
  await openSettings("privacy", "recent=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("form.add") && !${switchFor("暂停记录")}.disabled`), "记录状态与来源建议");
  await changeRow("暂停记录", true);
  await changeRow("下一次复制不记", true);
  await waitFor(async () => (await ipcCalls("set_capture_paused")).at(-1)?.args.paused === true
    && (await ipcCalls("set_ignore_next")).at(-1)?.args.ignore === true, "运行状态 IPC");
  await waitFor(() => cdp.evaluate(`${switchFor("暂停记录")}.checked && ${switchFor("下一次复制不记")}.checked`), "运行状态事件回读");
  assert.equal((await calls()).filter((call) => call.update.capture?.paused !== undefined || call.update.capture?.ignore_next !== undefined).length, 0,
    "运行状态不能写进设置文件");

  assert.match(await cdp.evaluate(`document.querySelector(".suggest")?.textContent`), /Recent.exe/);
  await clickButton("+ Recent.exe");
  await waitFor(async () => (await calls()).some((call) => call.update.capture?.excluded_apps?.includes("Recent.exe") && call.settled), "最近程序建议加入");
  await clickButton("放回默认的密码管理器");
  await waitFor(async () => (await calls()).some((call) => call.update.capture?.excluded_apps?.includes("KeePass.exe") && call.settled), "补回默认名单");
  const beforeInvalid = (await calls()).length;
  await fill("form.add input", "C:\\KeePass.exe");
  await cdp.evaluate(`document.querySelector("form.add").requestSubmit()`);
  assert.match(await cdp.evaluate(`document.querySelector("form.add")?.parentElement.textContent`), /只填程序名，比如 KeePass.exe/);
  assert.equal((await calls()).length, beforeInvalid, "路径不能写入排除名单");
  await fill("form.add input", "example.exe");
  await cdp.evaluate(`document.querySelector("form.add").requestSubmit()`);
  assert.match(await cdp.evaluate(`document.querySelector("form.add")?.parentElement.textContent`), /example.exe 已在名单里/);
  assert.equal((await calls()).length, beforeInvalid, "大小写重复不能写入排除名单");
  await click(`button[aria-label="拿掉 Example.exe"]`);
  await waitFor(async () => (await calls()).some((call) => call.settled && call.update.capture?.excluded_apps
    && !call.update.capture.excluded_apps.includes("Example.exe")), "移除程序真实保存");
  assert.equal(await cdp.evaluate(`document.querySelector('button[aria-label="拿掉 Example.exe"]')`), null);
});

test("记录状态读取失败有可读提示，不伪造开关状态", async () => {
  await openSettings("privacy", "captureError=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page]").textContent.includes("没读到记录状态，关掉窗口再开试试")`), "记录状态错误");
  assert.equal(await cdp.evaluate(`${switchFor("暂停记录")}.disabled`), true);
  assert.equal(await cdp.evaluate(`${switchFor("下一次复制不记")}.disabled`), true);
});

test("Task8 修复 运行状态写失败保留真实开关并显示错误，订阅失败可读", async () => {
  await openSettings("privacy");
  await waitFor(() => cdp.evaluate(`!${switchFor("暂停记录")}.disabled`), "记录状态就绪");
  await cdp.evaluate(`window.__settingsPagesTest.captureScenario({ pausedErrors: 1, ignoreErrors: 1 })`);
  await changeRow("暂停记录", true);
  await waitFor(() => cdp.evaluate(`document.querySelector(".state-error")?.textContent.includes("模拟暂停写失败")`), "暂停写失败提示");
  assert.equal(await cdp.evaluate(`${switchFor("暂停记录")}.checked`), false, "写失败不能留下假开启");
  await changeRow("下一次复制不记", true);
  await waitFor(() => cdp.evaluate(`document.querySelector(".state-error")?.textContent.includes("模拟下一次写失败")`), "下一次写失败提示");
  assert.equal(await cdp.evaluate(`${switchFor("下一次复制不记")}.checked`), false);
  assert.equal((await calls()).filter((call) => call.update.capture?.paused !== undefined || call.update.capture?.ignore_next !== undefined).length, 0);

  await openSettings("privacy", "captureListenError=1");
  await waitFor(() => cdp.evaluate(`document.querySelector(".state-error")?.textContent.includes("模拟记录状态订阅失败")`), "订阅失败提示");
  assert.match(await cdp.evaluate(`document.querySelector(".state-error").textContent`), /记录状态变化没收到/);
});

test("Task8 修复 四种类型与敏感规则逐项调用真实设置保存", async () => {
  await openSettings("privacy");
  await changeRow("带格式的文字", false);
  await changeRow("图片", false);
  await changeRow("文件", false);
  await changeRow("文字", false);
  await changeRow("登录凭证（JWT）", false);
  await waitFor(async () => (await calls()).length === 5 && (await calls()).every((call) => call.settled), "类型和规则保存");
  assert.deepEqual((await calls()).map((call) => call.update.capture), [
    { rich_text: false }, { images: false }, { files: false }, { text: false }, { sensitive_off: ["jwt"] },
  ]);
});

test("失效文件与截图清理第一次零调用，第二次只清理最新范围且没有清理撤销", async () => {
  await openSettings("data", "storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page]").textContent.includes("失效")`), "占用与清理读取");
  await clickSettingButton("失效", "清理");
  assert.equal((await ipcCalls("clean_missing")).length, 0, "第一次确认不能清理失效文件");
  await clickSettingButton("失效", "确认清理 3 条");
  await waitFor(async () => (await ipcCalls("clean_missing")).length === 1, "第二次才清理失效文件");
  assert.deepEqual((await ipcCalls("clean_missing"))[0].args, {});

  const selectRange = (value) => cdp.evaluate(`(() => {
    const row = [...document.querySelectorAll(".setting")].find((node) => node.textContent.includes("截图类图片"));
    row.querySelector("select").value = ${JSON.stringify(value)};
    row.querySelector("select").dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
  await selectRange("90");
  await clickSettingButton("截图类图片", "清理");
  assert.equal((await ipcCalls("clean_images")).length, 0, "第一次确认不能清理截图");
  await selectRange("all");
  await clickSettingButton("截图类图片", "确认清理 12 张");
  await waitFor(async () => (await ipcCalls("clean_images")).length === 1, "第二次清理截图");
  assert.deepEqual((await ipcCalls("clean_images"))[0].args, { olderDays: null }, "必须使用确认时的最新范围");
  assert.equal((await ipcCalls()).filter((call) => call.command === "restore_items" || call.command === "restoreItems").length, 0);
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /清理撤销|撤销清理/);
});

test("回收站单项彻底删除只发自身 ID，完整清空四秒超时复原且窗口内第二次才全清", async () => {
  await openSettings("trash", "trash=2");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 2`), "两条回收站内容");
  await click("ul[aria-label='回收站'] > li:first-child .danger");
  await waitFor(async () => (await ipcCalls("purge_items")).length === 1, "单项彻底删除");
  assert.deepEqual((await ipcCalls("purge_items"))[0].args, { ids: [700] });
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 1`), "单删后重读");

  await cdp.evaluate("window.__settingsPagesTest.installClock()");
  await click(".foot .danger");
  assert.equal((await ipcCalls("purge_items")).length, 1, "第一次清空确认零 purge");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(3999)");
  assert.equal((await ipcCalls("purge_items")).length, 1);
  await cdp.evaluate("window.__settingsPagesTest.tickClock(1)");
  await settleDom();
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector(".foot .danger").innerText`), /再点一次|确认清空/, "四秒后必须复原");
  assert.equal((await ipcCalls("purge_items")).length, 1, "超过四秒仍不能全清");

  await click(".foot .danger");
  await cdp.evaluate("window.__settingsPagesTest.tickClock(3999)");
  await click(".foot .danger");
  await waitFor(async () => (await ipcCalls("purge_items")).length === 2, "有效窗口第二次全清");
  assert.deepEqual((await ipcCalls("purge_items"))[1].args, { ids: null });
  await waitFor(() => cdp.evaluate(`document.querySelector("ul[aria-label='回收站']")?.textContent.includes("回收站是空的")`), "全清后真实空状态");
});

test("数据主页按真实顺序摊开最近八张，失败保留重试，成功后补入下一张并刷新计数", async () => {
  await openSettings("data", "trash=10&storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length === 8`), "最近八张");
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-trash-fan] [data-trash-card]")].map((node) => Number(node.dataset.id))`),
    [700, 701, 702, 703, 704, 705, 706, 707]);
  assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan]").textContent`), /查看全部/);
  assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan]").textContent`), /材料-4.pdf/);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] .danger").length`), 0, "主页不能复制危险动作");

  await cdp.evaluate(`window.__settingsPagesTest.trashScenario({ restoreErrors: 1 })`);
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan]").textContent.includes("模拟拿回失败")`), "恢复失败可读错误");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 8, "失败项必须保留");
  assert.ok(await cdp.evaluate(`document.querySelector('[data-trash-card][data-id="700"]')`));
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate(`window.__settingsPagesTest.trashItems().length === 9 && document.querySelector('[data-trash-card][data-id="708"]')`), "重试成功并补入第九张");
  assert.deepEqual((await ipcCalls("restore_item")).map((call) => call.args), [{ id: 700 }, { id: 700 }]);
  assert.equal(await cdp.evaluate(`window.__settingsPagesTest.storage().trashed.count`), 9);
  await waitFor(() => cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim() === "9"`), "真实占用计数刷新");
});

test("数据主页回收站空、少于八张、恰好八张和读取失败都有真实呈现，查看全部仍到旧深链组件", async () => {
  for (const count of [0, 3, 8]) {
    await openSettings("data", `trash=${count}`);
    await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan]")?.dataset.loaded === "true"`), `回收站 ${count} 张加载`);
    assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), count);
    if (count === 0) assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan]").textContent`), /回收站是空的/);
  }
  await openSettings("data", "trashError=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan]")?.textContent.includes("模拟回收站读取失败")`), "回收站读取失败");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 0);

  await openSettings("data", "trash=3");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length === 3`), "查看全部前加载");
  await clickButton("查看全部", `document.querySelector("[data-trash-fan]")`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView === "trash"`), "查看全部进入完整列表");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length`), 3);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan]").length`), 0);
});

test("回收站摊开区可收起且按钮可达，两类减少动画都直接落位", async () => {
  await openSettings("data", "trash=3");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length === 3`), "三张摊开");
  await clickButton("收起", `document.querySelector("[data-trash-fan]")`);
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] .fan-actions button:last-child").getAttribute("aria-expanded")`), "false");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 0);
  await cdp.evaluate(`document.querySelector("[data-trash-fan] .fan-actions button:last-child").focus()`);
  assert.equal(await cdp.evaluate(`document.activeElement?.getAttribute("aria-expanded")`), "false");
  await clickButton("摊开", `document.querySelector("[data-trash-fan]")`);
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length === 3`), "重新摊开");

  await cdp.evaluate(`window.__settingsPagesTest.save("general.motion", { feedback: { motion: "none" } }, "关闭动效")`);
  await settleDom();
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-trash-card]")).animationName`), "none", "应用内关闭动效");
  await cdp.evaluate(`window.__settingsPagesTest.save("general.motion", { feedback: { motion: "spring", follow_system: true } }, "跟随系统")`);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  await waitFor(() => cdp.evaluate(`document.documentElement.dataset.reduceMotion === "true"`), "系统减少动画");
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-trash-card]")).animationName`), "none", "跟随系统减少动画");
  assert.equal((await ipcCalls("restore_item")).length, 0, "动效变化不能触发恢复");
});

test("Task8 修复 TrashFan 外部旧读与恢复后新读乱序时，旧成功不能写回已恢复项", async () => {
  await openSettings("data", "trash=10&storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 8`), "初始八张");
  await cdp.evaluate("window.__settingsPagesTest.trashReadDeferred()");
  await clickSettingButton("失效", "清理");
  await clickSettingButton("失效", "确认清理 3 条");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingTrashReads() === 1"), "外部清理触发旧读 R1");
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingTrashReads() === 2"), "恢复触发新读 R2");
  await cdp.evaluate("window.__settingsPagesTest.resolveTrashRead(1, window.__settingsPagesTest.trashItems())");
  await settleDom();
  await cdp.evaluate("window.__settingsPagesTest.resolveTrashRead(0)");
  await settleDom();
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("[data-trash-card]")].map((node) => Number(node.dataset.id))`),
    [701, 702, 703, 704, 705, 706, 707, 708], "旧 R1 不能覆盖新 R2");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-trash-card][data-id="700"]')`), null);
});

test("Task8 修复 TrashFan onMount 旧失败晚到不覆盖外部新读，销毁后在途响应安全失效", async () => {
  await openSettings("data", "trash=3&storage=1&trashDeferred=1");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingTrashReads() === 1"), "onMount 旧读");
  await clickSettingButton("失效", "清理");
  await clickSettingButton("失效", "确认清理 3 条");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingTrashReads() === 2"), "外部新读");
  await cdp.evaluate("window.__settingsPagesTest.resolveTrashRead(1)");
  await settleDom();
  await cdp.evaluate(`window.__settingsPagesTest.rejectTrashRead(0, "模拟旧读取失败")`);
  await settleDom();
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] .fan-message.error")`), null, "旧失败不能覆盖新状态");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length`), 3);

  await openSettings("data", "trash=1&trashDeferred=1");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingTrashReads() === 1"), "销毁前在途读取");
  await emitNavigate("general");
  await cdp.evaluate(`window.__settingsPagesTest.rejectTrashRead(0, "销毁后的迟到失败")`);
  await settleDom();
  assertNoBrowserErrors();
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan]")`), null);

  await openSettings("data", "trash=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 1`), "销毁前恢复在途场景");
  await cdp.evaluate("window.__settingsPagesTest.restoreDeferred()");
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingRestores() === 1"), "销毁前恢复 pending");
  await emitNavigate("general");
  const trashReads = (await ipcCalls("get_trash")).length;
  const storageReads = (await ipcCalls("get_storage")).length;
  await cdp.evaluate("window.__settingsPagesTest.resolveRestore()");
  await settleDom();
  assert.equal((await ipcCalls("get_trash")).length, trashReads, "销毁后的恢复完成不能再读旧列表");
  assert.equal((await ipcCalls("get_storage")).length, storageReads, "销毁后的恢复完成不能再刷新旧父页");
  assertNoBrowserErrors();
});

test("Task8 修复 restoreItem 返回 null 只提示已不在回收站并重读，绝不伪造成功", async () => {
  await openSettings("data", "trash=2&restoreNull=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 2`), "null 场景初始牌");
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(async () => (await ipcCalls("get_trash")).length >= 2 && (await cdp.evaluate("window.__settingsPagesTest.trashItems().length")) === 1, "null 后真实重读");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='status']")`), null);
  assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='alert']")?.textContent`), /这张已经不在回收站/);
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector("[data-trash-fan]").textContent`), /拿回来了/);
});

test("Task8 修复 恢复非 null 后列表或占用刷新失败，已确认 ID 先移除并明确部分成功", async () => {
  await openSettings("data", "trash=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 1`), "列表刷新失败场景");
  await cdp.evaluate(`window.__settingsPagesTest.trashScenario({ readErrors: 1 })`);
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan] [role='alert']")?.textContent.includes("列表没刷新")`), "列表刷新失败提示");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-trash-card][data-id="700"]')`), null, "已恢复 ID 不能留在旧牌里");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='status']")`), null, "刷新失败不能显示完整成功");
  assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='alert']").textContent`), /已拿回/);

  await openSettings("data", "trash=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 1`), "占用刷新失败场景");
  await cdp.evaluate(`window.__settingsPagesTest.storageScenario({ readErrors: 1 })`);
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan] [role='alert']")?.textContent.includes("占用没刷新")`), "占用刷新失败提示");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-trash-card][data-id="700"]')`), null);
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='status']")`), null);
  assert.match(await cdp.evaluate(`document.querySelector("[data-settings-page] .banner.error").textContent`), /模拟占用读取失败/);
});

test("Task8 第二轮 父页旧占用成功晚到不能回退恢复后的真实计数", async () => {
  await openSettings("data", "trash=10&storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 8 && document.querySelector(".trash-count")?.textContent.trim() === "10"`), "父页旧成功场景初始状态");
  await cdp.evaluate("window.__settingsPagesTest.storageReadDeferred()");
  await clickSettingButton("失效", "清理");
  await clickSettingButton("失效", "确认清理 3 条");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 1"), "清理触发父页旧读 R1");
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 2"), "恢复触发父页新读 R2");
  await cdp.evaluate("window.__settingsPagesTest.resolveStorageRead(1)");
  await waitFor(() => cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim() === "9" && document.querySelector('[data-trash-card][data-id="708"]')`), "R2 先显示恢复后的九张");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.storage().trashed.count"), 9);
  await cdp.evaluate("window.__settingsPagesTest.resolveStorageRead(0)");
  // RollingNumber 最多滚动约 0.7 秒；等它停稳再判断，避免把动画中间帧误当成旧响应被拦截。
  await new Promise((resolve) => setTimeout(resolve, 800));
  await settleDom();
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.storage().trashed.count"), 9, "后端仍是九张");
  assert.equal(await cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim()`), "9", "旧 R1 不能把界面从九回退到十");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-trash-card][data-id="700"]')`), null);
  assertNoBrowserErrors();
});

test("Task8 第二轮 过期 load(true) 旧失败不覆盖新成功或伪造占用刷新失败", async () => {
  await openSettings("data", "trash=2&storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-card]").length === 2 && document.querySelector(".trash-count")?.textContent.trim() === "2"`), "父页旧失败场景初始状态");
  await cdp.evaluate("window.__settingsPagesTest.storageReadDeferred()");
  await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 1"), "恢复触发 load(true) 旧读 R1");
  await clickSettingButton("失效", "重新检查");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 2"), "其他子页动作触发新读 R2");
  await cdp.evaluate("window.__settingsPagesTest.resolveStorageRead(1)");
  await waitFor(() => cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim() === "1"`), "R2 新成功先提交");
  await cdp.evaluate(`window.__settingsPagesTest.rejectStorageRead(0, "模拟过期占用失败")`);
  await settleDom();
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.storage().trashed.count"), 1);
  assert.equal(await cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim()`), "1");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page] .banner.error")`), null, "过期失败不能覆盖较新的成功");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='alert']")`), null, "过期 load(true) 不能产生过期的占用没刷新");
  assert.match(await cdp.evaluate(`document.querySelector("[data-trash-fan] [role='status']")?.textContent`), /拿回来了/);
  assertNoBrowserErrors();
});

test("Task8 第二轮 父页旧失败不覆盖新错误，销毁时在途读取安全失效", async () => {
  await openSettings("data", "trash=2&storage=1");
  await waitFor(() => cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim() === "2"`), "新旧错误场景初始状态");
  await cdp.evaluate("window.__settingsPagesTest.storageReadDeferred()");
  await clickSettingButton("失效", "清理");
  await clickSettingButton("失效", "确认清理 3 条");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 1"), "清理触发旧读 R1");
  await clickSettingButton("失效", "重新检查");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 2"), "检查触发新读 R2");
  await cdp.evaluate(`window.__settingsPagesTest.rejectStorageRead(1, "模拟新占用失败")`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page] .banner.error")?.textContent.includes("模拟新占用失败")`), "R2 新错误先提交");
  await cdp.evaluate(`window.__settingsPagesTest.rejectStorageRead(0, "模拟旧占用失败")`);
  await settleDom();
  assert.match(await cdp.evaluate(`document.querySelector("[data-settings-page] .banner.error")?.textContent`), /模拟新占用失败/);
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector("[data-settings-page] .banner.error")?.textContent`), /模拟旧占用失败/);
  assert.equal(await cdp.evaluate(`document.querySelector(".trash-count")?.textContent.trim()`), "2");
  assert.equal(await cdp.evaluate("window.__settingsPagesTest.storage().trashed.count"), 2);
  assertNoBrowserErrors();

  await openSettings("data", "trash=1&storageDeferred=1");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 1"), "销毁前父页在途读取");
  await emitNavigate("general");
  await cdp.evaluate(`window.__settingsPagesTest.rejectStorageRead(0, "销毁后的占用失败")`);
  await settleDom();
  assertNoBrowserErrors();
});

test("Task8 修复 完整清空 pending 与单项恢复/删除共享 busy，双击和失败都不能交错", async () => {
  await openSettings("trash", "trash=2");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 2`), "共享 busy 初始列表");
  await cdp.evaluate("window.__settingsPagesTest.purgeDeferred()");
  await click(".foot .danger");
  await click(".foot .danger");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingPurges() === 1"), "全清 pending");
  assert.equal(await cdp.evaluate(`[...document.querySelectorAll("ul[aria-label='回收站'] button")].every((button) => button.disabled)`), true, "全清时单项按钮全部禁用");
  await click("ul[aria-label='回收站'] > li:first-child button:first-child");
  await click("ul[aria-label='回收站'] > li:first-child .danger");
  await click(".foot .danger");
  assert.equal((await ipcCalls("restore_item")).length, 0, "全清 pending 不能再恢复");
  assert.equal((await ipcCalls("purge_items")).length, 1, "双击不能重复全清或单删");
  await cdp.evaluate("window.__settingsPagesTest.resolvePurge()");
  await waitFor(() => cdp.evaluate(`document.querySelector("ul[aria-label='回收站']")?.textContent.includes("回收站是空的")`), "全清完成");

  await openSettings("trash", "trash=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 1`), "全清失败场景");
  await cdp.evaluate("window.__settingsPagesTest.purgeDeferred()");
  await click(".foot .danger");
  await click(".foot .danger");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingPurges() === 1"), "失败全清 pending");
  await cdp.evaluate(`window.__settingsPagesTest.rejectPurge(0, "模拟全清失败")`);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-empty-trash]")?.textContent.includes("模拟全清失败")`), "全清失败提示");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length`), 1);
  assert.equal(await cdp.evaluate(`[...document.querySelectorAll("ul[aria-label='回收站'] button")].every((button) => !button.disabled)`), true, "失败后单项操作恢复可用");
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector(".foot small").textContent`), /回收站已彻底清空/);
});

test("Task8 修复 armed 后 disabled 阻止 press，销毁中的全清完成后不再重读旧组件", async () => {
  await openSettings("trash", "trash=2");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 2`), "armed disabled 场景");
  await cdp.evaluate("window.__settingsPagesTest.purgeDeferred()");
  await click(".foot .danger");
  await click("ul[aria-label='回收站'] > li:first-child .danger");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingPurges() === 1"), "单删令共享 busy 开启");
  assert.equal(await cdp.evaluate(`document.querySelector(".foot .danger").disabled`), true, "armed 也必须尊重 disabled");
  await cdp.evaluate(`(() => { const button = document.querySelector(".foot .danger"); button.disabled = false; button.click(); })()`);
  assert.equal((await ipcCalls("purge_items")).length, 1, "即使测试绕过 DOM disabled，press 入口仍不能执行全清");
  await cdp.evaluate(`window.__settingsPagesTest.rejectPurge(0, "模拟单删失败")`);

  await openSettings("trash", "trash=1");
  await waitFor(() => cdp.evaluate(`document.querySelectorAll("ul[aria-label='回收站'] > li:not(.empty)").length === 1`), "销毁全清场景");
  await cdp.evaluate("window.__settingsPagesTest.purgeDeferred()");
  await click(".foot .danger");
  await click(".foot .danger");
  await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingPurges() === 1"), "销毁前全清 pending");
  await emitNavigate("general");
  const reads = (await ipcCalls("get_trash")).length;
  await cdp.evaluate("window.__settingsPagesTest.resolvePurge()");
  await settleDom();
  assert.equal((await ipcCalls("get_trash")).length, reads, "销毁后不能调用旧组件 onlanded 重读");
  assertNoBrowserErrors();
});

test("Task8 修复 回收站剩余时间跨本机日历日也不误称今天", async () => {
  await openSettings("data", "trash=1&trashNextDay=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-card] .left")`), "主页跨日时间");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-trash-card] .left").textContent`), "不到 1 天后清掉");
  await openSettings("trash", "trash=1&trashNextDay=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("ul[aria-label='回收站'] .meta")`), "完整列表跨日时间");
  assert.match(await cdp.evaluate(`document.querySelector("ul[aria-label='回收站'] .meta").textContent`), /不到 1 天后清掉/);
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /今天就清掉/);
});

test("Task8 修复 备份与手动挑图只更新用户文案，保留检查、覆盖和清理接线", async () => {
  await openSettings("export");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page]").textContent.includes("正在统计") === false`), "导出摘要");
  let copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.doesNotMatch(copy, /历史库|校验|条目|恢复前/);
  assert.match(copy, /复制过的内容/);

  await openSettings("images");
  copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.doesNotMatch(copy, /历史库|校验|条目|恢复前/);
  assert.match(copy, /存在拈来里/);

  await openSettings("import", "backupReady=1");
  await clickButton("选择…");
  await waitFor(() => cdp.evaluate(`document.querySelector(".choices")`), "三步检查后备份就绪");
  copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.doesNotMatch(copy, /历史库|校验|条目|恢复前|确认覆盖并重启/);
  await click(`button[role="radio"]:last-child`);
  await clickButton("覆盖并重启");
  await clickButton("再点一次，换成备份");
  await waitFor(async () => (await ipcCalls("import_backup")).length === 1, "覆盖仍走原 IPC");
  assert.deepEqual((await ipcCalls("import_backup"))[0].args, { token: "test-token", mode: "overwrite" });
  assert.match(await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`), /回去之前/);

  await openSettings("import", "backupFail=1");
  await clickButton("选择…");
  await waitFor(() => cdp.evaluate(`document.querySelector(".bad")`), "检查失败真实呈现");
  assert.match(await cdp.evaluate(`document.querySelector(".bad").textContent`), /这个备份有问题，什么都没动/);
  assert.equal((await ipcCalls("import_backup")).length, 0);
});

test("存档恢复、上限、手动存档和备份入口在重排后仍可达", async () => {
  await openSettings("data", "snapshots=1");
  await clickButton("查看存档");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView === "snapshots"`), "存档子页");
  await clickButton("立即存一份");
  await waitFor(async () => (await ipcCalls("create_snapshot")).length === 1, "手动存档");
  await change("label.setting select", "512");
  await waitFor(async () => (await calls()).some((call) => call.update.snapshots?.max_mb === 512 && call.settled), "存档上限保存");
  await clickButton("回到这天");
  assert.equal((await ipcCalls("restore_snapshot")).length, 0, "第一次只确认存档恢复");
  await clickButton("再点一次，回到这天");
  await waitFor(async () => (await ipcCalls("restore_snapshot")).length === 1, "第二次真实恢复");
  assert.deepEqual((await ipcCalls("restore_snapshot"))[0].args, { name: "test-snapshot" });
  for (const view of ["export", "import", "images", "trash"]) {
    await openSettings(view);
    assert.equal(await cdp.evaluate(`document.querySelector("[data-settings-page]").dataset.storageView`), view);
    await assertComponentProof(DATA_PROOFS[view], `保留深链 ${view}`);
  }
});

test("关于显示真实版本和目录，离线许可完整可读，首次启动进行中禁用两个重放入口", async () => {
  await openSettings("about");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-about-version]")?.textContent.includes("0.0.0-test")`), "真实 appVersion");
  assert.match(await cdp.evaluate(`document.querySelector("[data-about-dir]")?.textContent`), /D:\\test-data\\nianlai/);
  assert.match(await cdp.evaluate(`document.querySelector(".about-page")?.textContent`), /拈来[\s\S]*niān lái[\s\S]*复制过的，信手拈来[\s\S]*用两三个指头/);
  assert.equal(await cdp.evaluate(`document.querySelector(".about-page img")?.naturalWidth > 0`), true, "真实应用图标");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("button[data-guide-action]:not(:disabled)").length`), 0);
  assert.deepEqual(await cdp.evaluate(`[...document.querySelectorAll("button[data-guide-action]")].map((node) => node.dataset.guideAction)`), ["replay", "deal"]);
  assert.equal((await ipcCalls()).filter((call) => /guide|deal/i.test(call.command)).length, 0);
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector(".about-page").textContent`), /已打开|发好了/);

  await clickButton("打开文件夹");
  assert.deepEqual((await ipcCalls("open_data_dir")), [{ command: "open_data_dir", args: {} }]);
  await cdp.evaluate(`window.__licenseFetches = 0; window.fetch = new Proxy(window.fetch, { apply(target, self, args) { window.__licenseFetches += 1; return Reflect.apply(target, self, args); } })`);
  await clickButton("查看许可");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), "本地许可查看层");
  const licenses = await cdp.evaluate(`document.querySelector("[data-license-dialog]").textContent`);
  assert.match(licenses, /MIT License/);
  assert.match(licenses, /Nianlai contributors/);
  assert.match(licenses, /THE SOFTWARE IS PROVIDED "AS IS"/);
  assert.match(licenses, /THIRD_PARTY_NOTICES|第三方许可说明/);
  assert.match(licenses, /rusqlite 0\.41\.1/);
  assert.match(licenses, /完整性限制/);
  assert.equal(await cdp.evaluate("window.__licenseFetches"), 0, "许可不能发网络请求");
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-license-dialog] a[href^='http']").length`), 0);
  await nativeKey("[data-license-dialog]", ESCAPE_KEY);
  await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]")`), "Escape 关闭许可");
  assert.match(await cdp.evaluate(`document.activeElement?.textContent`), /查看许可/);
});

test("Task14 关于页说明区直接区分重看、重发与只重试开窗", async () => {
  await openSettings("about");
  const notes = await cdp.evaluate(`document.querySelector("[data-settings-notes]").textContent`);
  assert.doesNotMatch(notes, /两个首次启动入口还没接入|当前只留位置/);
  assert.match(notes, /“再看一次”从第一拍重新保存引导/);
  assert.match(notes, /“再发一遍”只把四张新手牌放回最近/);
  assert.match(notes, /只重试开窗，不会重复保存引导进度/);
});

test("Task14 隐私页排除程序使用直白功能文案", async () => {
  await openSettings("privacy");
  const copy = await cdp.evaluate(`document.querySelector("[data-settings-page]").textContent`);
  assert.match(copy, /名单里的程序，复制的内容不记/);
  assert.doesNotMatch(copy, /扣着的牌，就是这个程序里复制的不记/);
});

test("关于版本和目录读取失败可读降级，不写死版本或猜目录", async () => {
  await openSettings("about", "versionError=1&dirError=1");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-about-version]")?.textContent.includes("版本没读到")`), "版本失败降级");
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-about-dir]")?.textContent.includes("目录没读到")`), "目录失败降级");
  assert.doesNotMatch(await cdp.evaluate(`document.querySelector(".about-page").textContent`), /0\.1\.0-alpha\.3|AppData/);
});

test("Task9 修复 RollingNumber 长滚动中切换两类减少动画后旧 RAF 不覆盖新计数", async () => {
  const observations = [];
  for (const state of [VISUAL_STATES[2], SYSTEM_REDUCED]) {
    await openSettings("data", "trash=10&storage=1&storageDeferred=1");
    await configureVisualState(VISUAL_STATES[0]);
    await waitFor(() => cdp.evaluate("window.__settingsPagesTest.pendingStorageReads() === 1"), `${state.id} 初始占用读取受控等待`);
    await cdp.evaluate("window.__settingsPagesTest.resolveStorageRead(0); window.__settingsPagesTest.storageReadDeferred(false)");
    await waitFor(() => cdp.evaluate(`(() => {
      const shown = document.querySelector(".trash-count .rolling")?.getAttribute("aria-label");
      return shown !== "0" && shown !== "10" && shown !== undefined;
    })()`), `${state.id} 0→10 长滚动的真实中间帧`, 1000);
    const before = await cdp.evaluate(`document.querySelector(".trash-count .rolling").getAttribute("aria-label")`);

    await configureVisualState(state);
    assert.equal(await cdp.evaluate("document.documentElement.dataset.motion"), state.motion);
    assert.equal(await cdp.evaluate("document.documentElement.dataset.reduceMotion"), state.systemReduce ? "true" : "false");
    await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
    await waitFor(() => cdp.evaluate(`document.querySelector("[data-trash-fan] [role='status']")?.textContent.includes("拿回来了")`), `${state.id} 新真实计数已提交父页`, 1000);
    const actual = await cdp.evaluate("window.__settingsPagesTest.storage().trashed.count");
    const immediate = await cdp.evaluate(`document.querySelector(".trash-count .rolling").getAttribute("aria-label")`);
    assert.equal(actual, 9, `${state.id} 内存真实计数必须在旧动画时长内变成九`);

    await new Promise((resolve) => setTimeout(resolve, 800));
    const final = await cdp.evaluate(`document.querySelector(".trash-count .rolling").getAttribute("aria-label")`);
    const observation = { state: state.id, before, actual, immediate, final };
    observations.push(observation);
    console.log(`Task9 RAF 中途切换 ${JSON.stringify(observation)}`);
  }
  const failures = observations.filter((entry) => entry.immediate !== "9" || entry.final !== "9");
  assert.deepEqual(failures, [], `两类减少动画都必须立即显示新 target，且旧 RAF 成功闭包不能覆盖它；${JSON.stringify(observations)}`);
});

test("Task9 修复 ConfirmButton 飞行中切换两类减少动画仍只落地一次", async () => {
  const observations = [];
  for (const state of [VISUAL_STATES[2], SYSTEM_REDUCED]) {
    await openVisualScene("data", VISUAL_STATES[0]);
    const readsBefore = (await ipcCalls("get_storage")).length;
    await clickSettingButton("失效", "清理");
    await clickSettingButton("失效", "确认清理 3 条");
    await waitFor(() => cdp.evaluate(`(() => {
      const chip = document.querySelector(".fly-chip");
      return chip && chip.getAnimations().some((animation) => animation.playState === "running");
    })()`), `${state.id} ConfirmButton 真实飞行中间帧`, 1500);

    await configureVisualState(state);
    await new Promise((resolve) => setTimeout(resolve, 100));
    const earlyChip = await cdp.evaluate(`!!document.querySelector(".fly-chip")`);
    const earlyLanded = (await ipcCalls("get_storage")).length - readsBefore;
    await waitFor(() => cdp.evaluate(`!document.querySelector(".fly-chip")`), `${state.id} 飞行句柄清理`, 1500);
    await waitFor(async () => (await ipcCalls("get_storage")).length - readsBefore === 1, `${state.id} onlanded 完成一次`, 1500);
    await settleVisual();
    const finalLanded = (await ipcCalls("get_storage")).length - readsBefore;
    const observation = { state: state.id, earlyChip, earlyLanded, finalLanded };
    observations.push(observation);
    console.log(`Task9 ConfirmButton 动态取消 ${JSON.stringify(observation)}`);
  }
  const failures = observations.filter((entry) => entry.earlyChip || entry.earlyLanded !== 1 || entry.finalLanded !== 1);
  assert.deepEqual(failures, [], `切换后 100ms 内必须取消飞行并落地一次，旧时长后也不能再次落地；${JSON.stringify(observations)}`);
});

test("Task9 修复 RollingNumber bump 中途切换两类减少动画立即取消 WAAPI", async () => {
  const observations = [];
  for (const state of [VISUAL_STATES[2], SYSTEM_REDUCED]) {
    await openVisualScene("data", VISUAL_STATES[0]);
    await clickSettingButton("失效", "清理");
    await clickSettingButton("失效", "确认清理 3 条");
    await waitFor(() => cdp.evaluate(`document.querySelector(".trash-count .rolling")?.getAnimations().some((animation) => animation.playState === "running")`), `${state.id} bump 真实中间帧`, 2000);
    const before = await cdp.evaluate(`document.querySelector(".trash-count .rolling").getAnimations().map((animation) => ({ playState: animation.playState, currentTime: animation.currentTime }))`);
    await configureVisualState(state);
    const after = await cdp.evaluate(`(() => {
      const rolling = document.querySelector(".trash-count .rolling");
      return { animations: rolling.getAnimations().map((animation) => ({ playState: animation.playState, currentTime: animation.currentTime })), transform: getComputedStyle(rolling).transform };
    })()`);
    const observation = { state: state.id, before, after };
    observations.push(observation);
    console.log(`Task9 RollingNumber bump 动态取消 ${JSON.stringify(observation)}`);
    await settleVisual();
  }
  const failures = observations.filter((entry) => entry.after.animations.some((animation) => animation.playState === "running") || entry.after.transform !== "none");
  assert.deepEqual(failures, [], `进行中 bump 必须随两类减少动画立即取消并恢复无变换；${JSON.stringify(observations)}`);
});

test("Task9 可见文字 computed font-size 只含四档，六页与数据子页三态无横向溢出和禁用词", async () => {
  for (const state of VISUAL_STATES) {
    const reduced = state.motion === "none";
    for (const page of MAIN_PAGES) {
      await openVisualScene(page, state);
      await assertVisualAudit(`${state.id}/${page}`, { reduced });
      if (state.theme === "dark") await assertDarkSurfaces(`${state.id}/${page}`,
        page === "fav" ? ["favorite", "seal"] : page === "data" ? ["trash"] : page === "about" ? ["about"] : []);
      if (page === "fav") {
        await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
        await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true"`), `${state.id} 常用牌真实翻背`);
        await settleVisual();
        await assertVisualAudit(`${state.id}/fav/牌背`, { reduced });
        if (state.theme === "dark") await assertDarkSurfaces(`${state.id}/fav/牌背`, ["favoriteBack"]);
      }
      if (page === "data") {
        await clickSettingButton("失效", "清理");
        await assertVisualAudit(`${state.id}/data/ConfirmButton/armed`, { reduced });
        const readsBefore = (await ipcCalls("get_storage")).length;
        await clickSettingButton("失效", "确认清理 3 条");
        await waitFor(() => cdp.evaluate(`!!document.querySelector(".confirm-button.done")`), `${state.id} ConfirmButton done`);
        await assertVisualAudit(`${state.id}/data/ConfirmButton/done`, { reduced });
        await waitFor(async () => (await ipcCalls("get_storage")).length === readsBefore + 1, `${state.id} ConfirmButton onlanded 一次`);
        await waitFor(() => cdp.evaluate(`!document.querySelector(".fly-chip")`), `${state.id} 飞行清理后再离开场景`);
      }
      if (page === "about") {
        await clickButton("查看许可");
        await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), `${state.id} 本地许可层`);
        await assertVisualAudit(`${state.id}/about/许可`, { reduced });
        if (state.theme === "dark") await assertDarkSurfaces(`${state.id}/about/许可`, ["dialog"]);
        await clickButton("关闭", `document.querySelector("[data-license-dialog]")`);
        await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]")`), "关闭许可层后继续后续场景");
      }
    }
    for (const [view, proof] of Object.entries(DATA_PROOFS)) {
      await openSettings(view, "trash=10&storage=1&snapshots=1&recent=1&backupReady=1");
      await configureVisualState(state);
      const sceneProof = view === "trash" ? { selector: `ul[aria-label="回收站"] > li:not(.empty)`, count: 10 } : proof;
      await assertComponentProof(sceneProof, `${state.id}/数据子页 ${view}`);
      await assertVisualAudit(`${state.id}/data/${view}`, { reduced });

      if (view === "export") {
        await clickButton("导出");
        await waitFor(() => cdp.evaluate(`document.querySelector("button.primary.done") && document.querySelector(".result")`), `${state.id} BackupExport done`);
        await assertVisualAudit(`${state.id}/data/export/done`, { reduced });
        assert.equal((await ipcCalls("export_backup")).length, 1, `${state.id} 导出完成态必须来自真实内存 IPC`);
      }
      if (view === "import") {
        await cdp.evaluate("window.__settingsPagesTest.installClock()");
        await clickButton("选择…");
        await waitFor(() => cdp.evaluate(`!!document.querySelector(".step.running")`), `${state.id} BackupImport checking`);
        await assertVisualAudit(`${state.id}/data/import/checking`, { reduced });
        await finishBackupInspection(`${state.id}/import/ready`);
        await assertVisualAudit(`${state.id}/data/import/ready`, { reduced });
        await clickButton("合并");
        await waitFor(() => cdp.evaluate(`!!document.querySelector(".done")`), `${state.id} BackupImport merged`);
        await cdp.evaluate("window.__settingsPagesTest.tickClock(500)");
        await settleDom();
        await assertVisualAudit(`${state.id}/data/import/merged`, { reduced });

        await openSettings("import", "trash=10&storage=1&backupFail=1");
        await configureVisualState(state);
        await cdp.evaluate("window.__settingsPagesTest.installClock()");
        await clickButton("选择…");
        await waitFor(() => cdp.evaluate(`!!document.querySelector(".step.running")`), `${state.id} BackupImport failed 前检查`);
        await finishBackupInspection(`${state.id}/import/failed`);
        await assertVisualAudit(`${state.id}/data/import/failed`, { reduced });
      }
      if (view === "trash") {
        await cdp.evaluate("window.__settingsPagesTest.purgeDeferred()");
        await click(".foot .confirm-button");
        await click(".foot .confirm-button");
        await waitFor(() => cdp.evaluate(`!!document.querySelector(".confirm-button.working")`), `${state.id} ConfirmButton working`);
        assert.equal(await cdp.evaluate(`document.querySelector(".confirm-button.working").disabled`), true, `${state.id} working 必须保持 disabled`);
        assert.equal(await cdp.evaluate(`[...document.querySelectorAll("ul[aria-label='回收站'] button")].every((button) => button.disabled)`), true, `${state.id} onpending busy 必须禁用交错操作`);
        await assertVisualAudit(`${state.id}/data/trash/ConfirmButton/working`, { reduced });
        await cdp.evaluate("window.__settingsPagesTest.resolvePurge()");
        await waitFor(() => cdp.evaluate(`document.querySelector("ul[aria-label='回收站']")?.textContent.includes("回收站是空的")`), `${state.id} working 完成后真实清理`);
      }
    }
  }
});

test("Task9 system dark 的常用牌正背、八张回收站与许可层使用显式 dark 相同语义表面", async () => {
  for (const page of ["fav", "data", "about"]) {
    await openVisualScene(page, SYSTEM_DARK);
    assert.equal(await cdp.evaluate("document.documentElement.dataset.theme"), "system", `${page} 根 dataset 必须保留 system`);
    assert.equal(await cdp.evaluate(`matchMedia("(prefers-color-scheme: dark)").matches`), true, `${page} 必须来自真实 Edge 系统深色仿真`);
    if (page === "fav") {
      await assertDarkSurfaces("system-dark/fav/正面", ["favorite", "seal"]);
      await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
      await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true"`), "system dark 真实翻背");
      await settleVisual();
      await assertDarkSurfaces("system-dark/fav/牌背", ["favoriteBack"]);
    }
    if (page === "data") {
      assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 8, "system dark 必须真实摊开八张");
      await assertDarkSurfaces("system-dark/data/八张摊开", ["trash"]);
    }
    if (page === "about") {
      await assertDarkSurfaces("system-dark/about/主页", ["about"]);
      await clickButton("查看许可");
      await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), "system dark 真实许可层");
      await assertDarkSurfaces("system-dark/about/许可", ["dialog"]);
      await clickButton("关闭", `document.querySelector("[data-license-dialog]")`);
      await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]")`), "system dark 许可关闭");
    }
    assert.equal(await cdp.evaluate("document.documentElement.dataset.theme"), "system", `${page} 交互后仍保持 system 来源`);
  }
});

test("Task9 默认、最小与窄窗口的六页三态导航、保存与主要按钮均可达", async () => {
  for (const [width, height] of [[1120, 680], [860, 560], [720, 560]]) {
    for (const state of VISUAL_STATES) {
      for (const page of MAIN_PAGES) {
        await openVisualScene(page, state);
        await setViewport(width, height);
        await settleVisual();
        assert.equal(await cdp.evaluate(`document.querySelectorAll("button[data-page]").length`), 6);
        assert.equal(await cdp.evaluate(`document.querySelector("button[data-page='${page}']").getAttribute("aria-current")`), "page");
        await assertVisualAudit(`${width}x${height}/${state.id}/${page}`, { reduced: state.motion === "none" });
      }
    }
  }
});

test("Task9 应用内关闭动效与跟随系统减少动画都可翻背、摊开和恢复，不只是改标记", async () => {
  for (const state of [VISUAL_STATES[2], SYSTEM_REDUCED]) {
    await openVisualScene("fav", state);
    await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
    await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true" && document.activeElement?.dataset.favoriteField === "title"`), `${state.id} 翻背焦点`);
    await assertVisualAudit(`${state.id}/fav/减少动画`, { reduced: true });
    await fill('[data-favorite-card][data-id="101"] [data-favorite-field="title"]', `${state.id} 仍可编辑`);
    await click('[data-favorite-card][data-id="101"] [data-action="close-card"]');
    await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "false" && document.activeElement?.hasAttribute("data-favorite-front")`), `${state.id} 返回焦点`);
    assert.equal(await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"] [data-favorite-front]').getAttribute("aria-label").includes("仍可编辑")`), true);

    await openVisualScene("data", state);
    await assertVisualAudit(`${state.id}/data/减少动画`, { reduced: true });
    await clickButton("拿回来", `document.querySelector('[data-trash-card][data-id="700"]')`);
    await waitFor(() => cdp.evaluate(`window.__settingsPagesTest.trashItems().length === 9 && !!document.querySelector('[data-trash-card][data-id="708"]')`), `${state.id} 回收站真实恢复并补入下一张`);
    await cdp.evaluate("document.activeElement?.blur()");
    await tabUntil(`document.activeElement?.matches("[data-trash-fan] .fan-actions button:last-child")`, `${state.id} 回收站摊开/收起按钮`);
    await keyPair(SPACE_KEY);
    assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 0, `${state.id} Space 真实收起`);
    assert.equal(await cdp.evaluate(`document.activeElement?.getAttribute("aria-expanded")`), "false", `${state.id} 收起后焦点保留`);
    await keyPair(ENTER_KEY);
    await waitFor(() => cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length === 8`), `${state.id} Enter 收起后重新摊开`);
    assert.equal(await cdp.evaluate(`document.activeElement?.getAttribute("aria-expanded")`), "true", `${state.id} 重新摊开后焦点保留`);

    await openVisualScene("about", state);
    await setViewport(720, 560);
    await cdp.evaluate("document.activeElement?.blur()");
    await tabUntil(`document.activeElement?.textContent.trim() === "查看许可"`, `${state.id} 720×560 查看许可入口`);
    await keyPair(ENTER_KEY);
    await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), `${state.id} Enter 打开窄窗口许可`);
    await tabUntil(`document.activeElement?.matches("[data-license-dialog] .license-scroll")`, `${state.id} 许可正文滚动区`, 4);
    await assertVisualAudit(`${state.id}/720x560/about/许可键盘与遮挡`, { reduced: true });
    await keyPair(ESCAPE_KEY);
    await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]") && document.activeElement?.textContent.includes("查看许可")`), `${state.id} Escape 关闭与焦点返回`);
    await keyPair(SPACE_KEY);
    await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), `${state.id} Space 再次打开许可`);
    assert.equal(await cdp.evaluate(`document.activeElement?.matches("[data-license-dialog] .license-head button")`), true, `${state.id} 原生 dialog 焦点先到关闭`);
    await keyPair(ENTER_KEY);
    await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]") && document.activeElement?.textContent.includes("查看许可")`), `${state.id} Enter 关闭与焦点返回`);
  }
});

test("Task9 forced-colors 的文字、边界、焦点、按钮与藏印可辨，许可层键盘可关闭", async () => {
  await openVisualScene("fav", VISUAL_STATES[0]);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  assert.equal(await cdp.evaluate(`matchMedia("(forced-colors: active)").matches`), true, "Edge 支持 forced-colors 仿真");
  await nativeKey('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true"`), "forced-colors 翻背");
  const favorite = await cdp.evaluate(`(() => {
    const seal = getComputedStyle(document.querySelector(".favorite-seal"));
    const inputNode = document.querySelector('[data-favorite-field="title"]');
    const closeNode = document.querySelector('[data-action="close-card"]');
    const input = getComputedStyle(inputNode);
    const close = getComputedStyle(closeNode);
    return { sealBackground: seal.backgroundColor, sealColor: seal.color, inputColor: input.color,
      inputBackground: input.backgroundColor, inputCanvas: getComputedStyle(inputNode.closest(".favorite-back")).backgroundColor,
      inputBorder: input.borderTopStyle, inputBorderWidth: input.borderTopWidth,
      closeColor: close.color, closeBackground: close.backgroundColor, closeCanvas: getComputedStyle(closeNode.closest(".favorite-back")).backgroundColor };
  })()`);
  assert.notEqual(favorite.sealBackground, favorite.sealColor, "藏印文字与底色不同");
  assert.notEqual(favorite.inputColor, favorite.inputBackground, "输入文字与底色不同");
  assert.notEqual(favorite.inputColor, favorite.inputCanvas, "输入文字与相邻 Canvas 不同");
  assert.equal(favorite.inputBorder, "solid");
  assert.equal(favorite.inputBorderWidth, "1px");
  assert.notEqual(favorite.closeColor, favorite.closeBackground, "返回按钮仍可辨");
  assert.notEqual(favorite.closeColor, favorite.closeCanvas, "透明返回按钮文字与相邻 Canvas 不同");
  await click('[data-favorite-card][data-id="101"] [data-action="close-card"]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "false" && document.activeElement?.hasAttribute("data-favorite-front")`), "forced-colors 焦点回正面");
  await nativeKey('[data-favorite-card][data-id="101"] [data-favorite-front]', { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
  const focus = await cdp.evaluate(`(() => {
    const active = document.activeElement;
    const style = getComputedStyle(active);
    return { style: style.outlineStyle, width: style.outlineWidth, color: style.outlineColor,
      canvas: getComputedStyle(active.closest(".main") ?? document.documentElement).backgroundColor };
  })()`);
  assert.equal(focus.style, "solid");
  assert.notEqual(focus.width, "0px");
  assert.notEqual(focus.color, focus.canvas, "焦点轮廓与相邻 Canvas 实际颜色不同");

  await openVisualScene("data", VISUAL_STATES[0]);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  const trash = await cdp.evaluate(`(() => {
    const card = getComputedStyle(document.querySelector("[data-trash-card]"));
    const button = getComputedStyle(document.querySelector("[data-trash-card] .restore"));
    return { border: card.borderTopStyle, width: card.borderTopWidth, color: card.color, background: card.backgroundColor,
      buttonColor: button.color, buttonBackground: button.backgroundColor, buttonBorder: button.borderTopStyle };
  })()`);
  assert.equal(trash.border, "solid");
  assert.equal(trash.width, "1px");
  assert.notEqual(trash.color, trash.background);
  assert.notEqual(trash.buttonColor, trash.buttonBackground);
  assert.notEqual(trash.buttonColor, trash.background, "回收站按钮文字与相邻 Canvas 不同");
  assert.equal(trash.buttonBorder, "solid");

  await openVisualScene("about", VISUAL_STATES[0]);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  await cdp.evaluate("document.activeElement?.blur()");
  await tabUntil(`document.activeElement?.textContent.trim() === "查看许可"`, "forced-colors 查看许可入口");
  await keyPair(ENTER_KEY);
  await waitFor(() => cdp.evaluate(`document.querySelector("[data-license-dialog]")?.open === true`), "forced-colors 许可层");
  await assertVisualAudit("forced-colors/about/许可");
  const dialog = await cdp.evaluate(`(() => {
    const dialog = getComputedStyle(document.querySelector("[data-license-dialog]"));
    const close = getComputedStyle(document.querySelector("[data-license-dialog] .license-head button"));
    return { border: dialog.borderTopStyle, width: dialog.borderTopWidth, color: dialog.color, background: dialog.backgroundColor,
      closeColor: close.color, closeBackground: close.backgroundColor, closeCanvas: dialog.backgroundColor };
  })()`);
  assert.equal(dialog.border, "solid");
  assert.equal(dialog.width, "1px");
  assert.notEqual(dialog.color, dialog.background);
  assert.notEqual(dialog.closeColor, dialog.closeBackground);
  assert.notEqual(dialog.closeColor, dialog.closeCanvas, "许可关闭文字与相邻 Canvas 不同");
  await keyPair(ESCAPE_KEY);
  await waitFor(() => cdp.evaluate(`!document.querySelector("[data-license-dialog]") && document.activeElement?.textContent.includes("查看许可")`), "Escape 关闭后焦点回查看许可");
  console.log("Task9 forced-colors：仅为无头 Edge 媒体仿真，不代表真实 Windows 高对比度、WebView2 或系统主题已通过。");
});

test("Task9 真实六页三态截图、常用牌翻背、回收站八张摊开与系统减少动画证据", async () => {
  for (const state of VISUAL_STATES) {
    for (const page of MAIN_PAGES) {
      await openVisualScene(page, state);
      if (page === "general") {
        await clickButton("演示一次摊开");
        await waitFor(() => cdp.evaluate(`document.querySelector("[data-mini-preview]")?.dataset.demoState === "spread"`), `${state.id} 通用页真实演示摊开`);
        await settleVisual();
      }
      await captureShot(`${page}-${state.id}.png`, {
        page, state: state.id,
        themeSource: `真实设置 theme=${state.theme}`,
        motionSource: `真实设置 motion=${state.motion}; follow_system=${state.followSystem}; prefers-reduced-motion=${state.systemReduce}`,
      });
    }
  }

  await openVisualScene("fav", VISUAL_STATES[0]);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true" && document.activeElement?.dataset.favoriteField === "title"`), "翻背截图真实焦点");
  await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').scrollIntoView({ block: "center", inline: "center" })`);
  await captureShot("fav-flip-light.png", { page: "fav", state: "flip", themeSource: "真实设置 theme=light", motionSource: "真实设置 motion=spring; 非减少动画" });

  await openVisualScene("data", VISUAL_STATES[0]);
  assert.equal(await cdp.evaluate(`document.querySelectorAll("[data-trash-fan] [data-trash-card]").length`), 8, "截图必须包含真实八张回收站内容");
  await cdp.evaluate(`document.querySelector("[data-trash-fan]").scrollIntoView({ block: "start", inline: "nearest" })`);
  await captureShot("data-trash-fan-light.png", { page: "data", state: "trash-fan", themeSource: "真实设置 theme=light", motionSource: "真实设置 motion=spring; 非减少动画" });

  await openVisualScene("fav", SYSTEM_REDUCED);
  await click('[data-favorite-card][data-id="101"] [data-favorite-front]');
  await waitFor(() => cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').dataset.flipped === "true" && document.activeElement?.dataset.favoriteField === "title"`), "跟随系统减少动画翻背");
  await cdp.evaluate(`document.querySelector('[data-favorite-card][data-id="101"]').scrollIntoView({ block: "center", inline: "center" })`);
  await captureShot("fav-flip-system-reduced.png", { page: "fav", state: "system-reduced-flip", themeSource: "真实设置 theme=light", motionSource: "follow_system=true + Edge prefers-reduced-motion=reduce" });

  assert.equal(shotManifest.length, 21, "18 张六页三态 + 常用翻背 + 回收站摊开 + 跟随系统翻背");
  assert.equal(new Set(shotManifest.map((shot) => shot.name)).size, shotManifest.length, "截图文件名不重复");
});

test("组件运行没有未捕获浏览器异常", async () => {
  await openSettings("trash");
  await emitNavigate("general");
  assert.deepEqual(browserErrors, []);
});
