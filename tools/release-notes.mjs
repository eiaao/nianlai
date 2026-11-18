// 更新说明与版本文件生成（Node 运行，无第三方依赖）。
//
//   node tools/release-notes.mjs check --version 0.1.2
//       核对 release-notes/0.1.2.md 存在且有内容；发布工作流在“核对标签”阶段调用。
//   node tools/release-notes.mjs feed --platform windows|macos --version 0.1.2 --file <安装包> --out <latest-xxx.json> [--date YYYY-MM-DD]
//       生成客户端读取的版本文件（latest-windows.json / latest-macos.json）。
//   node tools/release-notes.mjs --self-test
//       自检（同 tools/test-release-notes.mjs 里的主要用例）。
//
// release-notes/<版本>.md 的结构：`## 通用`、`## Windows`、`## macOS` 三节，每节是 `- ` 列表，任何一节都可以省略。
// 某平台的更新说明 = 通用 + 该平台那一节。

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// 客户端只认这个前缀下的安装包地址（见 src-tauri/src/update/feed.rs）。
export const DOWNLOAD_PREFIX = "https://github.com/eiaao/nianlai/releases/download/";
export const MAX_VERSIONS = 5;
const SECTIONS = new Map([["通用", "common"], ["Windows", "windows"], ["macOS", "macos"]]);
const PLATFORMS = ["windows", "macos"];
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

export function parseVersion(text) {
  const match = SEMVER.exec(text);
  if (!match) return null;
  return { major: +match[1], minor: +match[2], patch: +match[3], pre: match[4] ? match[4].split(".") : [] };
}

/** 语义化版本比较：a<b 为负。预发布低于正式版。 */
export function compareVersions(a, b) {
  const x = parseVersion(a), y = parseVersion(b);
  if (!x || !y) throw new Error(`无效的版本号：${!x ? a : b}`);
  for (const key of ["major", "minor", "patch"]) if (x[key] !== y[key]) return x[key] - y[key];
  if (!x.pre.length || !y.pre.length) return y.pre.length - x.pre.length;
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i], q = y.pre[i];
    if (p === undefined) return -1;
    if (q === undefined) return 1;
    const pn = /^\d+$/.test(p), qn = /^\d+$/.test(q);
    if (pn && qn) { if (+p !== +q) return +p - +q; }
    else if (pn !== qn) return pn ? -1 : 1;
    else if (p !== q) return p < q ? -1 : 1;
  }
  return 0;
}

/** 解析一个 release-notes 文件；结构不对就抛错（发布前必须修）。 */
export function parseNotes(markdown, label = "更新说明") {
  const sections = { common: [], windows: [], macos: [] };
  let current = null;
  for (const [index, raw] of markdown.replace(/^\uFEFF/, "").split(/\r?\n/).entries()) {
    const line = raw.trim();
    if (!line) continue;
    const heading = /^##\s+(.+)$/.exec(line);
    if (heading) {
      const key = SECTIONS.get(heading[1].trim());
      if (!key) throw new Error(`${label} 第 ${index + 1} 行：未知的小节“${heading[1]}”，只能是 通用 / Windows / macOS`);
      current = key;
      continue;
    }
    const item = /^[-*]\s+(.+)$/.exec(line);
    if (item) {
      if (!current) throw new Error(`${label} 第 ${index + 1} 行：列表项要放在 ## 小节下面`);
      sections[current].push(item[1].trim());
      continue;
    }
    if (/^#\s/.test(line)) continue; // 允许一个一级标题
    throw new Error(`${label} 第 ${index + 1} 行：只能写 ## 小节标题和 “- ” 列表项`);
  }
  return sections;
}

/** 某平台的条目：通用在前，平台专属在后。 */
export function itemsFor(sections, platform) {
  return [...sections.common, ...sections[platform]];
}

export function noteVersions(dir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith(".md"))
    .map((name) => name.slice(0, -3))
    .filter((version) => parseVersion(version));
}

export function checkVersion(dir, version) {
  if (!parseVersion(version)) throw new Error(`无效的版本号：${version}`);
  const file = join(dir, `${version}.md`);
  if (!existsSync(file)) throw new Error(`缺少更新说明：release-notes/${version}.md（发布前必须补上）`);
  const sections = parseNotes(readFileSync(file, "utf8"), `release-notes/${version}.md`);
  if (!sections.common.length && !sections.windows.length && !sections.macos.length) {
    throw new Error(`release-notes/${version}.md 没有任何更新条目`);
  }
  return sections;
}

export function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** 最近 MAX_VERSIONS 个版本（含本版、不含比本版更新的），按新到旧。没有该平台条目的版本跳过。 */
export function buildNotes(dir, version, platform) {
  checkVersion(dir, version);
  return noteVersions(dir)
    .filter((candidate) => compareVersions(candidate, version) <= 0)
    .sort((a, b) => compareVersions(b, a))
    .slice(0, MAX_VERSIONS)
    .map((candidate) => ({
      version: candidate,
      items: itemsFor(parseNotes(readFileSync(join(dir, `${candidate}.md`), "utf8"), `release-notes/${candidate}.md`), platform),
    }))
    .filter((note) => note.items.length);
}

export function buildFeed({ dir, platform, version, packageFile, date, sha256 }) {
  if (!PLATFORMS.includes(platform)) throw new Error(`平台必须是 ${PLATFORMS.join(" 或 ")}`);
  const name = basename(packageFile);
  return {
    version,
    date,
    url: `${DOWNLOAD_PREFIX}v${version}/${name}`,
    sha256: sha256 ?? sha256File(packageFile),
    notes: buildNotes(dir, version, platform),
  };
}

function selfTest() {
  const assert = (cond, message) => { if (!cond) throw new Error(`自检失败：${message}`); };
  const sections = parseNotes("## 通用\n- a\n- b\n## Windows\n- w\n");
  assert(itemsFor(sections, "windows").join() === "a,b,w", "windows 合并通用与专属");
  assert(itemsFor(sections, "macos").join() === "a,b", "macos 只有通用");
  assert(compareVersions("0.10.0", "0.9.0") > 0 && compareVersions("1.0.0-rc.1", "1.0.0") < 0, "版本比较");
  let threw = false;
  try { parseNotes("## 其他\n- x"); } catch { threw = true; }
  assert(threw, "未知小节要报错");
  console.log("release-notes 自检通过");
}

function arg(args, name) {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : undefined;
}

function main(args) {
  if (args.includes("--self-test")) return selfTest();
  const [command] = args;
  const dir = resolve(arg(args, "notes-dir") ?? join(root, "release-notes"));
  const version = arg(args, "version");
  if (command === "check") {
    if (!version) throw new Error("缺少 --version");
    checkVersion(dir, version);
    console.log(`release-notes/${version}.md 核对通过`);
  } else if (command === "feed") {
    const platform = arg(args, "platform"), file = arg(args, "file"), out = arg(args, "out");
    if (!version || !platform || !file || !out) throw new Error("需要 --platform --version --file --out");
    const date = arg(args, "date") ?? new Date().toISOString().slice(0, 10);
    const feed = buildFeed({ dir, platform, version, packageFile: file, date });
    writeFileSync(out, `${JSON.stringify(feed, null, 2)}\n`, "utf8");
    console.log(`已生成 ${out}（${platform} ${version}，${feed.notes.length} 个版本的说明）`);
  } else {
    throw new Error("用法：check --version X | feed --platform P --version X --file F --out O | --self-test");
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(`::error::${error.message}`); process.exit(1); }
}
