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
export function reset() { calls.length = 0; rejectNext = false; }
export function rejectNextPaste() { rejectNext = true; }
export async function pasteItem(session, id, mode) {
  calls.push({ command: "pasteItem", session, id, mode });
  if (rejectNext) { rejectNext = false; throw new Error("模拟 IPC 失败"); }
}
export async function hidePanel(session) { calls.push({ command: "hidePanel", session }); }
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
    key: options.key, keyCode: options.key === "Enter" ? 13 : 0,
    ctrlKey: false, altKey: false, metaKey: false, shiftKey: false,
    repeat: false, isComposing: false, target: panel.input, defaultPrevented: false,
    ...options,
    preventDefault() { this.defaultPrevented = true; },
  };
  panel.keydown(event);
  return event;
}

test("搜索提交键的首次按下、自动重复和组合输入保护", async (suite) => {
  const { PanelState, ipc, cleanup } = await compilePanel();
  suite.after(cleanup);
  const createPanel = (context) => {
    let now = 1000;
    context.mock.method(performance, "now", () => now);
    ipc.reset();
    const panel = new PanelState();
    panel.session = 37;
    panel.visible = true;
    panel.mode = "search";
    panel.searchItems = Array.from({ length: 9 }, (_, index) => ({
      id: 101 + index, kind: "文本", preview: "合成条目", file_names: [], source: "合成来源",
      copied_ms: 0, copy_count: 1, partial: false,
    }));
    panel.selected = 1;
    panel.input = {};
    assert.equal(panel.busy, false);
    assert.equal(panel.filtered.length, 9);
    return { panel, advance: (milliseconds) => { now += milliseconds; } };
  };
  const expectedPaste = (action) => ({ command: "pasteItem", session: 37, id: 101 + action.index, mode: action.mode });

  for (const action of submissions) {
    await suite.test(`${action.name} 新按下保留原提交模式`, (context) => {
      const { panel } = createPanel(context);
      const event = keydown(panel, action);
      assert.equal(event.defaultPrevented, true);
      assert.deepEqual(ipc.calls, [expectedPaste(action)]);
    });
    await suite.test(`${action.name} 自动重复不得提交，即使 busy=false`, (context) => {
      const { panel } = createPanel(context);
      const event = keydown(panel, { ...action, repeat: true });
      assert.equal(event.defaultPrevented, true);
      assert.deepEqual(ipc.calls, [], `${action.name} 的重复事件不得发出粘贴或复制 IPC`);
      assert.equal(panel.busy, false);
      assert.equal(panel.selected, 1);
    });
  }

  await suite.test("组合 Enter 上屏超过 50ms 后，重复仍不提交，新的 Enter 正常提交", (context) => {
    const { panel, advance } = createPanel(context);
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
    const { panel } = createPanel(context);
    ipc.rejectNextPaste();
    keydown(panel, { key: "Enter" });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(panel.busy, false);
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])]);
    keydown(panel, { key: "Enter", repeat: true });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0])], "失败不能把长按重复转换成重试授权");
    keydown(panel, { key: "Enter", repeat: false });
    assert.deepEqual(ipc.calls, [expectedPaste(submissions[0]), expectedPaste(submissions[0])]);
  });

  await suite.test("方向键长按继续导航，不提交", (context) => {
    const { panel } = createPanel(context);
    keydown(panel, { key: "ArrowDown" });
    keydown(panel, { key: "ArrowDown", repeat: true });
    keydown(panel, { key: "ArrowDown", repeat: true });
    assert.equal(panel.selected, 4);
    keydown(panel, { key: "ArrowUp", repeat: true });
    assert.equal(panel.selected, 3);
    assert.deepEqual(ipc.calls, []);
  });

  for (const guard of ["composing", "isComposing", "keyCode229"]) {
    await suite.test(`${guard} 仍阻止提交和方向键导航`, (context) => {
      const { panel } = createPanel(context);
      if (guard === "composing") panel.startComposition();
      const options = guard === "isComposing" ? { isComposing: true } : guard === "keyCode229" ? { keyCode: 229 } : {};
      for (const action of [...submissions, { key: "ArrowDown" }, { key: "ArrowUp" }]) {
        keydown(panel, { ...action, ...options });
        assert.deepEqual(ipc.calls, []);
        assert.equal(panel.selected, 1);
      }
    });
  }

  await suite.test("组合结束后的原 50ms 保护及阈值保持不变", (context) => {
    const { panel, advance } = createPanel(context);
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
