// 牌面与摆放的纯函数：类型判定、内容拆分、时间分组、扇面 / 摊开几何。
// 不依赖 DOM 和 Svelte，state.svelte.ts 与组件共用，tools/test-panel-key-repeat.mjs 直接测试。
import type { HistoryItem } from "./ipc";

/** 九种牌面。判定只用后端已有字段（kind、preview、file_names），文本类按内容推断。 */
export type CardKind = "short" | "long" | "link" | "code" | "image" | "color" | "file" | "mail" | "number";

export const KIND_NAME: Record<CardKind, string> = {
  short: "文字", long: "长文", link: "网址", code: "代码", image: "图片", color: "颜色", file: "文件", mail: "邮箱", number: "号码",
};

const SHORT_LIMIT = 24;

export type LinkParts = { href: string; host: string; prefix: string; tail: string };

/** 网址拆三段：域名 / 前面的路径（淡色）/ 最后两段（墨色）。相似网址的差别几乎都在最后两段。 */
export function linkParts(text: string): LinkParts | null {
  const value = text.trim();
  if (!value || /\s/.test(value)) return null;
  const match = /^(?:https?:\/\/)(?:www\.)?([^/?#]+)([^]*)$/i.exec(value) ?? /^www\.([^/?#]+\.[a-z]{2,})([^]*)$/i.exec(value);
  if (!match || !/\./.test(match[1]) && !/^localhost(?::\d+)?$/i.test(match[1])) return null;
  const host = match[1];
  const path = match[2] ?? "";
  if (!path || path === "/") return { href: value, host, prefix: "", tail: "" };
  if (!path.startsWith("/")) return { href: value, host, prefix: "", tail: path };
  const segments = path.split("/");
  const cut = Math.max(1, segments.length - 2);
  return { href: value, host, prefix: `${segments.slice(0, cut).join("/")}/`, tail: segments.slice(cut).join("/") };
}

export type ColorValue = { value: string; hex: string; rgb: [number, number, number]; hsl: [number, number, number]; format: "HEX" | "RGB" | "HSL" };

const toHex = (rgb: readonly number[]) => `#${rgb.map((channel) => channel.toString(16).padStart(2, "0")).join("").toUpperCase()}`;

function rgbToHsl([r, g, b]: [number, number, number]): [number, number, number] {
  const [red, green, blue] = [r / 255, g / 255, b / 255];
  const max = Math.max(red, green, blue), min = Math.min(red, green, blue), light = (max + min) / 2;
  let hue = 0, saturation = 0;
  if (max !== min) {
    const delta = max - min;
    saturation = light > 0.5 ? delta / (2 - max - min) : delta / (max + min);
    hue = max === red ? (green - blue) / delta + (green < blue ? 6 : 0) : max === green ? (blue - red) / delta + 2 : (red - green) / delta + 4;
    hue *= 60;
  }
  return [Math.round(hue), Math.round(saturation * 100), Math.round(light * 100)];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const sat = s / 100, light = l / 100;
  const k = (n: number) => (n + h / 30) % 12;
  const a = sat * Math.min(light, 1 - light);
  const f = (n: number) => light - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [f(0), f(8), f(4)].map((value) => Math.round(value * 255)) as [number, number, number];
}

/** `#RGB`、`#RRGGBB`（含透明度位）、`rgb()`/`rgba()`、`hsl()`。其他都不算颜色。 */
export function colorValue(text: string): ColorValue | null {
  const value = text.trim();
  const hex = /^#([\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.exec(value);
  if (hex) {
    const raw = hex[1];
    const full = raw.length <= 4 ? [...raw].map((part) => part + part).join("") : raw;
    const rgb = [0, 2, 4].map((offset) => Number.parseInt(full.slice(offset, offset + 2), 16)) as [number, number, number];
    return { value, hex: toHex(rgb), rgb, hsl: rgbToHsl(rgb), format: "HEX" };
  }
  const rgbMatch = /^rgba?\(\s*([\d.]+%?)\s*[, ]\s*([\d.]+%?)\s*[, ]\s*([\d.]+%?)(?:\s*[,/]\s*[\d.]+%?)?\s*\)$/i.exec(value);
  if (rgbMatch) {
    const rgb = rgbMatch.slice(1, 4).map((channel) => channel.endsWith("%")
      ? Math.round(Math.min(100, Number.parseFloat(channel)) * 2.55)
      : Math.max(0, Math.min(255, Math.round(Number.parseFloat(channel))))) as [number, number, number];
    return { value, hex: toHex(rgb), rgb, hsl: rgbToHsl(rgb), format: "RGB" };
  }
  const hslMatch = /^hsla?\(\s*([\d.]+)(?:deg)?\s*[, ]\s*([\d.]+)%\s*[, ]\s*([\d.]+)%(?:\s*[,/]\s*[\d.]+%?)?\s*\)$/i.exec(value);
  if (hslMatch) {
    const [h, s, l] = hslMatch.slice(1, 4).map(Number.parseFloat);
    const rgb = hslToRgb(h % 360, Math.min(100, s), Math.min(100, l));
    return { value, hex: toHex(rgb), rgb, hsl: [Math.round(h % 360), Math.round(s), Math.round(l)], format: "HSL" };
  }
  return null;
}

export function mailParts(text: string): { user: string; domain: string } | null {
  const match = /^([^\s@]+)@([^\s@]+\.[^\s@]+)$/.exec(text.trim());
  return match ? { user: match[1], domain: match[2] } : null;
}

/** 电话、银行卡、税号、单号：单行，只有数字、大写字母、空格、连字符；至少 6 位数字且数字占多数。 */
export function isNumber(text: string) {
  const value = text.trim();
  if (value.length < 6 || value.length > 40 || !/^\+?[\dA-Z][\dA-Z \-]*[\dA-Z]$/.test(value)) return false;
  const digits = value.replace(/\D/g, "").length;
  const letters = value.replace(/[^A-Z]/g, "").length;
  return digits >= 6 && digits >= letters * 3;
}

export function looksLikeCode(value: string) {
  const first = value.trimStart().split("\n", 1)[0];
  if (/^(?:const|let|var|function|class|import|export|return|async|await|interface|type|enum|fn|pub\s|use\s|#include|package\s|def\s|SELECT\s|INSERT\s|UPDATE\s|DELETE\s|git\s|pnpm\s|npm\s|cargo\s|winget\s)/i.test(first)) return true;
  const lines = value.split("\n");
  return lines.length > 1 && /[;{}[\]]$/.test(value.trim()) && /(?:=>|\bfunction\b|\bconst\b|\blet\b|[{}];)/.test(value);
}

const textOf = (item: Pick<HistoryItem, "preview">) => item.preview ?? "";

/** 九种牌面的判定（纯函数）。顺序：文件 → 图片 → 颜色 → 网址 → 邮箱 → 号码 → 代码 → 短 / 长文字。 */
export function cardKind(item: Pick<HistoryItem, "kind" | "preview" | "file_names">): CardKind {
  if (item.kind === "文件") return "file";
  if (item.kind === "图片") return "image";
  const text = textOf(item);
  const value = text.trim();
  if (!value) return "short";
  if (colorValue(value)) return "color";
  if (linkParts(value)) return "link";
  if (mailParts(value)) return "mail";
  if (isNumber(value)) return "number";
  if (looksLikeCode(text)) return "code";
  return [...value].length <= SHORT_LIMIT && !value.includes("\n") ? "short" : "long";
}

export const fileExt = (name: string) => (/\.([a-z0-9]{1,5})$/i.exec(name)?.[1] ?? "文件").toUpperCase();
export const firstLine = (text: string) => text.split("\n").find((line) => line.trim()) ?? "";
export const sourceName = (item: HistoryItem) => item.source_name?.trim() || item.source || "来源未知";
/** 图片条目的“1280 × 720”；后端的预览文字形如“图片 1280×720”。 */
export const imageSize = (item: HistoryItem) => (/(\d+)\s*[×x]\s*(\d+)/.exec(item.preview ?? "") ?? []).slice(1, 3).join(" × ");
export const cardLabel = (item: HistoryItem) => item.title || firstLine(item.preview ?? "") || item.file_names[0] || "图片";

const pad = (value: number) => String(value).padStart(2, "0");
const clock = (date: Date) => `${date.getHours()}:${pad(date.getMinutes())}`;
const dayStart = (millis: number) => { const date = new Date(millis); return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime(); };

/** 牌脚 / 行尾的相对时间：刚刚、3 分钟前、2 小时前、昨天 14:51、4 天前、11月2日。 */
export function relativeTime(millis: number, now = Date.now()) {
  const elapsed = Math.max(0, now - millis);
  if (elapsed < 60_000) return "刚刚";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} 分钟前`;
  const today = dayStart(now);
  if (millis >= today) return `${Math.floor(elapsed / 3_600_000)} 小时前`;
  if (millis >= today - 86_400_000) return `昨天 ${clock(new Date(millis))}`;
  const days = Math.round((today - dayStart(millis)) / 86_400_000);
  if (days < 7) return `${days} 天前`;
  const date = new Date(millis);
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

export type TimeGroup = "常用" | "刚才" | "今天上午" | "今天下午" | "昨天" | "这周早些时候" | "更早";

/** 时间分组：一小时内“刚才”，今天按上午 / 下午，昨天，本周（周一起）更早的几天，其余“更早”。 */
export function timeGroup(millis: number, now = Date.now()): TimeGroup {
  if (now - millis < 3_600_000 && millis <= now) return "刚才";
  const today = dayStart(now);
  if (millis >= today) return new Date(millis).getHours() < 12 ? "今天上午" : "今天下午";
  if (millis >= today - 86_400_000) return "昨天";
  const weekday = (new Date(now).getDay() + 6) % 7;
  if (millis >= today - weekday * 86_400_000) return "这周早些时候";
  return "更早";
}

// ---------------- 扇面几何（窗口 960 × 540，CSS 像素） ----------------
export const FAN = 7;
export const CARD_W = 136, CARD_H = 176, PITCH = 92, ARC = 14, TILT = 2;
export const LIFT_W = 248, LIFT_MIN_H = 176, LIFT_MAX_H = 212;
export const ORIGIN_X = 112, ORIGIN_Y = 13;
export const COMP_H = 514;
/** 手牌顶边（扇面第 4 张的上沿）。 */
export const HAND_TOP = ORIGIN_Y + 32 + LIFT_MAX_H + 28;
export const HAND_W = (FAN - 1) * PITCH + CARD_W;
export const DECK_X = ORIGIN_X + HAND_W + 28, DECK_Y = HAND_TOP + 44, DECK_W = 104, DECK_H = 136;
/** 拈起的牌底边离手牌顶边的距离：盖不住后面常用的牌头（牌头露出 26px）。 */
export const LIFT_GAP = 30;

export const arc = (index: number) => ARC * ((index - 3) / 3) ** 2;

export type Box = { x: number; y: number; w: number; h: number; r: number };

/** 拈起那张的高度：随内容，最小 176、最大 212；短内容不加高。 */
export function liftHeight(item: HistoryItem, kind = cardKind(item)) {
  const text = item.preview ?? "";
  const lines = text.split("\n").length;
  const body = (() => {
    switch (kind) {
      case "long": return Math.min(6, Math.max(lines, Math.ceil([...text].length / 16))) * 20;
      case "code": return Math.min(7, lines) * 18;
      case "file": return Math.min(5, item.file_names.length) * 22 + (item.file_names.length > 5 ? 22 : 0);
      case "image": return 212;
      case "link": { const link = linkParts(text); return link && link.href.length > 60 ? 100 : 0; }
      default: return 0;
    }
  })();
  return Math.max(LIFT_MIN_H, Math.min(LIFT_MAX_H, 76 + body));
}

/** 扇面第 index 张；selected 是拈起的那张（-1 表示没有）。邻牌两侧分别让开 16 / 8 / 4px。 */
export function fanBox(index: number, selected: number, liftH = LIFT_MIN_H, gap = LIFT_GAP): Box {
  let x = ORIGIN_X + index * PITCH;
  const y = HAND_TOP + arc(index);
  if (selected >= 0) {
    const distance = index - selected, away = Math.abs(distance);
    if (distance === 0) {
      // 以原位为中心变宽，但夹在这手牌左右边界内：第 1 张左对齐、第 7 张右对齐。
      const lx = Math.max(ORIGIN_X, Math.min(ORIGIN_X + HAND_W - LIFT_W, x + CARD_W / 2 - LIFT_W / 2));
      return { x: lx, y: HAND_TOP - gap - liftH, w: LIFT_W, h: liftH, r: 0 };
    }
    x += Math.sign(distance) * (away === 1 ? 16 : away === 2 ? 8 : 4);
  }
  return { x, y, w: CARD_W, h: CARD_H, r: (index - 3) * TILT };
}

export const backBox = (index: number): Box => ({ x: ORIGIN_X + index * PITCH, y: HAND_TOP + arc(index) - 26, w: CARD_W, h: 30, r: (index - 3) * TILT });

// ---------------- 摊开几何 ----------------
export const SPREAD_W = 616, ROW_X = 8, ROW_W = 600, ROW_H = 40, ROW_GAP = 4, HEAD_H = 28, SPREAD_TOP = 48, SPREAD_BOTTOM = 48;
export const SPREAD_VIEW = COMP_H - SPREAD_TOP - SPREAD_BOTTOM;
/** 摊开时只为可视区上下这么远的行建元素。 */
export const SPREAD_MARGIN = 160;

/** 选中行展开后的高度（承担预览）；已经看全的类型不展开。 */
export function expandedHeight(item: HistoryItem, kind = cardKind(item)) {
  const lines = (item.preview ?? "").split("\n").filter((line) => line.trim()).length;
  switch (kind) {
    case "long": return Math.max(ROW_H, 28 + 20 * Math.min(4, lines));
    case "code": return Math.max(ROW_H, 28 + 20 * Math.min(4, lines));
    case "link": return linkParts(item.preview ?? "")?.prefix || linkParts(item.preview ?? "")?.tail ? 64 : ROW_H;
    case "image": return 124;
    case "color": return 100;
    case "file": return item.file_names.length > 1 ? Math.min(4, item.file_names.length) * 20 + 22 : ROW_H;
    default: return ROW_H;
  }
}

export type SpreadRow = { y: number; h: number };
export type SpreadHead = { name: string; y: number; index: number; count: number };
export type SpreadLayout = {
  rows: SpreadRow[]; heads: SpreadHead[]; total: number; scroll: number;
  /** 完整落在可视区内的行（数字 1–7 依次对应前 7 条）。 */
  visible: number[];
  /** 需要建元素的行（可视区上下 SPREAD_MARGIN 以内）。 */
  rendered: number[];
};

/**
 * 摊开：算出每一行和每个分组标题在内容坐标里的位置，再按选中项调整滚动。
 * 选中项始终在可视区内；可视区顶边不切半截行（推到行边界），底边保留半截提示下面还有。
 */
export function spreadLayout(items: readonly HistoryItem[], selected: number, previousScroll: number,
  groupOf: ((item: HistoryItem) => string) | null, kinds?: readonly CardKind[]): SpreadLayout {
  const rows: SpreadRow[] = [];
  const heads: SpreadHead[] = [];
  let y = 0;
  let last: string | null = null;
  items.forEach((item, index) => {
    if (groupOf) {
      const name = groupOf(item);
      if (name !== last) { heads.push({ name, y, index, count: 0 }); y += HEAD_H; last = name; }
      heads[heads.length - 1].count += 1;
    }
    const h = index === selected ? expandedHeight(item, kinds?.[index]) : ROW_H;
    rows.push({ y, h });
    y += h + ROW_GAP;
  });
  const total = Math.max(0, y - ROW_GAP);
  let scroll = Math.max(0, previousScroll);
  const current = rows[selected];
  if (current) {
    const head = heads.find((entry) => entry.index === selected);
    const top = head ? head.y : current.y;
    if (top < scroll) scroll = top;
    if (current.y + current.h > scroll + SPREAD_VIEW) scroll = current.y + current.h - SPREAD_VIEW;
  }
  scroll = Math.max(0, Math.min(scroll, Math.max(0, total - SPREAD_VIEW)));
  const blocks = [...heads.map((head) => ({ y: head.y, h: HEAD_H })), ...rows];
  const cut = blocks.find((block) => block.y < scroll - 0.5 && block.y + block.h > scroll + 0.5);
  if (cut && current) {
    // 推到被切那一块的下沿；列表末尾因此可能在底部留一点空白，换来顶边总在行边界上。
    const next = cut.y + cut.h;
    if (current.y >= next) scroll = next;
  }
  const visible: number[] = [];
  const rendered: number[] = [];
  rows.forEach((row, index) => {
    const top = row.y - scroll;
    if (top >= -1 && top + row.h <= SPREAD_VIEW + 1) visible.push(index);
    if (top + row.h >= -SPREAD_MARGIN && top <= SPREAD_VIEW + SPREAD_MARGIN) rendered.push(index);
  });
  return { rows, heads, total, scroll, visible, rendered };
}

/** 一屏的步长（可见条数 − 1）。 */
export const pageStep = (layout: SpreadLayout | null) => Math.max(1, (layout?.visible.length ?? 1) - 1);

// ---------------- 避让外形（set_hit_regions 的 avoid） ----------------
// 后端只按这个外形挪窗口避开光标行；它在同一姿态下固定不变，不随选中哪张、拈起的牌多高变化。
// 按键提示、看全、横幅、通知、已选清单这些临时件不算在内（它们照常进点击区域）。
export const WINDOW_W = 960, WINDOW_H = 540;
/** 阴影外扩，与 Panel.svelte 上报点击区域时的外扩一致。 */
export const SHADOW_PAD = 16;
/** “↑ 常用”小标签：贴在手牌左边 12px，宽度按最长的“↓ 最近”留 72px。 */
export const TAG_ALLOWANCE = 12 + 72;
/** 牌堆后面最多 3 层错开的牌，各向右上错 3px。 */
export const DECK_LAYERS = 9;
/** 扇面底部一行（位置条 / 回收站入口）：在手牌顶边下 216px 起，高 24px，再留 4px 浅阴影。 */
export const FAN_FOOT = 216 + 24 + 4;

export type Outline = { left: number; top: number; width: number; height: number };

const outline = (left: number, top: number, right: number, bottom: number): Outline => {
  const l = Math.max(0, left), t = Math.max(0, top), r = Math.min(WINDOW_W, right), b = Math.min(WINDOW_H, bottom);
  return { left: l, top: t, width: r - l, height: b - t };
};

/**
 * 扇面的固定外形：左边是“↑ 常用”标签，上边是第 1 张以最大高度（212）拈起时的顶边减阴影，
 * 右边是牌堆（含错开的层）加阴影，下边是位置条 / 回收站那一行。常用露头在手牌顶边上方 26px，已含在内。
 */
export function fanOutline(): Outline {
  const left = ORIGIN_X - TAG_ALLOWANCE;
  const top = HAND_TOP - LIFT_GAP - LIFT_MAX_H - SHADOW_PAD;
  const right = DECK_X + DECK_W + DECK_LAYERS + SHADOW_PAD;
  const bottom = HAND_TOP + FAN_FOOT;
  return outline(left, top, right, bottom);
}

/** 摊开的固定外形：底板 616 × 514 加阴影外扩。 */
export function spreadOutline(): Outline {
  return outline(ORIGIN_X - SHADOW_PAD, ORIGIN_Y - SHADOW_PAD, ORIGIN_X + SPREAD_W + SHADOW_PAD, ORIGIN_Y + COMP_H + SHADOW_PAD);
}

export const outlineFor = (pose: "fan" | "spread") => pose === "spread" ? spreadOutline() : fanOutline();
