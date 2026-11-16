// README 配图专用的内存 Tauri 边界：只喂演示数据给真实面板组件，不触碰剪贴板、按键或真实窗口。
import { clone, merge } from "../ipcTestUtils.mjs";

const query = new URLSearchParams(location.search);
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const now = Date.now();

let nextId = 1;
const item = (ago, preview, extra = {}) => ({
  id: nextId++, kind: "文本", preview, file_names: [], source: "", copied_ms: now - ago,
  copy_count: 1, partial: false, favorite: false, favorite_position: null, source_name: null, source_icon: null, ...extra,
});
const from = (source, source_name) => ({ source, source_name });

const recent = [
  item(40_000, "周五下午三点，三楼小会议室，记得带投影转接头", from("WeChat.exe", "微信")),
  item(3 * MINUTE, "const items = await history.search(query, { limit: 50 });\nfor (const item of items) {\n  render(item);\n}", from("Code.exe", "Visual Studio Code")),
  item(9 * MINUTE, "https://github.com/eiaao/nianlai/releases", from("msedge.exe", "Microsoft Edge")),
  item(18 * MINUTE, "图片 1920×1080", { kind: "图片", ...from("SnippingTool.exe", "截图工具") }),
  item(26 * MINUTE, "#C8553D", from("Figma.exe", "Figma")),
  item(47 * MINUTE, "季度报告.docx", { kind: "文件", file_names: ["季度报告.docx", "数据附表.xlsx"], ...from("explorer.exe", "文件资源管理器") }),
  item(2 * HOUR, "本周进展：\n1. 搜索支持中文分词，1 万条历史内 15 毫秒返回\n2. 收藏可以加标题和备注\n3. 修复管理员窗口前台时无法打字的问题", from("WINWORD.EXE", "Word")),
  item(3 * HOUR, "hello@example.com", from("OUTLOOK.EXE", "Outlook")),
  item(4 * HOUR, "2026111500342", from("msedge.exe", "Microsoft Edge")),
  item(5 * HOUR, "会议纪要已经发到群里了，大家有空看一下", from("WeChat.exe", "微信")),
  item(DAY + 2 * HOUR, "git switch -c feature/readme-media", from("WindowsTerminal.exe", "终端")),
  item(DAY + 4 * HOUR, "下周一的周会改到线上，会议链接稍后发", from("WeChat.exe", "微信")),
  item(DAY + 6 * HOUR, "https://tauri.app/start/", from("msedge.exe", "Microsoft Edge")),
  item(2 * DAY, "上海市浦东新区世纪大道 100 号 21 层", from("WeChat.exe", "微信")),
  item(2 * DAY + HOUR, "SELECT kind, COUNT(*) FROM items GROUP BY kind;", from("Code.exe", "Visual Studio Code")),
  item(3 * DAY, "#2F6F8F", from("Figma.exe", "Figma")),
  item(3 * DAY + HOUR, "会议室已经订好：周三 10:00–11:00，A305", from("OUTLOOK.EXE", "Outlook")),
  item(4 * DAY, "感谢反馈！这个问题在 0.1.0 里已经修好了，升级后再试试看。", from("msedge.exe", "Microsoft Edge")),
  item(5 * DAY, "cargo test --workspace", from("WindowsTerminal.exe", "终端")),
  item(6 * DAY, "图片 1280×720", { kind: "图片", ...from("SnippingTool.exe", "截图工具") }),
];
const favorites = [
  item(30 * DAY, "上海市浦东新区世纪大道 100 号 21 层 前台转 2103", { favorite: true, favorite_position: 1, title: "公司地址", note: "寄快递用", ...from("WeChat.exe", "微信") }),
  item(40 * DAY, "祝好，\n张明\n产品设计 · 拈来团队", { favorite: true, favorite_position: 2, title: "邮件签名", ...from("OUTLOOK.EXE", "Outlook") }),
  item(50 * DAY, "收到，我看一下，稍后回复你。", { favorite: true, favorite_position: 3, title: "常用回复", ...from("WeChat.exe", "微信") }),
  item(60 * DAY, "9 1310 0000 7178 8888 X", { favorite: true, favorite_position: 4, title: "税号", ...from("msedge.exe", "Microsoft Edge") }),
];
const all = [...recent, ...favorites];

let settings = {
  hotkey: "Ctrl+`", theme: query.get("theme") ?? "light",
  feedback: { motion: query.get("motion") ?? "soft", follow_system: false },
  interaction: { placement: "caret", click: "paste", middle_click: "text" },
  capture: { excluded_apps: [], sensitive_off: [], text: true, rich_text: true, images: true, files: true, max_text_kb: 2048, transient: false, transient_ms: 1000 },
  snapshots: { max_mb: 1024 }, history: { reuse_to_top: true },
  guide: { version: 1, phase: "completed", baseline_ms: null, baseline_total: 0, sample_id: null, cards_dealt: true, dismissed: ["t1", "t2", "t3", "t4"] },
};
const history = () => ({ items: clone(all), total: 2318, recording: true, last_recorded_ms: now - 40_000, store_status: null });
let session = 10;
const listeners = new Map();

function picture(id) {
  // 演示用的“截图”：一张简单的风景插画，避免把任何真实截图放进 README。
  const warm = id % 2 === 0;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360" viewBox="0 0 640 360">
    <defs><linearGradient id="s" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${warm ? "#F6C28B" : "#9CC9E8"}"/><stop offset="1" stop-color="${warm ? "#F3E2C7" : "#E4F1F7"}"/></linearGradient></defs>
    <rect width="640" height="360" fill="url(#s)"/>
    <circle cx="470" cy="110" r="46" fill="${warm ? "#E9734E" : "#FFF6D8"}"/>
    <path d="M0 260 L120 170 L230 245 L340 150 L470 250 L640 180 L640 360 L0 360 Z" fill="${warm ? "#B9714F" : "#5C8DA8"}"/>
    <path d="M0 300 L160 230 L300 290 L450 220 L640 290 L640 360 L0 360 Z" fill="${warm ? "#7C4A36" : "#2F5D75"}"/>
  </svg>`;
  return `data:image/svg+xml;base64,${btoa(svg)}`;
}

function emit(name, payload = {}) {
  for (const handler of [...(listeners.get(name) ?? [])]) handler({ event: name, payload: clone(payload) });
}

export async function invoke(command, args = {}) {
  switch (command) {
    case "get_settings": return clone(settings);
    case "set_settings": settings = merge(settings, clone(args.settings)); emit("settings:changed", clone(settings)); return clone(settings);
    case "get_history": return history();
    case "get_favorites": return clone(favorites);
    case "search_history": {
      const words = String(args.query).trim().toLowerCase().split(/\s+/).filter(Boolean);
      const hits = all.filter((entry) => words.every((word) => `${entry.title ?? ""} ${entry.preview} ${entry.file_names.join(" ")}`.toLowerCase().includes(word)));
      return { request_id: args.requestId, items: clone(hits) };
    }
    case "get_image": {
      const found = all.find((entry) => entry.id === args.id);
      if (!found || found.kind !== "图片") return null;
      const [width, height] = (/(\d+)\s*×\s*(\d+)/.exec(found.preview) ?? [0, 1920, 1080]).slice(1).map(Number);
      return { url: picture(found.id), width, height, bytes: 412_000 };
    }
    case "get_thumbnail": return null;
    case "get_full_text": {
      const found = all.find((entry) => entry.id === args.id);
      return found ? { text: found.preview, total: [...found.preview].length, shown: [...found.preview].length } : null;
    }
    case "get_file_paths": {
      const found = all.find((entry) => entry.id === args.id);
      return found ? found.file_names.map((name) => ({ path: `D:\\工作\\${name}`, state: "present" })) : null;
    }
    case "get_hotkey_status": return { requested: settings.hotkey, active: settings.hotkey, error: null, detail: null, warning: null, overridden: false, disabled: false };
    case "hide_panel": emit("panel:hidden", { session: args.session }); return;
    default: return null;
  }
}
export function listen(name, handler) {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return Promise.resolve(() => { handlers.delete(handler); if (!handlers.size) listeners.delete(name); });
}
export function emitTo(_target, name, payload) { emit(name, payload); return Promise.resolve(); }
export function getVersion() { return Promise.resolve("0.1.0"); }
export function getCurrentWindow() { return { close: () => Promise.resolve() }; }

export const demoApi = {
  emit,
  show: () => {
    const payload = { session: ++session, interaction: "quick", foregroundRevision: 1, hotkeyToShowMs: 1, history: history(), guide: clone(settings.guide) };
    emit("panel:shown", payload);
    return payload.session;
  },
  session: () => session,
  ready: () => listeners.has("panel:shown") && listeners.has("panel:key"),
};
