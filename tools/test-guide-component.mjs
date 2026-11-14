// 真实 Guide.svelte + 生产 GuideFlow / ipc.ts 的无头 Edge 测试。
// 只 alias Tauri core/event/window；所有复制、面板与粘贴结果均由测试显式注入，不触碰系统剪贴板或按键。
import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer as createNetServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const root = fileURLToPath(new URL("../", import.meta.url));
const temporaryBase = resolve(tmpdir());
const shotDirectory = resolve(root, "internal/design/v11-around/shots/implementation");
const shotManifest = [];
mkdirSync(shotDirectory, { recursive: true });
let temporary;
let vite;
let edge;
let cdp;
let pageUrl;
let navigationId = 0;
const browserErrors = [];
const edgeStderr = [];

async function freePort() {
  const server = createNetServer();
  await new Promise((resolve, reject) => server.once("error", reject).listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}
async function waitFor(predicate, description, timeout = 12000) {
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
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text ?? "浏览器表达式失败");
    return result.result?.value;
  }
  close() { this.ws.close(); }
}
function edgePath() {
  const candidates = [process.env.EDGE_PATH,
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"].filter(Boolean);
  const found = candidates.find((path) => existsSync(path));
  if (!found) throw new Error("找不到 Edge；可用 EDGE_PATH 指定 msedge.exe");
  return found;
}

before(async () => {
  temporary = mkdtempSync(join(temporaryBase, "nianlai-guide-component-"));
  const vitePort = await freePort();
  const debugPort = await freePort();
  const ipcPath = resolve(root, "tools/guideTestIpc.mjs").replace(/\\/g, "/");
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
  pageUrl = `http://127.0.0.1:${vitePort}/tools/guideTest.html`;
  edge = spawn(edgePath(), [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--disable-component-update", "--disable-sync",
    `--remote-debugging-port=${debugPort}`, `--user-data-dir=${join(temporary, "edge-profile")}`, "about:blank",
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
  cdp?.close();
  await vite?.close();
  if (edge && edge.exitCode === null) {
    const exited = new Promise((resolve) => edge.once("exit", resolve));
    edge.kill();
    await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 3000))]);
  }
  if (temporary) {
    assert.equal(dirname(temporary), temporaryBase);
    assert.ok(temporary.includes("nianlai-guide-component-"));
    for (let attempt = 0; ; attempt += 1) {
      try { rmSync(temporary, { recursive: true, force: true }); break; }
      catch (error) {
        if (attempt >= 5 || !["EPERM", "EBUSY"].includes(error.code)) throw error;
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
    }
  }
  const stderr = edgeStderr.join("").trimEnd();
  if (stderr) console.error(`\n[guide 无头 Edge 非阻断 stderr 原文]\n${stderr}\n[/guide 无头 Edge stderr]\n`);
});
afterEach(() => {
  try { assert.deepEqual(browserErrors, [], "任何场景都不能留下未捕获浏览器异常"); }
  finally { browserErrors.length = 0; }
});

const settle = () => cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
const api = (expression) => cdp.evaluate(`window.__guideTest.${expression}`);
const phase = () => cdp.evaluate(`document.querySelector("[data-guide-paper]")?.dataset.phase`);
const paperText = () => cdp.evaluate(`document.querySelector("[data-guide-paper]")?.textContent ?? ""`);
const saveCalls = () => api("saveCalls()");
const pendingSaves = () => api("pendingSaves()");
const invokeCalls = (command) => api(`invokeCalls(${JSON.stringify(command)})`);
const targetedEvents = () => api("targetedEvents()");
const closeCalls = () => api("closeCalls()");
const click = (action) => cdp.evaluate(`(() => {
  const button = document.querySelector(${JSON.stringify(`[data-action="${action}"]`)});
  if (!button) throw new Error("找不到按钮 ${action}");
  button.click();
})()`);
async function emit(name, payload = {}) {
  await api(`emit(${JSON.stringify(name)}, ${JSON.stringify(payload)})`);
  await settle();
}
async function hidePanel() {
  await api("hidePanel()");
  await settle();
}
async function paste(id, result, hide = false) {
  await api(`pasteResult(${JSON.stringify(id)}, ${JSON.stringify(result)}, ${JSON.stringify(hide)})`);
  await settle();
}
async function openGuide(extra = "", reducedMotion = false) {
  const navigation = ++navigationId;
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 360, height: 360, deviceScaleFactor: 1, mobile: false });
  await cdp.send("Emulation.setEmulatedMedia", { features: reducedMotion ? [{ name: "prefers-reduced-motion", value: "reduce" }] : [] });
  const target = `${pageUrl}?scenario=component-${navigation}&nav=${navigation}${extra ? `&${extra}` : ""}`;
  await cdp.send("Page.navigate", { url: target });
  await waitFor(() => cdp.evaluate(`location.search.includes("nav=${navigation}") && !!window.__guideTest && !!document.querySelector("[data-guide-paper]")`), `guide 导航 ${navigation}`);
  await settle();
}
async function resolveNextSave() {
  const ids = await waitFor(async () => {
    const pending = await pendingSaves();
    return pending.length ? pending : null;
  }, "一次待确认保存");
  await api(`resolveSave(${ids[0]})`);
  await settle();
  return ids[0];
}
async function rejectNextSave(reason = "磁盘不让写") {
  const ids = await waitFor(async () => {
    const pending = await pendingSaves();
    return pending.length ? pending : null;
  }, "一次待拒绝保存");
  await api(`rejectSave(${ids[0]}, ${JSON.stringify(reason)})`);
  await settle();
  return ids[0];
}
async function openReady(extra = "", reducedMotion = false) {
  await openGuide(`manualSaves=1${extra ? `&${extra}` : ""}`, reducedMotion);
  await resolveNextSave();
  assert.equal(await phase(), "await_copy");
}
async function openShow(extra = "", reducedMotion = false) {
  await openReady(extra, reducedMotion);
  await api("copyLatest()");
  await emit("history:changed");
  await resolveNextSave();
  assert.equal(await phase(), "await_show");
}
async function openPaste(extra = "", reducedMotion = false) {
  await openShow(extra, reducedMotion);
  await click("confirm-target");
  await waitFor(async () => (await invokeCalls("guide_open_panel")).length === 1, "真实 guide_open_panel");
  await waitFor(async () => (await api("pendingSaves()")).length === 1, "替身真实展示产生新 session 与第三拍候选");
  await resolveNextSave();
  assert.equal(await phase(), "await_paste");
  assert.equal(await api("panelState().visible"), true);
}
async function captureShot(name, metadata) {
  await settle();
  for (const action of metadata.visibleActions ?? []) {
    const selector = `[data-action="${action}"]`;
    const visible = await cdp.evaluate(`(() => {
      const button = document.querySelector(${JSON.stringify(selector)});
      const paper = document.querySelector("[data-guide-paper]");
      if (!button || !paper) return { missing: true };
      const rect = button.getBoundingClientRect();
      const bounds = paper.getBoundingClientRect();
      return { top: rect.top, bottom: rect.bottom, paperTop: bounds.top, paperBottom: bounds.bottom };
    })()`);
    assert.ok(!visible.missing && visible.top >= visible.paperTop - 1 && visible.bottom <= visible.paperBottom + 1,
      `${name} 的 ${action} 必须在纸笺可视范围内：${JSON.stringify(visible)}`);
  }
  const { data } = await cdp.send("Page.captureScreenshot", { format: "png", fromSurface: true, captureBeyondViewport: false });
  const bytes = Buffer.from(data, "base64");
  assert.equal(bytes.subarray(1, 4).toString(), "PNG");
  assert.deepEqual([bytes.readUInt32BE(16), bytes.readUInt32BE(20)], [360, 360]);
  const filename = name.endsWith(".png") ? name : `${name}.png`;
  writeFileSync(resolve(shotDirectory, filename), bytes);
  shotManifest.push({ name: filename, file: `internal/design/v11-around/shots/implementation/${filename}`, width: 360, height: 360, ...metadata });
  writeFileSync(resolve(shotDirectory, "guide-manifest.json"), JSON.stringify(shotManifest, null, 2) + "\n", "utf8");
}

test("组件：完整三拍只由真实历史、session/ID 与持久化成功推进", async () => {
  await openReady();
  const baseline = await saveCalls();
  assert.equal(baseline.length, 1);
  assert.deepEqual(baseline[0].update.guide, {
    version: 1, phase: "await_copy", baseline_ms: 100, baseline_total: 1,
    sample_id: null, cards_dealt: false, dismissed: [],
  });
  await click("copy-sample");
  assert.equal((await invokeCalls("guide_copy_sample")).length, 1);
  assert.equal(await phase(), "await_copy", "调用示例复制不等于真实记下");
  await api("copyLatest()");
  await emit("history:changed");
  assert.equal(await phase(), "await_copy", "复制候选保存前不能可见推进");
  await resolveNextSave();
  assert.equal(await phase(), "await_show");
  assert.match(await paperText(), /这是刚复制并真实记下的一句话/);
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.animationCount`), "1");
  assert.equal((await invokeCalls("guide_open_panel")).length, 0);

  await emit("guide:panel-shown", { session: 10 });
  assert.equal(await phase(), "await_show", "未确认可输入位置时面板事件不能推进");
  await click("confirm-target");
  assert.equal((await invokeCalls("guide_open_panel")).length, 1);
  await waitFor(async () => (await pendingSaves()).length === 1, "生产入口触发真实展示事件与第三拍候选");
  await resolveNextSave();
  assert.equal(await phase(), "await_paste");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`), "11");
  const beforeInvalid = (await saveCalls()).length;
  for (const payload of [
    { session: 10, id: 2, result: "sent" },
    { session: 11, id: 1, result: "confirmed" },
    { session: null, id: 2, result: "sent" },
  ]) await emit("guide:paste-result", payload);
  assert.equal((await saveCalls()).length, beforeInvalid);
  assert.equal(await phase(), "await_paste");

  await paste(2, "confirmed", true);
  assert.equal(await phase(), "await_paste", "生产 shown→hidden→result 后，完成保存前仍停留在第三拍");
  assert.equal(await closeCalls(), 0);
  assert.equal((await targetedEvents()).length, 0);
  await resolveNextSave();
  assert.equal(await phase(), "completed");
  assert.equal((await api("settings()")).guide.cards_dealt, true);
  assert.equal(await closeCalls(), 1);
  assert.deepEqual(await targetedEvents(), [{ target: "panel", name: "guide:cards-dealt", payload: { cards_dealt: true } }]);
  await emit("guide:paste-result", { session: 11, id: 2, result: "sent" });
  assert.equal(await closeCalls(), 1);
  assert.equal((await targetedEvents()).length, 1);
});

test("组件：persist_guide 严格串行，迟到回包不覆盖新样本，失败可重试", async () => {
  await openGuide("manualSaves=1");
  const initialToken = (await pendingSaves())[0];
  await api("copyLatest()");
  await emit("history:changed");
  await api("copyNewer()");
  await emit("history:changed");
  assert.equal((await saveCalls()).length, 1, "前一个 setSettings 未完成时不能并发写下一个 token");
  await api(`resolveSave(${initialToken})`);
  await settle();
  await waitFor(async () => (await saveCalls()).length === 2, "第二个保存进入执行器");
  assert.equal(await phase(), "await_copy", "旧 baseline 回包不能覆盖最新候选");
  await resolveNextSave();
  await waitFor(async () => (await saveCalls()).length === 3, "最新保存进入执行器");
  assert.equal(await phase(), "await_copy", "中间 token 成功也不能推进旧样本");
  const latest = (await saveCalls())[2];
  assert.equal(latest.update.guide.sample_id, 3, "最新真实复制取代旧候选");
  await rejectNextSave("最新保存失败");
  assert.equal(await phase(), "await_copy");
  assert.match(await paperText(), /最新保存失败/);
  assert.match(await paperText(), /重试/);
  await click("retry-persist");
  await resolveNextSave();
  assert.equal(await phase(), "await_show");
  assert.match(await paperText(), /呼出前又复制了这一句/);
});

test("组件：暂停与类型化存储/记录故障不伪造复制，设置入口调用真实 IPC", async () => {
  for (const [extra, expected, page] of [
    ["paused=1", /已暂停/, "privacy"],
    ["storeHealth=failed", /存东西出了问题/, "data"],
    ["captureHealth=failed", /记录出了问题/, "privacy"],
  ]) {
    await openGuide(extra);
    await settle();
    assert.match(await paperText(), expected);
    assert.ok((await invokeCalls("get_guide_health")).length >= 1, "首读必须调用类型化健康 IPC");
    if (extra !== "paused=1") {
      const legacy = await api("history()");
      assert.equal(legacy.store_status, null, "类型化 Failed 不依赖旧文案");
      assert.equal(legacy.recording, true, "类型化 CaptureHealth 不依赖旧 recording 布尔值");
    }
    await click("copy-sample");
    assert.equal((await invokeCalls("guide_copy_sample")).length, 0);
    await click(`settings-${page}`);
    const calls = await invokeCalls("open_settings");
    assert.deepEqual(calls.at(-1).args, { page });
  }
});

test("组件：复用 history:changed 重读类型化健康，无历史增长只更新故障提示", async () => {
  await openReady();
  const beforeReads = (await invokeCalls("get_guide_health")).length;
  const beforeSaves = (await saveCalls()).length;
  await api(`setGuideHealth({store:"failed",capture:"recording"})`);
  await emit("history:changed");
  await waitFor(async () => (await invokeCalls("get_guide_health")).length > beforeReads, "健康变化复用定向 history:changed 触发健康重读");
  assert.match(await paperText(), /存东西出了问题/);
  assert.equal((await api("history()")).store_status, null);
  assert.equal((await saveCalls()).length, beforeSaves, "健康变化没有历史增长，不得推进或另写复制进度");
  await click("copy-sample");
  assert.equal((await invokeCalls("guide_copy_sample")).length, 0);
  assert.equal(await phase(), "await_copy");
});

test("组件：示例被规则跳过或历史没有增长都不进入下一拍", async () => {
  await openReady();
  await click("copy-sample");
  await emit("history:changed");
  const count = (await saveCalls()).length;
  await emit("history:changed");
  assert.equal(await phase(), "await_copy");
  assert.equal((await saveCalls()).length, count, "收藏/检查等无增长通知不能产生复制候选");
  assert.equal(await closeCalls(), 0);
});

test("组件：copied/copied_only 与 native_submission_unknown 文案精确；Visible 重试真实收开并产生新 session", async () => {
  const activeSession = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`);
  for (const [result, expected, forbidden] of [
    ["copied", "这里可能不能输入文字，刚才只复制了内容；请先点一下记事本等能打字的地方再试。", "没能确认"],
    ["copied_only", "这里可能不能输入文字，刚才只复制了内容；请先点一下记事本等能打字的地方再试。", "没能确认"],
    ["native_submission_unknown", "没能确认是否贴出，请换一个能打字的地方再试。", "刚才只复制"],
  ]) {
    await openPaste();
    await paste(2, result);
    const text = await paperText();
    assert.match(text, new RegExp(expected.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    assert.ok(!text.includes(forbidden));
    assert.match(text, /再试一次/);
    assert.match(text, /跳过这一步/);
    assert.equal(await cdp.evaluate(`document.querySelector('[data-action="skip-step"]').disabled`), false);
    assert.equal(await api("panelState().visible"), true, "这些被动失败后普通面板仍可见");

    await click("retry-paste");
    await waitFor(async () => await activeSession() === "12", "专用入口真实收开后自动给出递增 session");
    assert.equal((await invokeCalls("guide_open_panel")).length, 2, "重试必须重新真实呼出");
    assert.deepEqual((await api("panelTransitions()")).slice(-2), [
      { event: "hidden", session: 11 }, { event: "shown", session: 12 },
    ]);
    const beforeLate = (await saveCalls()).length;
    await emit("guide:paste-result", { session: 11, id: 2, result: "sent" });
    assert.equal(await phase(), "await_paste", "新 shown 前的旧结果不能完成");
    assert.equal((await saveCalls()).length, beforeLate);
    assert.equal((await targetedEvents()).length, 0);

    await click("skip-step");
    await rejectNextSave("跳过本步保存失败");
    assert.equal(await phase(), "await_paste");
    assert.equal(await closeCalls(), 0);
    assert.equal((await targetedEvents()).length, 0);
    await click("retry-persist");
    await resolveNextSave();
    assert.equal(await phase(), "completed");
    assert.equal(await closeCalls(), 1);
    assert.equal((await targetedEvents()).length, 1);
  }
});

test("组件：整份跳过与关闭保存失败均不伪完成，成功后 skipped 且零发牌", async () => {
  for (const action of ["skip-guide", "close-guide"]) {
    await openShow();
    await click(action);
    await rejectNextSave(`${action} 保存失败`);
    assert.equal(await phase(), "await_show");
    assert.equal(await closeCalls(), 0);
    assert.equal((await targetedEvents()).length, 0);
    assert.match(await paperText(), new RegExp(`${action} 保存失败`));
    await click("retry-persist");
    await resolveNextSave();
    assert.equal(await phase(), "skipped");
    assert.equal((await api("settings()")).guide.cards_dealt, false);
    assert.equal(await closeCalls(), 1);
    assert.equal((await targetedEvents()).length, 0);
  }
});

test("组件：生产 hidden→paste-result 接收 sent/confirmed 与三类失败", async () => {
  const activeSession = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`);
  for (const result of ["sent", "confirmed"]) {
    await openPaste();
    await paste(2, result, true);
    assert.equal(await activeSession(), "11", "hidden 后、结果前仍保留本次会话");
    assert.equal((await saveCalls()).at(-1).update.guide.phase, "completed", `${result} 必须生成完成候选`);
    assert.equal(await phase(), "await_paste");
    assert.equal((await targetedEvents()).length, 0);
    assert.equal(await closeCalls(), 0);
    await resolveNextSave();
    assert.equal(await phase(), "completed");
    assert.equal(await closeCalls(), 1);
    assert.equal((await targetedEvents()).length, 1);
  }
  for (const [result, expected] of [
    ["copied", /刚才只复制了内容/],
    ["copied_only", /刚才只复制了内容/],
    ["native_submission_unknown", /没能确认是否贴出/],
  ]) {
    await openPaste();
    await paste(2, result, true);
    assert.match(await paperText(), expected);
    assert.equal(await activeSession(), "", "失败结论到达后才清理本次会话");
    assert.match(await paperText(), /再试一次/);
    assert.equal((await targetedEvents()).length, 0);
    assert.equal(await closeCalls(), 0);
  }
});

test("组件：面板开了但没贴就收起保留第三拍，新真实 session 可重试", async () => {
  const activeSession = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`);
  await openPaste();
  for (const session of [10, 12]) {
    await emit("panel:hidden", { session });
    assert.equal(await activeSession(), "11", "旧/错误 session 不能收起当前面板会话");
    assert.ok(!(await paperText()).includes("面板已收起"));
  }
  await hidePanel();
  assert.equal(await phase(), "await_paste");
  assert.equal(await activeSession(), "11", "没有粘贴结论时 hidden 保留会话，直到用户选择重试");
  assert.match(await paperText(), /面板已收起/);
  await click("retry-paste");
  await waitFor(async () => await activeSession() === "12", "Hidden 重试只打开展示并产生新 session");
  await emit("guide:paste-result", { session: 11, id: 2, result: "confirmed" });
  assert.equal(await phase(), "await_paste");
  assert.equal((await targetedEvents()).length, 0, "旧粘贴结果不能完成或发牌");
  await paste(2, "sent", true);
  await resolveNextSave();
  assert.equal(await phase(), "completed");
});

test("组件：hidden → 重新确认后旧 sent/confirmed 均在新 shown 前拒绝", async () => {
  const activeSession = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`);
  const pendingPhase = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.pendingPhase`);
  await openPaste();
  await hidePanel();
  assert.equal(await activeSession(), "11", "hidden 后先保留本次会话等待粘贴结论");

  await api("useManualPanel()");
  await click("retry-paste");
  await waitFor(async () => (await invokeCalls("guide_open_panel")).length === 2, "重新确认已调用真实专用呼出");
  await settle();
  assert.equal(await api("panelState().visible"), false, "deferred 展示前没有伪造新 shown");
  const beforeLate = (await saveCalls()).length;

  const lateResults = [];
  for (const result of ["sent", "confirmed"]) {
    await emit("guide:paste-result", { session: 11, id: 2, result });
    lateResults.push({
      result,
      phase: await phase(),
      session: await activeSession(),
      pendingPhase: await pendingPhase(),
      saves: (await saveCalls()).length - beforeLate,
      deals: (await targetedEvents()).length,
      closes: await closeCalls(),
    });
  }
  assert.deepEqual(lateResults, ["sent", "confirmed"].map((result) => ({
    result, phase: "await_paste", session: "", pendingPhase: "", saves: 0, deals: 0, closes: 0,
  })), "重新确认后，旧 sent/confirmed 都不得在新 shown 前保存、发牌或关闭");
  assert.equal(await activeSession(), "", "重新确认输入位置后，真实新 shown 前旧会话已失效");

  await api("completePanelOpen()");
  await waitFor(async () => await activeSession() === "12", "真实新 shown 才能接管");
  await paste(2, "confirmed", true);
  assert.equal(await pendingPhase(), "completed");
  await resolveNextSave();
  assert.equal(await phase(), "completed");
  assert.equal(await closeCalls(), 1);
  assert.equal((await targetedEvents()).length, 1);
});

test("组件：session 高水位 12 后旧 shown=11/12 无效，真实重开 13 才能完成", async () => {
  const activeSession = () => cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`);
  await openPaste();
  await paste(2, "copied_only");
  await click("retry-paste");
  await waitFor(async () => await activeSession() === "12");
  await paste(2, "copied_only");
  assert.equal(await activeSession(), "");

  await api("useManualPanel()");
  await click("retry-paste");
  await settle();
  assert.equal(await api("panelState().visible"), false, "Visible 重试已先收窗，等待真实再展示");
  for (const session of [11, 12]) {
    await emit("guide:panel-shown", { session });
    assert.equal(await activeSession(), "", `高水位 12 后 shown=${session} 不得重冻结`);
    await emit("guide:paste-result", { session, id: 2, result: "sent" });
    assert.notEqual(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.pendingPhase`), "completed");
    assert.equal((await targetedEvents()).length, 0);
  }
  await api("completePanelOpen()");
  await waitFor(async () => await activeSession() === "13", "只有生产重开后的严格更新 session 能接管");
  await paste(2, "confirmed", true);
  await resolveNextSave();
  assert.equal(await phase(), "completed");
  assert.equal((await targetedEvents()).length, 1);
});

test("组件：重开恢复当前拍，样本删除回第一拍且不能继续呼出", async () => {
  await openGuide("phase=await_paste&sample=2&history=copied&manualSaves=1");
  assert.equal(await phase(), "await_paste");
  assert.equal((await saveCalls()).length, 0, "可信相同基线恢复不重复写");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`), "");
  await click("confirm-target");
  assert.equal((await invokeCalls("guide_open_panel")).length, 1);

  await openGuide("phase=await_paste&sample=99&history=copied&manualSaves=1");
  await resolveNextSave();
  assert.equal(await phase(), "await_copy");
  assert.match(await paperText(), /刚才那条不在了，再复制一句/);
  assert.equal(await cdp.evaluate(`!!document.querySelector('[data-action="confirm-target"]')`), false);
});

test("组件：打开时记录读取失败不伪造基线，重试后从真实快照恢复", async () => {
  await openGuide("manualSaves=1&historyErrors=1");
  assert.match(await paperText(), /还没读到复制记录/);
  assert.equal((await saveCalls()).length, 0);
  await click("retry-load");
  await resolveNextSave();
  assert.equal(await phase(), "await_copy");
  assert.equal((await api("settings()")).guide.baseline_ms, 100);
});

test("组件：首读未返回也能整份跳过，迟到快照不覆盖 skipped", async () => {
  await openGuide("manualHistory=1&manualSaves=1");
  assert.equal((await saveCalls()).length, 0);
  await click("skip-guide");
  await resolveNextSave();
  assert.equal(await phase(), "skipped");
  assert.equal(await closeCalls(), 1);
  await api("resolveHistory(1, window.__guideTest.snapshots.old)");
  await settle();
  assert.equal(await phase(), "skipped");
  assert.equal((await saveCalls()).length, 1, "迟到首读不能另写基线覆盖跳过");
  assert.equal((await targetedEvents()).length, 0);
});

test("组件：StoreHealth::Loading 不拿临时快照当基线，首个可信事件只补基线", async () => {
  await openGuide("manualSaves=1&storeHealth=loading&captureHealth=starting");
  assert.equal((await saveCalls()).length, 0);
  assert.match(await paperText(), /还在准备记录/);
  assert.equal((await api("history()")).store_status, null, "不再用旧“正在打开”文案判断 Loading");
  await api(`setGuideHealth({store:"ready",capture:"recording"})`);
  await cdp.evaluate("window.__guideTest.setHistory(window.__guideTest.snapshots.old)");
  await emit("history:changed");
  await resolveNextSave();
  assert.equal(await phase(), "await_copy");
  assert.equal((await api("settings()")).guide.baseline_ms, 100);
  assert.equal((await api("settings()")).guide.sample_id, null);
});

test("组件：动画只第一次真实记下播放，应用内 motion=none 与跟随系统均直接更新", async () => {
  await openShow();
  await api("copyNewer()");
  await emit("history:changed");
  await resolveNextSave();
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.animationCount`), "1", "第二条新复制不重复播首次动画");

  await openShow("motion=none");
  assert.equal(await cdp.evaluate(`document.documentElement.dataset.motion`), "none");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.animationCount`), "0");
  assert.equal(await cdp.evaluate(`getComputedStyle(document.querySelector("[data-fly-card]")).display`), "none");

  await openShow("followSystem=1", true);
  assert.equal(await cdp.evaluate(`document.documentElement.dataset.reduceMotion`), "true");
  assert.equal(await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.animationCount`), "0");
});

test("组件：监听器完整清理，复用现有 panel:hidden，纸笺外无命中覆盖", async () => {
  await openReady();
  const listeners = await api("activeListeners()");
  const names = listeners.map((entry) => entry.name).sort();
  assert.deepEqual(names, ["capture:changed", "guide:panel-shown", "guide:paste-result", "history:changed", "panel:hidden", "settings:changed"]);
  assert.ok(listeners.every((entry) => entry.count === 1));
  assert.deepEqual(names.filter((name) => name.startsWith("panel:")), ["panel:hidden"], "只复用既有 hidden 广播，不新增普通 panel 事件或订阅");
  const region = await cdp.evaluate(`(() => {
    const paper = document.querySelector("[data-guide-paper]");
    const rect = paper.getBoundingClientRect();
    const hit = window.__guideTest.hitRegionCalls().at(-1);
    return { rect: { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }, hit,
      outside: document.elementFromPoint(1, 1)?.closest("[data-guide-paper]") === null,
      bodyPointer: getComputedStyle(document.body).pointerEvents };
  })()`);
  assert.equal(region.outside, true);
  assert.deepEqual(region.hit.regions, [region.rect]);
  assert.equal(region.hit.armed, true);
  await api("clearInvokeCalls()");
  await api("destroy()");
  await settle();
  assert.deepEqual(await api("activeListeners()"), []);
  const before = (await invokeCalls()).length;
  await emit("history:changed");
  await emit("guide:panel-shown", { session: 99 });
  assert.equal((await invokeCalls()).length, before, "销毁后事件不再引起 IPC");
});

test("组件：按钮可由真实 Tab/Enter 到达，跳过本步与整份跳过始终可聚焦", async () => {
  await openPaste();
  const key = async (init) => {
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", ...init });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...init });
  };
  const tab = { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9, nativeVirtualKeyCode: 9 };
  let reachedSkip = false;
  for (let index = 0; index < 12; index += 1) {
    if (await cdp.evaluate(`document.activeElement?.dataset.action === "skip-step"`)) { reachedSkip = true; break; }
    await key(tab);
  }
  assert.equal(reachedSkip, true);
  await key({ key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13, text: "\r", unmodifiedText: "\r" });
  await waitFor(async () => (await pendingSaves()).length === 1, "键盘触发跳过本步");
  assert.equal(await cdp.evaluate(`document.querySelector('[data-action="skip-guide"]').disabled`), false);
});

test("组件：输出三拍三态、失败与两种跳过的真实组件截图", async () => {
  for (const [label, extra, reduced] of [
    ["light", "", false],
    ["dark", "theme=dark", false],
    ["reduced", "motion=none", false],
  ]) {
    await openReady(extra);
    await captureShot(`guide-${label}-1-copy.png`, { beat: 1, theme: label });
    await api("copyLatest()");
    await emit("history:changed");
    await resolveNextSave();
    await cdp.evaluate("new Promise((resolve) => setTimeout(resolve, 760))");
    await captureShot(`guide-${label}-2-recorded.png`, { beat: 2, theme: label });
    await click("confirm-target");
    await waitFor(async () => (await pendingSaves()).length === 1, "真实展示产生第三拍保存");
    await resolveNextSave();
    await captureShot(`guide-${label}-3-paste.png`, { beat: 3, theme: label });
  }

  await openPaste();
  await paste(2, "copied_only");
  await captureShot("guide-failure-copied-only.png", { state: "不可输入位置，只复制", visibleActions: ["retry-paste", "skip-step"] });
  await click("retry-paste");
  await waitFor(async () => await cdp.evaluate(`document.querySelector("[data-guide-paper]").dataset.session`) === "12");
  await paste(2, "native_submission_unknown");
  await captureShot("guide-failure-native-unknown.png", { state: "无法确认是否贴出", visibleActions: ["retry-paste", "skip-step"] });
  await click("skip-step");
  await captureShot("guide-skip-step-saving.png", { state: "跳过本步，等待保存" });
  await rejectNextSave("跳过本步保存失败");
  await captureShot("guide-skip-step-save-failed.png", { state: "跳过本步保存失败", visibleActions: ["retry-persist", "skip-step"] });

  await openShow();
  await click("skip-guide");
  await rejectNextSave("整份跳过保存失败");
  await captureShot("guide-skip-all-save-failed.png", { state: "整份跳过保存失败", visibleActions: ["retry-persist", "skip-guide"] });
  assert.equal(shotManifest.length, 14);
});
