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
test("Mac 动作键使用主修饰键和删除键符号", () => {
  assert.deepEqual(["Ctrl+D", "Ctrl+Enter", "Ctrl+Delete", "Shift+↵", "Backspace"].map((key) => mac.keyLabel(key)), ["⌘D", "⌘↩", "⌘⌫", "⇧↩", "⌫"]);
});
test("Mac 呼出键保留物理修饰键含义", () => {
  assert.deepEqual(["Ctrl+`", "Ctrl + Alt + Shift + Win + V", "Delete", "control+Backquote", "Super+KeyV"].map((key) => mac.keyLabel(key, true)), ["⌃`", "⌃⌥⇧⌘V", "⌦", "⌃`", "⌘V"]);
});
test("只替换提示中的键名及组合键连接符", () => {
  const cases = [
    { input: "已复制，按 Ctrl+V；Tab / Esc 合上，A+B 保留", physical: false, expected: "已复制，按 ⌘V；⇥ / ⎋ 合上，A+B 保留" },
    ...["unexpected end of input", "cannot return from tab", "Return value missing", "End of stream", "failed to register control handler"]
      .flatMap((input) => [false, true].map((physical) => ({ input, physical, expected: input }))),
  ];
  assert.deepEqual(cases.map(({ input, physical }) => mac.keyLabel(input, physical)), cases.map(({ expected }) => expected));
});
test("主修饰键与另一修饰键按平台换位", () => {
  const command = { metaKey: true, ctrlKey: false };
  assert.deepEqual([mac.primary(command), mac.secondary(command), windows.primary(command), windows.secondary(command)], [true, false, false, true]);
});
