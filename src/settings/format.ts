// 设置页共用的显示格式。

export function size(bytes: number) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(bytes >= 100 * 1024 ** 2 ? 0 : 1)} MB`;
  if (bytes <= 0) return "0 KB";
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

const pad = (n: number) => String(n).padStart(2, "0");

export function time(ms: number) {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 今天的只写时分，其余写月-日 时:分。 */
export function shortTime(ms: number) {
  const d = new Date(ms);
  const today = new Date();
  const clock = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return d.toDateString() === today.toDateString() ? `今天 ${clock}` : `${d.getMonth() + 1}-${pad(d.getDate())} ${clock}`;
}

export function message(error: unknown) {
  return typeof error === "string" ? error : error instanceof Error ? error.message : "操作没有完成，请重试";
}

export function count(n: number) {
  return n.toLocaleString("zh-CN");
}
