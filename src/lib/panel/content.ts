import type { HistoryItem } from "../ipc";

export type VisualKind = "short" | "long" | "code" | "link" | "image" | "color" | "files" | "rich";

export const sourceName = (item: HistoryItem) => item.source_name?.trim() || item.source || "来源未知";

export function relativeTime(millis: number, now = Date.now()) {
  const elapsed = Math.max(0, now - millis);
  if (elapsed < 60_000) return "刚刚";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} 分钟前`;
  if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)} 小时前`;
  const date = new Date(millis);
  const today = new Date(now);
  const start = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
  const day = date.getTime();
  if (day >= start - 86_400_000 && day < start) return "昨天";
  if (elapsed < 7 * 86_400_000) return `${Math.floor(elapsed / 86_400_000)} 天前`;
  return date.toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
}

export function linkParts(item: HistoryItem) {
  const value = item.preview.trim();
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    return { url, host: url.host || value };
  } catch {
    return null;
  }
}

export type ColorValue = { value: string; format: "HEX" | "RGB" | "HSL"; rgb?: [number, number, number] };

export function colorValue(item: HistoryItem): ColorValue | null {
  const value = item.preview.trim();
  const hex = /^#([\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.exec(value);
  if (hex) {
    const raw = hex[1];
    const full = raw.length <= 4 ? [...raw].map((part) => part + part).join("") : raw;
    return {
      value,
      format: "HEX",
      rgb: [0, 2, 4].map((offset) => Number.parseInt(full.slice(offset, offset + 2), 16)) as [number, number, number],
    };
  }
  if (/^rgba?\(\s*[\d.]+%?\s*[, ]\s*[\d.]+%?\s*[, ]\s*[\d.]+%?(?:\s*[,/]\s*[\d.]+%?)?\s*\)$/i.test(value)) {
    const channels = value.match(/[\d.]+%?/g)?.slice(0, 3);
    if (channels?.length === 3) {
      return {
        value,
        format: "RGB",
        rgb: channels.map((channel) => channel.endsWith("%")
          ? Math.round(Math.min(100, Number.parseFloat(channel)) * 2.55)
          : Math.max(0, Math.min(255, Math.round(Number.parseFloat(channel))))) as [number, number, number],
      };
    }
    return { value, format: "RGB" };
  }
  if (/^hsla?\(\s*[\d.]+(?:deg)?\s*[, ]\s*[\d.]+%\s*[, ]\s*[\d.]+%(?:\s*[,/]\s*[\d.]+%?)?\s*\)$/i.test(value)) {
    return { value, format: "HSL" };
  }
  return null;
}

function looksLikeCode(value: string) {
  const first = value.trimStart().split("\n", 1)[0];
  if (/^(?:const|let|var|function|class|import|export|return|async|await|interface|type|enum|fn|pub\s|use\s|#include|package\s|def\s|SELECT\s|INSERT\s|UPDATE\s|git\s|pnpm\s|npm\s|cargo\s|winget\s)/i.test(first)) return true;
  const lines = value.split("\n");
  return lines.length > 1 && /[;{}[\]]$/.test(value.trim()) && /(?:=>|\bfunction\b|\bconst\b|\blet\b|[{}];)/.test(value);
}

export function visualKind(item: HistoryItem): VisualKind {
  if (item.kind === "文件") return "files";
  if (item.kind === "图片") return "image";
  if (item.kind === "富文本") return "rich";
  const value = item.preview.trim();
  if (!value) return "short";
  if (linkParts(item)) return "link";
  if (colorValue(item)) return "color";
  if (looksLikeCode(value)) return "code";
  if (value.includes("\n") || [...value].length > 90) return "long";
  return "short";
}

export function typeLabel(item: HistoryItem) {
  const kind = visualKind(item);
  const length = [...item.preview].length;
  const lines = item.preview ? item.preview.split("\n").length : 0;
  switch (kind) {
    case "short": return `短文本 · ${length} 字`;
    case "long": return `长文 · ${length} 字 / ${lines} 行`;
    case "code": return `代码 · ${lines} 行`;
    case "link": return "链接";
    case "image": return item.preview.replace(/^图片\s*/, "图片 · ").replace("×", " × ");
    case "color": return `颜色值 · ${colorValue(item)?.format ?? "值"}`;
    case "files": return `文件 · ${item.file_names.length} 项`;
    case "rich": return "富文本 · 纯文本预览";
  }
}
