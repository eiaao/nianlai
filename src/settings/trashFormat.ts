const DAY_MS = 24 * 60 * 60 * 1000;

// 只描述实际剩余时长，不把“明天到期但不足 24 小时”误称为今天。
export function trashTimeLeft(deletedMs: number, now = Date.now()) {
  const days = Math.ceil((deletedMs + 7 * DAY_MS - now) / DAY_MS);
  return days <= 1 ? "不到 1 天后清掉" : `再过 ${days} 天清掉`;
}
