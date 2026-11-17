export const isMac = navigator.userAgent.includes("Mac");

// 动作键随平台换位；呼出快捷键仍记录物理按键。
export const primary = (event: KeyboardEvent | MouseEvent) => isMac ? event.metaKey : event.ctrlKey;
export const secondary = (event: KeyboardEvent | MouseEvent) => isMac ? event.ctrlKey : event.metaKey;

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
