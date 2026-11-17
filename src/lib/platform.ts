export const isMac = navigator.userAgent.includes("Mac");

// 动作键随平台换位；呼出快捷键仍记录物理按键。
export const primary = (event: KeyboardEvent | MouseEvent) => isMac ? event.metaKey : event.ctrlKey;
export const secondary = (event: KeyboardEvent | MouseEvent) => isMac ? event.ctrlKey : event.metaKey;

// 仅处理后端队列提示的完整原文，不替换内容或其它错误中的词语。
const QUEUE_ERRORS = new Set([
  "逐张放接收不到按键，收起再呼出一次",
  "两分钟没粘贴，逐张放已停止。重新选择剩下的",
  "这张等了十秒还没贴完，逐张放已停止。先看看那个程序，再试一次",
  "剪贴板刚被改过，逐张放已停止，新内容已保留",
  "没放进剪贴板，逐张放已停止。再试一次",
  "不确定这张贴上没有，逐张放已停止。先看看那个程序，别贴两遍",
  "那个程序权限更高，只复制了当前这张，逐张放已停止。按 Ctrl+V 贴上",
  "逐张放没启动好，已停止。收起再呼出一次",
  "没法确认剪贴板的变化，逐张放没开始。再试一次",
  "逐张粘贴只支持粘贴模式，请重新选择。",
  "剪贴板状态无法确认，逐张粘贴未启动；请重新复制后再试。",
]);
export function queueErrorMessage(message: string): string {
  if (!isMac || !QUEUE_ERRORS.has(message)) return message;
  return message === "逐张放接收不到按键，收起再呼出一次" ? "一起粘贴没启动好，收起再呼出一次"
    : message.replace(/逐张放|逐张粘贴/, "一起粘贴").replace("Ctrl+V", "⌘V");
}

export function normalizePanelKey(mac: boolean, mode: string, hasSelected: boolean,
  event: Pick<KeyboardEvent, "key" | "isComposing" | "keyCode">): string {
  return mac && mode === "quick" && !hasSelected && !event.isComposing && event.keyCode !== 229
    && event.key === "Backspace" ? "Delete" : event.key;
}

export function keyLabel(keys: string, physical = false): string {
  if (!isMac) return keys;
  const labels: Record<string, string> = {
    ctrl: physical ? "⌃" : "⌘", control: physical ? "⌃" : "⌘",
    alt: "⌥", shift: "⇧", super: "⌘", win: "⌘", meta: "⌘",
    enter: "↩", return: "↩", "↵": "↩", backspace: "⌫", delete: "⌦",
    tab: "⇥", escape: "⎋", esc: "⎋", space: "空格",
    pageup: "⇞", pgup: "⇞", pagedown: "⇟", pgdn: "⇟", home: "↖", end: "↘",
  };
  return keys.split("+").map((part) => {
    const key = part.trim();
    const named = physical ? key.replace(/^Key([A-Z])$/, "$1").replace(/^Digit([0-9])$/, "$1").replace(/^Backquote$/, "`") : key;
    return labels[named.toLowerCase()] ?? (named === key ? part : named);
  }).join("+").replace(/([⌘⌃⌥⇧])\s*\+\s*/g, "$1");
}
