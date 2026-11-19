// 仓库根目录运行：node tools/test-release-notes.mjs
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  DOWNLOAD_PREFIX, buildFeed, buildNotes, checkVersion, compareVersions, itemsFor, noteVersions, parseNotes,
} from "./release-notes.mjs";

function notesDir(files) {
  const dir = mkdtempSync(join(tmpdir(), "nianlai-notes-"));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, `${name}.md`), text);
  return dir;
}
const note = (common, windows = [], macos = []) =>
  [["通用", common], ["Windows", windows], ["macOS", macos]]
    .filter(([, items]) => items.length)
    .map(([title, items]) => `## ${title}\n${items.map((item) => `- ${item}`).join("\n")}\n`).join("\n");

test("三节结构按平台合并", () => {
  const sections = parseNotes(note(["通用一"], ["只在 Windows"], ["只在 Mac"]));
  assert.deepEqual(itemsFor(sections, "windows"), ["通用一", "只在 Windows"]);
  assert.deepEqual(itemsFor(sections, "macos"), ["通用一", "只在 Mac"]);
});

test("小节可省略，CRLF 和 BOM 可接受", () => {
  const sections = parseNotes("\uFEFF## macOS\r\n- 只有 Mac\r\n");
  assert.deepEqual(sections, { common: [], windows: [], macos: ["只有 Mac"] });
});

test("结构错误要报错", () => {
  assert.throws(() => parseNotes("## 其他\n- x"), /未知的小节/);
  assert.throws(() => parseNotes("- 没有小节"), /小节下面/);
  assert.throws(() => parseNotes("## 通用\n随便一段话"), /只能写/);
});

test("版本比较遵循语义化版本", () => {
  assert.ok(compareVersions("0.10.0", "0.9.9") > 0);
  assert.ok(compareVersions("1.0.0-rc.1", "1.0.0") < 0);
  assert.ok(compareVersions("1.0.0-rc.2", "1.0.0-rc.10") < 0);
  assert.equal(compareVersions("0.1.2", "0.1.2"), 0);
  assert.throws(() => compareVersions("abc", "0.1.0"));
});

test("缺少当前版本的说明文件就失败", () => {
  const dir = notesDir({ "0.1.1": note(["旧"]) });
  assert.throws(() => checkVersion(dir, "0.1.2"), /缺少更新说明/);
  assert.throws(() => checkVersion(dir, "bad"), /无效的版本号/);
  rmSync(dir, { recursive: true });
});

test("空说明文件失败", () => {
  const dir = notesDir({ "0.1.2": "## 通用\n" });
  assert.throws(() => checkVersion(dir, "0.1.2"), /没有任何更新条目/);
  rmSync(dir, { recursive: true });
});

test("只取最近 5 个版本（含本版），更新的版本不进入", () => {
  const files = {};
  for (let n = 0; n <= 8; n++) files[`0.1.${n}`] = note([`说明 ${n}`]);
  files["0.2.0"] = note(["未来"]);
  const dir = notesDir(files);
  const notes = buildNotes(dir, "0.1.7", "windows");
  assert.deepEqual(notes.map((entry) => entry.version), ["0.1.7", "0.1.6", "0.1.5", "0.1.4", "0.1.3"]);
  rmSync(dir, { recursive: true });
});

test("没有该平台条目的旧版本被跳过", () => {
  const dir = notesDir({ "0.1.1": note([], [], ["只有 Mac"]), "0.1.2": note(["通用"]) });
  assert.deepEqual(buildNotes(dir, "0.1.2", "windows").map((entry) => entry.version), ["0.1.2"]);
  assert.deepEqual(buildNotes(dir, "0.1.2", "macos").map((entry) => entry.version), ["0.1.2", "0.1.1"]);
  rmSync(dir, { recursive: true });
});

test("版本文件字段符合客户端约定", () => {
  const dir = notesDir({ "0.1.2": note(["新"]) });
  const packageFile = join(dir, "nianlai-0.1.2-windows-x64-portable.zip");
  writeFileSync(packageFile, "abc");
  const feed = buildFeed({ dir, platform: "windows", version: "0.1.2", packageFile, date: "2026-11-25" });
  assert.deepEqual(Object.keys(feed), ["version", "date", "url", "sha256", "notes"]);
  assert.equal(feed.url, `${DOWNLOAD_PREFIX}v0.1.2/nianlai-0.1.2-windows-x64-portable.zip`);
  assert.equal(feed.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.deepEqual(feed.notes, [{ version: "0.1.2", items: ["新"] }]);
  assert.throws(() => buildFeed({ dir, platform: "linux", version: "0.1.2", packageFile, date: "x" }), /平台/);
  rmSync(dir, { recursive: true });
});

test("仓库里的 release-notes 全部有效", () => {
  const dir = new URL("../release-notes", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
  const versions = noteVersions(dir);
  assert.ok(versions.length >= 3);
  assert.equal(readdirSync(dir).filter((name) => name.endsWith(".md")).length, versions.length, "文件名必须是版本号");
  for (const version of versions) {
    checkVersion(dir, version);
    assert.ok(readFileSync(join(dir, `${version}.md`), "utf8").trim());
  }
});
