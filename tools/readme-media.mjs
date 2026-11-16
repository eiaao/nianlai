// 生成 README 的截图和演示 GIF：真实生产组件 + 内存 IPC 替身 + 无头 Edge，输出到 docs/images/。
// 用法（Windows）：node tools\readme-media.mjs [场景名…]；GIF 需要 PATH 里有 ffmpeg。
// 不触碰系统剪贴板、按键、注册表或真实窗口；演示数据全部虚构。
import { existsSync, mkdirSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer as createNetServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { cleanupHeadlessEdge } from "./headlessEdgeCleanup.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const outDir = resolve(root, "docs/images");
const temporaryBase = resolve(tmpdir());
const temporary = mkdtempSync(join(temporaryBase, "nianlai-readme-"));
const only = new Set(process.argv.slice(2));
mkdirSync(outDir, { recursive: true });

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
async function freePort() {
  const server = createNetServer();
  await new Promise((done, fail) => server.once("error", fail).listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}
async function waitFor(predicate, description, timeout = 20000) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < timeout) {
    try { const value = await predicate(); if (value) return value; } catch (error) { lastError = error; }
    await sleep(30);
  }
  throw new Error(`等待超时：${description}${lastError ? `；${lastError.message}` : ""}`);
}
function edgePath() {
  const found = [process.env.EDGE_PATH, "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"].filter(Boolean).find((path) => existsSync(path));
  if (!found) throw new Error("找不到 Edge");
  return found;
}

class Cdp {
  constructor(url) { this.ws = new WebSocket(url); this.nextId = 0; this.pending = new Map(); this.errors = []; }
  async open() {
    await new Promise((done, fail) => { this.ws.addEventListener("open", done, { once: true }); this.ws.addEventListener("error", fail, { once: true }); });
    this.ws.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      if (message.id) {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id);
        if (message.error) pending.reject(new Error(JSON.stringify(message.error))); else pending.resolve(message.result);
      } else if (message.method === "Runtime.exceptionThrown") {
        this.errors.push(message.params.exceptionDetails?.exception?.description ?? message.params.exceptionDetails?.text);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.nextId;
    return new Promise((done, fail) => { this.pending.set(id, { resolve: done, reject: fail }); this.ws.send(JSON.stringify({ id, method, params })); });
  }
  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result?.value;
  }
}

async function viteFor(ipcFile) {
  const port = await freePort();
  const ipcPath = resolve(root, ipcFile).replace(/\\/g, "/");
  const server = await createServer({
    configFile: false, root, plugins: [svelte()], clearScreen: false, logLevel: "error",
    resolve: { alias: ["core", "event", "app", "window"].map((name) => ({ find: `@tauri-apps/api/${name}`, replacement: ipcPath })) },
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { host: "127.0.0.1", port, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/internal/**", "**/target/**", "**/docs/**"] } },
  });
  await server.listen();
  return { server, base: `http://127.0.0.1:${port}` };
}

const debugPort = await freePort();
const edge = spawn(edgePath(), ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--disable-component-update", "--disable-sync", "--hide-scrollbars", `--remote-debugging-port=${debugPort}`, `--user-data-dir=${join(temporary, "edge-profile")}`, "about:blank"], { stdio: "ignore", windowsHide: true });
const target = await waitFor(async () => (await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json()).find((page) => page.type === "page"), "Edge 调试目标");
const cdp = new Cdp(target.webSocketDebuggerUrl);
await cdp.open();
await cdp.send("Runtime.enable");
await cdp.send("Page.enable");

const frame = () => cdp.evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
async function settle(timeout = 4000) {
  await cdp.evaluate("document.fonts.ready.then(() => true)");
  await frame();
  await waitFor(() => cdp.evaluate(`document.getAnimations().every((animation) => animation.playState !== "running" || animation.effect?.getTiming().iterations === Infinity)`), "动画落位", timeout).catch(() => {});
  await frame();
}
async function navigate(url, width, height, { dark = false } = {}) {
  await cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: dark ? "dark" : "light" }, { name: "prefers-reduced-motion", value: "no-preference" }] });
  const [path, hash = ""] = url.split("#");
  await cdp.send("Page.navigate", { url: `${path}${path.includes("?") ? "&" : "?"}nav=${Date.now()}${hash ? `#${hash}` : ""}` });
  await sleep(200);
  await waitFor(() => cdp.evaluate("document.readyState === 'complete'"), "页面加载");
}
async function shot(name, selector = null) {
  const clip = selector ? await cdp.evaluate(`(() => { const box = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); const pad = 12;
    return { x: Math.max(0, box.x - pad), y: Math.max(0, box.y - pad), width: box.width + pad * 2, height: box.height + pad * 2, scale: 1 }; })()`) : undefined;
  const { data } = await cdp.send("Page.captureScreenshot", { format: "png", ...(clip ? { clip } : {}) });
  const path = join(outDir, `${name}.png`);
  writeFileSync(path, Buffer.from(data, "base64"));
  console.log(`  ${name}.png  ${(statSync(path).size / 1024).toFixed(0)} KB`);
}

/** 边执行动作边连续截帧，再交给 ffmpeg 按真实时间间隔合成 GIF。 */
async function record(name, actions, { width = 800, fps = 20, hold = 1200 } = {}) {
  const frames = join(temporary, `frames-${name}`);
  mkdirSync(frames, { recursive: true });
  const stamps = [];
  let running = true;
  const loop = (async () => {
    while (running) {
      const at = Date.now();
      const { data } = await cdp.send("Page.captureScreenshot", { format: "png", optimizeForSpeed: true });
      const file = `f${String(stamps.length).padStart(4, "0")}.png`;
      writeFileSync(join(frames, file), Buffer.from(data, "base64"));
      stamps.push({ file, at });
      const spent = Date.now() - at;
      if (spent < 1000 / fps) await sleep(1000 / fps - spent);
    }
  })();
  await sleep(300);
  await actions();
  await sleep(hold);
  running = false;
  await loop;
  const lines = [];
  stamps.forEach((stamp, index) => {
    const next = stamps[index + 1]?.at ?? stamp.at + hold;
    lines.push(`file '${stamp.file}'`, `duration ${((next - stamp.at) / 1000).toFixed(3)}`);
  });
  lines.push(`file '${stamps.at(-1).file}'`);
  writeFileSync(join(frames, "list.txt"), lines.join("\n"));
  const path = join(outDir, `${name}.gif`);
  const filter = `fps=${fps},scale=${width}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle`;
  const result = spawnSync("ffmpeg", ["-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", "list.txt", "-vf", filter, "-loop", "0", path], { cwd: frames, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`ffmpeg 失败：${result.stderr || result.error}`);
  console.log(`  ${name}.gif  ${stamps.length} 帧  ${(statSync(path).size / 1024).toFixed(0)} KB`);
}

// ---------- 面板 ----------
const PANEL = { width: 960, height: 560 };
let panelBase;
const demo = (expression) => cdp.evaluate(`window.__demo.${expression}`);
async function openPanel(theme = "light") {
  await navigate(`${panelBase}/tools/readme-media/demo.html?theme=${theme}`, PANEL.width, PANEL.height, { dark: theme === "dark" });
  await waitFor(() => cdp.evaluate("!!window.__demo && window.__demo.ready()"), "面板订阅");
  await sleep(300);
}
async function show() { await demo("show()"); await settle(); }
async function key(name, pause = 420) {
  await cdp.evaluate(`window.__demo.emit("panel:key", { session: window.__demo.session(), key: ${JSON.stringify(name)} })`);
  await sleep(pause);
}
async function typeText(text, gap = 160) {
  await cdp.evaluate(`window.__demo.emit("panel:mode", { session: window.__demo.session(), mode: "search" })`);
  await waitFor(() => cdp.evaluate("document.activeElement?.tagName === 'INPUT'"), "搜索框获得焦点", 4000);
  for (const char of text) { await cdp.send("Input.insertText", { text: char }); await sleep(gap); }
}

const scenes = {
  async hero() {
    for (const theme of ["light", "dark"]) {
      await openPanel(theme);
      await show();
      await shot(theme === "light" ? "panel-fan" : "panel-fan-dark");
    }
  },
  async spread() {
    await openPanel();
    await show();
    await key("ArrowDown", 600);
    await settle();
    await shot("panel-spread");
  },
  async favorites() {
    await openPanel();
    await show();
    await key("ArrowUp", 600);
    await settle();
    await shot("panel-favorites");
  },
  async preview() {
    await openPanel();
    await show();
    for (let step = 0; step < 3; step += 1) await key("ArrowRight", 250);
    await key("Tab", 900);
    await settle();
    await shot("panel-preview");
  },
  async demoFan() {
    await openPanel();
    await record("demo-hand", async () => {
      await show(); await sleep(700);
      for (const step of ["ArrowRight", "ArrowRight", "ArrowRight", "ArrowLeft"]) await key(step, 550);
      await key("ArrowUp", 1100);
      await key("ArrowUp", 900);
      await key("ArrowDown", 1100);
      await key("ArrowDown", 450); await key("ArrowDown", 450); await key("ArrowDown", 700);
    });
  },
  async demoSearch() {
    await openPanel();
    await record("demo-search", async () => {
      await show(); await sleep(700);
      await typeText("会议", 380);
      await sleep(1300);
      await cdp.evaluate(`(() => { const input = document.activeElement; input.value = ""; input.dispatchEvent(new Event("input", { bubbles: true })); })()`);
      await sleep(500);
      await typeText("设置", 380);
      await sleep(1200);
    });
  },
};

// ---------- 设置与引导 ----------
async function settingsScenes() {
  const { server, base } = await viteFor("tools/settingsPagesIpc.mjs");
  try {
    // 设置页沿用 settingsPagesIpc 的测试数据；收藏、数据页的数据太“测试”，README 只放这两页。
    for (const page of ["general", "privacy"]) {
      await navigate(`${base}/tools/settingsPagesTest.html?test=readme#${page}`, 1120, 680);
      await waitFor(() => cdp.evaluate(`document.querySelector("[data-settings-loading='false']") !== null`), "设置加载");
      await cdp.evaluate(`window.__settingsPagesTest.save("general.appearance", { theme: "light", feedback: { motion: "spring", follow_system: false } }, "外观已保存并应用")`);
      await sleep(400);
      await settle();
      await shot(`settings-${page}`);
    }
  } finally { await server.close(); }
}
async function guideScenes() {
  const { server, base } = await viteFor("tools/guideTestIpc.mjs");
  try {
    await navigate(`${base}/tools/guideTest.html?phase=await_copy&theme=light`, 360, 360);
    await sleep(600);
    await settle();
    await shot("guide", ".guide-paper");
  } finally { await server.close(); }
}

try {
  const wanted = (name) => only.size === 0 || only.has(name);
  const panelNames = Object.keys(scenes).filter(wanted);
  if (panelNames.length) {
    const { server, base } = await viteFor("tools/readme-media/demoIpc.mjs");
    panelBase = base;
    try {
      for (const name of panelNames) { console.log(`面板：${name}`); await scenes[name](); }
    } finally { await server.close(); }
  }
  if (wanted("settings")) { console.log("设置"); await settingsScenes(); }
  if (wanted("guide")) { console.log("引导"); await guideScenes(); }
  if (cdp.errors.length) console.warn(`浏览器异常：\n${cdp.errors.join("\n")}`);
} finally {
  await Promise.race([cdp.send("Browser.close").catch(() => {}), sleep(3000)]);
  cdp.ws.close();
  await cleanupHeadlessEdge({ edge, temporary, temporaryBase, prefix: "nianlai-readme-", label: "README" });
  rmSync(temporary, { recursive: true, force: true });
}
