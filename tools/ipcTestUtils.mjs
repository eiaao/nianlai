// 各内存 IPC 替身共用的最小工具；场景数据和命令行为仍由各自替身维护。
export const clone = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));

export function merge(base, update) {
  if (base && update && typeof base === "object" && typeof update === "object" && !Array.isArray(base) && !Array.isArray(update)) {
    const result = { ...base };
    for (const [key, value] of Object.entries(update)) result[key] = merge(result[key], value);
    return result;
  }
  return clone(update);
}

export function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
