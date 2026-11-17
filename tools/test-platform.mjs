import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(readFileSync(new URL("../src/lib/platform.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText;
function labels(userAgent) {
  const context = { navigator: { userAgent }, exports: {} };
  runInNewContext(source, context);
  return context.exports;
}
const windows = labels("Windows NT 10.0");
const mac = labels("Macintosh");

test("Windows 键名保持原样", () => {
  const keys = ["Ctrl+D", "Ctrl + Shift + Enter", "Delete", "Ctrl+`", "A+B"];
  assert.deepEqual(keys.map((key) => windows.keyLabel(key)), keys);
});

test("Mac 动作键与物理呼出键保留各自含义", () => {
  const cases = [
    ["Ctrl+D", false, "⌘D"], ["Ctrl+Enter", false, "⌘↩"], ["Ctrl+Delete", false, "⌘⌦"],
    ["Shift+↵", false, "⇧↩"], ["Backspace", false, "⌫"],
    ["Ctrl+`", true, "⌃`"], ["Ctrl + Alt + Shift + Win + V", true, "⌃⌥⇧⌘V"],
    ["Delete", true, "⌦"], ["control+Backquote", true, "⌃`"], ["Super+KeyV", true, "⌘V"],
  ];
  assert.deepEqual(cases.map(([input, physical]) => mac.keyLabel(input, physical)), cases.map(([, , expected]) => expected));
});

test("完整键名转换后可拼接提示，整句与普通英文保持原样", () => {
  const cases = [
    { input: `已复制，按 ${mac.keyLabel("Ctrl+V")}；${mac.keyLabel("Tab")} / ${mac.keyLabel("Esc")} 合上，A+B 保留`, physical: false, expected: "已复制，按 ⌘V；⇥ / ⎋ 合上，A+B 保留" },
    { input: "已复制，按 Ctrl+V；Tab / Esc 合上，A+B 保留", physical: false, expected: "已复制，按 Ctrl+V；Tab / Esc 合上，A+B 保留" },
    ...["unexpected end of input", "cannot return from tab", "Return value missing", "End of stream", "failed to register control handler"]
      .flatMap((input) => [false, true].map((physical) => ({ input, physical, expected: input }))),
  ];
  assert.deepEqual(cases.map(({ input, physical }) => mac.keyLabel(input, physical)), cases.map(({ expected }) => expected));
});

test("主修饰键与另一修饰键按平台换位", () => {
  const command = { metaKey: true, ctrlKey: false };
  assert.deepEqual([mac.primary(command), mac.secondary(command), windows.primary(command), windows.secondary(command)], [true, false, false, true]);
});

test("删除键纯映射只在 Mac 快速模式无已选且非输入法组合时生效", () => {
  const cases = [
    [true, "quick", false, {}, "Delete"], [true, "search", false, {}, "Backspace"],
    [true, "search", false, { metaKey: true }, "Backspace"],
    [true, "quick", true, {}, "Backspace"], [false, "quick", false, {}, "Backspace"],
    [true, "quick", false, { isComposing: true }, "Backspace"],
    [true, "quick", false, { keyCode: 229 }, "Backspace"],
    [true, "search", false, { key: "Delete", metaKey: true }, "Delete"],
  ];
  const run = (platform) => cases.map(([mac, mode, selected, event]) => platform.normalizePanelKey(mac, mode, selected, { key: "Backspace", isComposing: false, keyCode: 8, ...event }));
  const expected = cases.map(([, , , , key]) => key);
  assert.deepEqual([run(mac), run(windows)], [expected, expected]);
});
