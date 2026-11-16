export const isMac = navigator.userAgent.includes("Mac");

// 动作键随平台换位；呼出快捷键仍记录物理按键。
export const primary = (event: KeyboardEvent | MouseEvent) => isMac ? event.metaKey : event.ctrlKey;
export const secondary = (event: KeyboardEvent | MouseEvent) => isMac ? event.ctrlKey : event.metaKey;

export function keyLabel(keys: string, physical = false): string {
  if (!isMac) return keys;
  if (physical) keys = keys.replace(/\bKey([A-Z])\b/g, "$1").replace(/\bDigit([0-9])\b/g, "$1").replace(/\bBackquote\b/g, "`");
  const labels: Record<string, string> = {
    ctrl: physical ? "⌃" : "⌘", control: physical ? "⌃" : "⌘",
    alt: "⌥", shift: "⇧", super: "⌘", win: "⌘", meta: "⌘",
    enter: "↩", return: "↩", "↵": "↩", backspace: "⌫", delete: physical ? "⌦" : "⌫",
    tab: "⇥", escape: "⎋", esc: "⎋", space: "空格",
    pageup: "⇞", pgup: "⇞", pagedown: "⇟", pgdn: "⇟", home: "↖", end: "↘",
  };
  return keys.replace(/\b(Control|Ctrl|Alt|Shift|Super|Win|Meta|Enter|Return|Backspace|Delete|Tab|Escape|Esc|Space|PageUp|PgUp|PageDown|PgDn|Home|End)\b|↵/gi,
    (key) => labels[key.toLowerCase()]).replace(/([⌘⌃⌥⇧])\s*\+\s*/g, "$1");
}
