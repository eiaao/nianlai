// 面板状态和交互逻辑（Svelte 5 runes）。组件只负责显示和把 DOM 事件转到这里；和后端的往来都经过 ipc.ts。
// 这里的时序细节都是 G0 踩过的坑，改之前先读 src/lib/README.md 的“不能动的细节”。
import { tick, untrack } from "svelte";
import * as ipc from "./ipc";
import type { HistoryItem, HistorySnapshot, PanelInteraction, PasteMode, SearchSelection, Timing, UnlistenFn } from "./ipc";
import type { Preferences } from "./preferences";

const HOLD_HINT_MS = 300;
// 页脚撤销提示停留的时长。删除的条目在回收站里保留 7 天，提示消失后 Ctrl+Z 照样能撤销。
const UNDO_MS = 5000;
// 删除收藏要在这段时间内再按一次 Delete 确认（B13）。
const HOLD_MS = 600;
const HINT_MS = 1600;
const LEAVE_MS = 900;
// 删掉后多留一会儿铺满的进度：粉碎在下一次渲染时才给这一行拍底版。
const HOLD_CLEAR_MS = 100;

// 页脚删除标签里的条目摘要：最多 16 个字。
function deleteLabel(item: HistoryItem) {
  const text = [...(item.preview || item.file_names[0] || "").replace(/\s+/g, " ").trim()];
  return text.length > 16 ? `${text.slice(0, 16).join("")}…` : text.join("") || "1 条记录";
}
const DEFAULT_PREFERENCES: Preferences = { layout: "a", theme: "system", motion: "soft", density: "compact" };

export type PasteFeedback = {
  kind: "copied" | "failed"; title: string; detail: string; duration: number; startedAt: number;
};

// 缺少后端确认时不猜权限或原输入控件是否恢复。
export const COPIED_HINT = "已复制，请先点击目标输入位置，再按 Ctrl+V";

type DomChoice = {
  session: number; panelMode: "quick" | "search"; id: number | null; mode: PasteMode;
  selection?: SearchSelection; code: string | null; downAt: number; released: boolean;
};

export const formatTime = (millis: number) => new Date(millis).toLocaleTimeString("zh-CN", { hour12: false });

export class PanelState {
  history = $state<HistorySnapshot>({ items: [], total: 0, recording: false, last_recorded_ms: null, store_status: null });
  favorites = $state<HistoryItem[]>([]);
  favoritePending = $state<number | null>(null);
  // 刚删除的那一条，页脚显示撤销提示用；撤销本身由后端按删除顺序恢复，跨呼出有效。
  undo = $state<{ id: number; session: number; label: string; startedAt: number; duration: number } | null>(null);
  // 删除收藏要按住 Delete：holding 时条目上走进度，提前松开转成 hint 提示一会儿。
  holdDelete = $state<{ id: number; session: number; phase: "holding" | "hint"; keys: string; startedAt: number; duration: number; leaving?: boolean } | null>(null);
  private holdTimer: ReturnType<typeof setTimeout> | null = null;
  // 正在粉碎退出的条目（只有它播放删除动画），以及列表收起后让选中竖条重新量位置的计数。
  leavingId: number | null = null;
  layoutRevision = $state(0);
  private deleting = false;
  private undoTimer: ReturnType<typeof setTimeout> | null = null;
  preferences = $state<Preferences>({ ...DEFAULT_PREFERENCES });
  motion = $derived(this.preferences.motion);
  ctrlDown = $state(false);
  // 按住修饰键超过 HOLD_HINT_MS 才在页脚换成对应提示；快速组合键（Ctrl+D、Ctrl+Enter）不闪。
  heldModifier = $state<"" | "ctrl" | "shift">("");
  previewOpen = $state(false);
  imeActive = $state(false);
  feedback = $state<PasteFeedback | null>(null);
  query = $state("");
  mode = $state<"quick" | "search">("quick");
  interaction = $state<PanelInteraction>("quick");
  visible = $state(false);
  // 启动预热：窗口还隐藏时把真实历史按当前布局渲染一遍（不可见），让首次呼出不再付冷启动的脚本、样式与图标解码开销。
  warming = $state(false);
  busy = $state(false);
  domFocus = $state<"idle" | "pending" | "ready" | "failed">("idle");
  searchStarting = $state(false);
  searchStartFailed = $state(false);
  awaitingRelease = $state(false);
  inputSurface = $derived(this.visible && (this.interaction === "quick" || this.interaction === "activated"));
  interactive = $derived(this.inputSurface && (this.interaction === "quick" || this.domFocus === "ready") && !this.searchStarting && !this.searchStartFailed);
  activationRetrying = $state(false);
  // 实验 NIANLAI_G0_SEARCHBOX_FOCUS（后端随 panel:shown 下发）：开着时快速模式也让搜索框持有页面内焦点。
  // 只用于不激活的普通 quick；activated 使用同一真实输入框承载首字和输入法。
  searchBoxFocus = $state(false);
  locked = $derived(!this.inputSurface || (this.interaction === "quick" && this.mode === "quick" && !this.searchBoxFocus)
    || (this.interaction === "activated" && this.busy));
  selected = $state(0);
  session = $state(0);
  timing = $state<Timing | null>(null);
  message = $state("");
  version = $state("");
  searchItems = $state<HistoryItem[]>([]);
  searchedQuery = $state("");
  searchRevision = $state(0);
  private resultItems = $derived(this.warming && !this.visible ? this.history.items
    : !this.interactive ? [] : this.mode === "search" ? this.searchItems : this.history.items);
  // 本次呼出里冻结的收藏区（条目 ID 顺序）。null 表示还没动过收藏，按当前收藏实时排；
  // 第一次切换收藏时冻结，之后收藏/取消只改星标、不挪位置，下次呼出再按新收藏重排。
  pinnedIds = $state<number[] | null>(null);
  // 收藏顺序以条目上的 favorite_position 为准（后端分配后永不改变）；同位次时按 get_favorites() 的先后。
  private livePins(items: HistoryItem[]) {
    const rank = new Map(this.favorites.map((favorite, index) => [favorite.id, index]));
    const flagged = new Map<number, HistoryItem>();
    for (const item of items) if (item.favorite && !flagged.has(item.id)) flagged.set(item.id, item);
    return [...flagged.values()].sort((a, b) =>
      (a.favorite_position ?? Number.MAX_SAFE_INTEGER) - (b.favorite_position ?? Number.MAX_SAFE_INTEGER)
      || (rank.get(a.id) ?? Number.MAX_SAFE_INTEGER) - (rank.get(b.id) ?? Number.MAX_SAFE_INTEGER)).map((item) => item.id);
  }
  private orderItems(items: HistoryItem[]) {
    const byId = new Map(items.map((item) => [item.id, item]));
    const ordered: HistoryItem[] = [];
    const included = new Set<number>();
    for (const id of this.pinnedIds ?? this.livePins(items)) {
      const item = byId.get(id);
      if (item && !included.has(id)) { ordered.push(item); included.add(id); }
    }
    const pinned = ordered.length;
    for (const item of items) if (!included.has(item.id)) { ordered.push(item); included.add(item.id); }
    return { items: ordered, pinned };
  }
  private ordered = $derived.by(() => this.orderItems(this.resultItems));
  filtered = $derived(this.ordered.items);
  // 被动提示只读展示旧历史，但可操作序列仍为空，继续封锁搜索、数字和鼠标取用。
  private visibleOrder = $derived.by(() => this.interaction === "passive" ? this.orderItems(this.history.items) : this.ordered);
  visibleItems = $derived(this.visibleOrder.items);
  /** 收藏区条数（列表最前面那一段）。 */
  favoriteCount = $derived(this.visibleOrder.pinned);
  currentItem = $derived(this.visibleItems[this.selected] ?? null);

  /** 数字槽位只编最近区：收藏不挤占 1–9（PRD 6.5“常用库不靠置顶挤占历史的位置”）。 */
  slotLabel(index: number) {
    const slot = index - this.favoriteCount;
    return slot >= 0 && slot < 9 ? String(slot + 1).padStart(2, "0") : null;
  }
  private slotIndex(slot: number) { return this.ordered.pinned + slot - 1; }

  // 以下故意不是响应式状态：搜索副作用不能因为它们变化而重跑。
  composing = false;
  compositionEnded = -Infinity;
  firstInput: { session: number; readyAt: number } | null = null;
  // 由 SearchBox、HistoryList 用 bind:this 填进来。
  input!: HTMLInputElement;
  list!: HTMLUListElement;
  // 只在 search() 里递增。真实按键的 input 事件在 bind:value 和委托的 oninput 两个监听器之间会先跑微任务、
  // 发出搜索；若在 oninput 里再递增，刚发出的请求就过期了，回复全被丢掉，列表一直空着。
  searchRequest = 0;
  previousQuery = "";
  previousSession = -1;
  private lastShownSession = -1;
  private foregroundRevision = -1;
  private foregroundSnapshot: { session: number; revision: number; receivedAt: number } | null = null;
  private selectionNonce = 0;
  private closeOnlyEscape: number | null = null;
  private selectionEvents = new WeakSet<KeyboardEvent | MouseEvent>();
  private domEvents = new WeakSet<KeyboardEvent>();
  private heldDomKeys = new Map<string, number>();
  private pendingDom: DomChoice | null = null;
  private focusGeneration = 0;
  private focusTimer: ReturnType<typeof setTimeout> | null = null;
  private focusFrames = new Set<number>();
  private searchTimer: ReturnType<typeof setTimeout> | null = null;
  private searchAfterFocus = false;
  private feedbackTimer: ReturnType<typeof setTimeout> | null = null;
  private modifierTimer: ReturnType<typeof setTimeout> | null = null;
  private pendingModifier: "" | "ctrl" | "shift" = "";

  /** 修饰键状态：activated 来自 DOM 事件，普通 quick 来自钩子（panel:key 的 Mod:*）。 */
  setModifiers = (ctrl: boolean, shift: boolean) => {
    this.ctrlDown = ctrl;
    const next = ctrl && !shift ? "ctrl" : shift && !ctrl ? "shift" : "";
    if (next === this.pendingModifier) return;
    this.pendingModifier = next;
    if (this.modifierTimer !== null) clearTimeout(this.modifierTimer);
    this.modifierTimer = null;
    if (!next) { this.heldModifier = ""; return; }
    this.modifierTimer = setTimeout(() => { this.modifierTimer = null; this.heldModifier = this.pendingModifier; }, HOLD_HINT_MS);
  };

  /** 登记面板的两个副作用（搜索、选中项夹紧和滚动）。必须在根组件初始化期间调用，顺序不要换。 */
  watch() {
    $effect(() => {
      const value = this.query;
      const revision = this.searchRevision;
      // 输入法组合中框里是拼音，先不搜；上屏后 compositionend 会再触发一次。
      if (this.visible && this.interactive && this.mode === "search" && !this.composing) untrack(() => { void revision; void this.search(value); });
    });

    $effect(() => {
      const index = this.selected;
      const count = this.visibleItems.length;
      if (count && index >= count) this.selected = count - 1;
      void tick().then(() => this.list?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" }));
    });
  }

  applySettings = (settings: import("./ipc").AppSettings) => {
    const layout = settings.panel?.layout;
    const motion = settings.feedback?.motion;
    const density = settings.panel?.density;
    this.preferences = {
      layout: layout === "a" || layout === "b" || layout === "c" ? layout : "a",
      theme: settings.theme === "light" || settings.theme === "dark" || settings.theme === "system" ? settings.theme : "system",
      motion: motion === "none" || motion === "spring" || motion === "soft" ? motion : "soft",
      density: density === "comfortable" || density === "compact" ? density : "compact",
    };
    // 与 preferences.ts 保持同一映射；本地实现让 G0 的单文件 Node 回归无需打包器。
    document.documentElement.dataset.layout = this.preferences.layout;
    document.documentElement.dataset.theme = this.preferences.theme;
    document.documentElement.dataset.motion = this.preferences.motion;
    document.documentElement.dataset.density = "compact";
  };

  refreshFavorites = async () => {
    const session = this.session;
    try {
      const favorites = await ipc.getFavorites?.();
      if (!favorites) return;
      // 按回来那一刻的选中项追踪：请求期间用户可能已经移动过，不能拉回请求前的位置。
      const selectedId = this.currentItem?.id;
      this.favorites = favorites;
      if (this.visible && session === this.session && selectedId !== undefined) {
        const index = this.visibleItems.findIndex((item) => item.id === selectedId);
        this.selected = Math.max(0, index);
      }
    } catch {
      // 读不到就按条目上的收藏标记排，结果仍可用，不打扰用户。
    }
  };

  refreshRecordingStatus = async () => {
    const session = this.session;
    const selectedId = this.currentItem?.id;
    try {
      const snapshot = await ipc.getHistory();
      if (!this.visible || session !== this.session) return;
      this.history = snapshot;
      const index = this.visibleItems.findIndex((item) => item.id === selectedId);
      this.selected = Math.max(0, index);
      await this.refreshFavorites();
    } catch {
      if (this.visible && session === this.session) this.message = "状态读取失败，请稍后重试。";
    }
  };

  toggleFavorite = async (id: number) => {
    if (!this.visible || !this.interactive || this.busy || this.favoritePending !== null) return;
    const item = this.filtered.find((entry) => entry.id === id);
    if (!item) return;
    const selectedId = this.currentItem?.id;
    const session = this.session;
    if (this.pinnedIds === null) this.pinnedIds = this.livePins([...this.favorites, ...this.history.items, ...this.searchItems]);
    this.favoritePending = id;
    try {
      const updated = await ipc.setFavorite(id, !item.favorite);
      if (session !== this.session) return;
      if (!updated) { this.message = "这条记录已不在历史里。"; return; }
      // 只补丁更新返回项；收藏区已冻结，这条只换星标、不换位置。
      const patch = (entry: HistoryItem) => entry.id === id ? updated : entry;
      this.history = { ...this.history, items: this.history.items.map(patch) };
      this.searchItems = this.searchItems.map(patch);
      if (updated.favorite) {
        const at = this.favorites.findIndex((entry) => entry.id === id);
        if (at >= 0) this.favorites = this.favorites.map(patch);
        else this.favorites = [...this.favorites, updated];
      } else {
        this.favorites = this.favorites.filter((entry) => entry.id !== id);
      }
      const index = this.filtered.findIndex((entry) => entry.id === selectedId);
      this.selected = Math.max(0, index);
    } catch {
      if (session === this.session) this.message = "收藏状态未保存，请重试。";
    } finally {
      if (session === this.session) this.favoritePending = null;
    }
  };

  private clearUndo() {
    this.undo = null;
    if (this.undoTimer !== null) clearTimeout(this.undoTimer);
    this.undoTimer = null;
  }

  /**
   * 按下 Delete（搜索模式是 Ctrl+Delete，Delete 仍是删字）：普通条目立即移到回收站（可撤销）；
   * 收藏的要按住不放，条目上的进度走满才删，中途松开就取消。按键重复不算新的按下。
   */
  pressDelete = () => {
    const item = this.currentItem;
    if (!this.visible || !this.interactive || this.busy || !item || this.deleting || this.holdDelete?.phase === "holding") return;
    if (!item.favorite) { void this.deleteCurrent(); return; }
    const session = this.session;
    this.clearHold();
    this.holdDelete = { id: item.id, session, phase: "holding", keys: this.mode === "search" ? "Ctrl+Delete" : "Delete", startedAt: Date.now(), duration: HOLD_MS };
    this.holdTimer = setTimeout(() => { this.holdTimer = null; this.finishHold(); }, HOLD_MS);
  };

  // 按满：进度保持铺满，让粉碎从铺满的样子开始；动画结束后再清掉。
  private finishHold() {
    const hold = this.holdDelete;
    if (hold?.phase !== "holding" || hold.leaving) return;
    if (hold.session !== this.session || !this.visible || this.currentItem?.id !== hold.id) { this.clearHold(); return; }
    this.holdDelete = { ...hold, leaving: true };
    const leaving = this.holdDelete;
    void this.deleteCurrent().finally(() => {
      this.holdTimer = setTimeout(() => { this.holdTimer = null; if (this.holdDelete === leaving) this.holdDelete = null; }, HOLD_CLEAR_MS);
    });
  }

  /** 松开 Delete：还没按满就取消，留一句“按住不放”的提示；主线程忙、计时器晚到时按实际按住的时长算。 */
  releaseDelete = () => {
    const hold = this.holdDelete;
    if (!hold || hold.phase !== "holding" || hold.leaving) return;
    if (Date.now() - hold.startedAt >= hold.duration) { this.finishHold(); return; }
    this.clearHold();
    if (hold.session !== this.session || !this.visible) return;
    this.holdDelete = { ...hold, phase: "hint", startedAt: Date.now(), duration: HINT_MS };
    this.holdTimer = setTimeout(() => { this.holdTimer = null; this.holdDelete = null; }, HINT_MS);
  };

  private clearHold() {
    this.holdDelete = null;
    if (this.holdTimer !== null) clearTimeout(this.holdTimer);
    this.holdTimer = null;
  }

  private deleteCurrent = async () => {
    const item = this.currentItem;
    if (!this.visible || !this.interactive || this.busy || !item || this.deleting) return;
    const session = this.session;
    const index = this.selected;
    this.deleting = true;
    try {
      const removed = await ipc.deleteItem?.(item.id);
      if (session !== this.session || !this.visible) return;
      if (!removed) { this.message = "这条记录已经不在了。"; return; }
      this.message = "";
      this.previewOpen = false;
      this.leavingId = item.id;
      setTimeout(() => { if (this.leavingId === item.id) this.leavingId = null; }, LEAVE_MS);
      this.history = { ...this.history, items: this.history.items.filter((entry) => entry.id !== item.id), total: Math.max(0, this.history.total - 1) };
      this.searchItems = this.searchItems.filter((entry) => entry.id !== item.id);
      this.favorites = this.favorites.filter((entry) => entry.id !== item.id);
      // 删除后停在原位置（也就是下一条），没有下一条时退到上一条。
      this.selected = Math.max(0, Math.min(index, this.visibleItems.length - 1));
      this.clearUndo();
      this.undo = { id: item.id, session, label: deleteLabel(item), startedAt: Date.now(), duration: UNDO_MS };
      this.undoTimer = setTimeout(() => { this.undoTimer = null; this.undo = null; }, UNDO_MS);
    } catch {
      if (session === this.session && this.visible) this.message = "删除失败，请重试。";
    } finally {
      this.deleting = false;
    }
  };

  /** Ctrl+Z：恢复最近删除的一条。不限于这次呼出：后端记着本次运行里的删除顺序，可以连续撤销。 */
  undoDelete = async () => {
    if (!this.visible) return;
    const session = this.session;
    this.clearUndo();
    try {
      const restored = await ipc.undoDelete?.();
      if (session !== this.session || !this.visible) return;
      if (!restored) { this.message = "没有可以撤销的删除。"; return; }
      const snapshot = await ipc.getHistory();
      if (session !== this.session || !this.visible) return;
      this.history = snapshot;
      if (restored.favorite) {
        const favorites = await ipc.getFavorites?.();
        if (favorites && session === this.session) this.favorites = favorites;
      }
      if (this.mode === "search") this.searchRevision += 1;
      this.message = "";
      const index = this.visibleItems.findIndex((entry) => entry.id === restored.id);
      if (index >= 0) this.selected = index;
    } catch {
      if (session === this.session && this.visible) this.message = "撤销失败，请重试。";
    }
  };

  togglePreview = () => {
    if (!this.visible || !this.interactive || this.busy || this.awaitingRelease || this.mode !== "quick") return;
    if (this.previewOpen) this.previewOpen = false;
    else if (this.currentItem) this.previewOpen = true;
  };

  private clearFeedback() {
    this.feedback = null;
    if (this.feedbackTimer !== null) clearTimeout(this.feedbackTimer);
    this.feedbackTimer = null;
  }

  private showFailure(message: string) {
    this.clearFeedback();
    this.feedback = { kind: "failed", title: "粘贴失败", detail: message, duration: 5000, startedAt: Date.now() };
    this.feedbackTimer = setTimeout(() => { this.clearFeedback(); }, 5000);
  }

  search = async (value: string) => {
    const requestId = ++this.searchRequest;
    const currentSession = this.session;
    const reset = value !== this.previousQuery || currentSession !== this.previousSession;
    this.previousQuery = value;
    this.previousSession = currentSession;
    if (reset) { this.selected = 0; this.searchItems = []; }
    try {
      const reply = await ipc.searchHistory(value, requestId);
      if (reply.request_id !== this.searchRequest || !this.visible || !this.interactive || this.mode !== "search" || this.session !== currentSession) return;
      const selectedId = this.filtered[this.selected]?.id;
      const scrollTop = this.list?.scrollTop ?? 0;
      this.searchItems = reply.items;
      this.searchedQuery = value;
      const index = reset ? 0 : this.visibleItems.findIndex((item) => item.id === selectedId);
      this.selected = Math.max(0, index);
      await tick();
      if (!reset && requestId === this.searchRequest && this.list) this.list.scrollTop = scrollTop;
    } catch {
      if (requestId === this.searchRequest && this.visible && currentSession === this.session) this.message = "搜索失败，请查看 G0 日志。";
    }
  };

  // 搜索结果高亮按 searchedQuery（已经拿到回复的那次搜索词）算，不按正在输入的 query。
  highlights(text: string) {
    const lower = (value: string) => value.replace(/[A-Z]/g, (c) => c.toLowerCase());
    const normalized = lower(text);
    const hits = new Set<number>();
    if (this.mode === "search") for (const word of this.searchedQuery.split(/\s+/).filter(Boolean).map(lower)) {
      let at = normalized.indexOf(word);
      while (at >= 0) {
        for (let i = at; i < at + word.length; i++) hits.add(i);
        at = normalized.indexOf(word, at + 1);
      }
    }
    const parts: { text: string; hit: boolean }[] = [];
    let offset = 0;
    for (const character of text) {
      const hit = hits.has(offset);
      const last = parts.at(-1);
      if (last?.hit === hit) last.text += character;
      else parts.push({ text: character, hit });
      offset += character.length;
    }
    return parts;
  }

  // 发命令；只对仍可操作的同一会话提示失败，已确认的被动结果不被迟到错误覆盖。
  private async command(call: () => Promise<unknown>, session = this.session) {
    try { await call(); }
    catch {
      if (this.session !== session || !this.visible || this.interaction === "passive" || this.interaction === "close_only") return;
      this.message = "操作未完成，请查看 G0 日志。";
      this.busy = false;
    }
  }

  private inputHasFocus() {
    return document.hasFocus() && document.activeElement === this.input;
  }

  private clearDomChoice(clearHeld = false) {
    this.closeOnlyEscape = null;
    this.pendingDom = null;
    this.awaitingRelease = false;
    if (clearHeld) this.heldDomKeys.clear();
    if (["请松开按键以完成取用。", "请松开按键以收起面板。"].includes(this.message)) this.message = "";
  }

  private cancelDomFocus() {
    this.focusGeneration += 1;
    if (this.focusTimer !== null) clearTimeout(this.focusTimer);
    this.focusTimer = null;
    this.focusFrames.forEach(cancelAnimationFrame);
    this.focusFrames.clear();
  }

  private finishDomFocus(ready: boolean, generation: number) {
    if (generation !== this.focusGeneration || !this.visible || this.interaction !== "activated") return;
    const openSearch = ready && this.searchAfterFocus;
    this.searchAfterFocus = false;
    this.cancelDomFocus();
    this.domFocus = ready ? "ready" : "failed";
    if (!ready) this.clearDomChoice(true);
    if (openSearch) this.requestActivatedSearch();
  }

  private prepareDomFocus(openSearch = false) {
    if (!this.visible || this.interaction !== "activated" || this.busy) return;
    this.cancelDomFocus();
    this.clearDomChoice(true);
    this.domFocus = "pending";
    this.searchAfterFocus = openSearch;
    const generation = this.focusGeneration;
    const session = this.session;
    const current = () => generation === this.focusGeneration && this.visible && this.session === session && this.interaction === "activated";
    const frame = (callback: () => void) => {
      const id = requestAnimationFrame(() => { this.focusFrames.delete(id); if (current()) callback(); });
      this.focusFrames.add(id);
    };
    // 动画帧停滞也必须结束准备态，失败后由鼠标本地重试，不冒充后端激活失败。
    this.focusTimer = setTimeout(() => this.finishDomFocus(false, generation), 500);
    void tick().then(() => {
      if (!current()) return;
      try { if (!this.inputHasFocus()) this.input?.focus({ preventScroll: true }); }
      catch { this.finishDomFocus(false, generation); return; }
      frame(() => frame(() => this.finishDomFocus(this.inputHasFocus(), generation)));
    });
  }

  private ensureDomInput(event: Event) {
    if (!this.visible || this.interaction !== "activated" || event.isTrusted !== true || event.target !== this.input || !this.inputHasFocus()) return false;
    // 实际输入事件也能确认焦点；不再调用 focus，避免打断已经开始的组合。
    if (this.domFocus !== "ready") this.finishDomFocus(true, this.focusGeneration);
    return true;
  }

  private clearSearchTimer() {
    if (this.searchTimer !== null) clearTimeout(this.searchTimer);
    this.searchTimer = null;
  }

  private requestActivatedSearch() {
    if (!this.visible || this.interaction !== "activated" || this.mode !== "quick" || this.busy
      || this.searchStarting || this.searchStartFailed || !this.inputHasFocus()) return;
    const session = this.session;
    this.clearDomChoice();
    this.searchStarting = true;
    const failed = () => {
      if (!this.visible || this.session !== session || this.mode !== "quick" || !this.searchStarting) return;
      this.clearSearchTimer();
      this.searchStarting = false;
      this.searchStartFailed = true;
      this.message = "未能进入搜索，请关闭面板后重新呼出。";
    };
    this.searchTimer = setTimeout(failed, 1000);
    void ipc.startSearch(session).catch(failed);
  }

  retryDomFocus = (event: MouseEvent) => {
    if (event.isTrusted === true && event.type === "click" && event.button === 0 && event.detail > 0
      && this.visible && this.interaction === "activated" && !this.searchStartFailed) this.prepareDomFocus();
  };

  inputBlur = () => {
    this.clearDomChoice(true);
    if (this.visible && this.interaction === "activated") {
      this.cancelDomFocus();
      this.domFocus = "failed";
    }
    if (this.composing) { this.composing = false; this.imeActive = false; this.compositionEnded = performance.now(); this.searchRevision += 1; }
  };

  windowBlur = () => { this.setModifiers(false, false); this.inputBlur(); };

  private domCode(event: KeyboardEvent) {
    return event.code || `${event.key}:${event.location}`;
  }

  private observeModifiers(event: KeyboardEvent | MouseEvent) {
    for (const [key, held] of [["Control", event.ctrlKey], ["Shift", event.shiftKey], ["Alt", event.altKey], ["Meta", event.metaKey]] as const) {
      if (held) this.heldDomKeys.set(`@${key}`, Math.max(this.heldDomKeys.get(`@${key}`) ?? -Infinity, event.timeStamp));
    }
  }

  private armDomChoice(choice: DomChoice) {
    if (this.pendingDom) return;
    this.pendingDom = choice;
    this.awaitingRelease = true;
    this.message = choice.id === null ? "请松开按键以收起面板。" : "请松开按键以完成取用。";
    this.releaseDomChoice();
  }

  private releaseDomChoice() {
    const choice = this.pendingDom;
    if (!choice || !choice.released || this.heldDomKeys.size) return;
    this.clearDomChoice();
    if (!this.visible || this.session !== choice.session || this.interaction !== "activated" || !this.inputHasFocus() || this.composing) return;
    // 关闭动作优先于复制，即使请求已在处理或搜索正在切换，也允许后端取消。
    if (choice.id === null) { void this.command(() => ipc.hidePanel(choice.session), choice.session); return; }
    if (this.mode !== choice.panelMode || !this.interactive || this.busy) return;
    if (choice.selection && (!this.foregroundSnapshot || choice.selection.revision !== this.foregroundRevision)) {
      this.message = "选择期间前台状态已变化，请重新选择条目。";
      return;
    }
    const index = this.filtered.findIndex((item) => item.id === choice.id);
    if (index < 0) { this.message = "条目已更新，请重新选择。"; return; }
    this.selected = index;
    this.busy = true;
    void this.command(() => ipc.pasteItem(choice.session, choice.id!, choice.mode, choice.selection), choice.session);
  }

  keyup = (event: KeyboardEvent) => {
    this.setModifiers(event.ctrlKey, event.shiftKey);
    // 页面自己收到的 Delete（activated 快速模式、搜索模式的 Ctrl+Delete）在这里松开；松开 Ctrl 不算。
    if (event.key === "Delete" && event.isTrusted === true) this.releaseDelete();
    if (this.interaction === "close_only") {
      const downAt = this.closeOnlyEscape;
      this.closeOnlyEscape = null;
      if (this.visible && downAt !== null && event.isTrusted === true && event.type === "keyup" && event.key === "Escape"
        && Number.isFinite(event.timeStamp) && event.timeStamp >= downAt && event.timeStamp <= performance.now()
        && !event.isComposing && !this.composing && document.hasFocus()) {
        event.preventDefault(); this.close();
      }
      return;
    }
    if (this.interaction !== "activated" || !this.ensureDomInput(event) || event.isTrusted !== true || this.domEvents.has(event)
      || !Number.isFinite(event.timeStamp) || event.timeStamp > performance.now()) return;
    this.domEvents.add(event);
    const code = this.domCode(event);
    // 迟到的任何释放事件都不能清除新一轮按下证据，修饰键也遵守同一时间边界。
    if (event.timeStamp < (this.pendingDom?.downAt ?? -Infinity)
      || event.timeStamp < (this.heldDomKeys.get(code) ?? -Infinity)) return;
    this.heldDomKeys.delete(code);
    for (const [key, held] of [["Control", event.ctrlKey], ["Shift", event.shiftKey], ["Alt", event.altKey], ["Meta", event.metaKey]] as const) {
      if ((event.key === key || event.key === `${key}Graph`) && !held && event.timeStamp >= (this.heldDomKeys.get(`@${key}`) ?? -Infinity)) this.heldDomKeys.delete(`@${key}`);
    }
    if (this.pendingDom?.code === code) {
      event.preventDefault();
      if (event.timeStamp < this.pendingDom.downAt || event.isComposing || this.composing) this.clearDomChoice();
      else this.pendingDom.released = true;
    }
    this.releaseDomChoice();
  };

  // 回调一收到通知就保存同一单调时钟的时间。乱序/重复保留高水位，但挡住这之前已生成的 DOM 事件。
  private receiveForegroundRevision(revision: number, receivedAt: number) {
    if (!Number.isSafeInteger(revision) || revision < 0 || !Number.isFinite(receivedAt) || receivedAt < 0
      || (this.foregroundSnapshot && receivedAt < this.foregroundSnapshot.receivedAt)) {
      this.foregroundSnapshot = null;
      return;
    }
    this.foregroundRevision = Math.max(this.foregroundRevision, revision);
    this.foregroundSnapshot = { session: this.session, revision: this.foregroundRevision, receivedAt };
    if (this.pendingDom?.selection && this.pendingDom.selection.revision !== this.foregroundRevision) {
      this.clearDomChoice();
      this.message = "选择期间前台状态已变化，请重新选择条目。";
    }
  }

  private captureSelection(event?: KeyboardEvent | MouseEvent): SearchSelection | null {
    if (!event || event.isTrusted !== true || this.selectionEvents.has(event)) return null;
    this.selectionEvents.add(event);
    const now = performance.now();
    const snapshot = this.foregroundSnapshot;
    // Chromium 的 DOM timeStamp 与 performance.now 使用同一起点；未来/非有限时间不能证明这个顺序。
    if (!snapshot || snapshot.session !== this.session || !Number.isFinite(now) || !Number.isFinite(event.timeStamp)
      || event.timeStamp < snapshot.receivedAt || event.timeStamp > now || this.composing
      || now - this.compositionEnded < 50 || event.timeStamp - this.compositionEnded < 50) return null;
    if (event.type === "keydown") {
      const key = event as KeyboardEvent;
      if (key.repeat || key.isComposing || key.keyCode === 229 || key.altKey || key.metaKey) return null;
      if (!(key.key === "Enter" && !(key.ctrlKey && key.shiftKey)) && !(key.ctrlKey && /^[1-9]$/.test(key.key))) return null;
    } else if (event.type === "click") {
      const pointer = event as MouseEvent;
      if (pointer.button !== 0 || pointer.detail <= 0) return null;
    } else return null;
    if (!Number.isSafeInteger(this.selectionNonce + 1)) return null;
    return { revision: snapshot.revision, nonce: ++this.selectionNonce };
  }

  private setInteraction(interaction: PanelInteraction) {
    // 两种结束态都不能被迟到的激活/模式通知重新开放。
    const transitions: Record<PanelInteraction, readonly PanelInteraction[]> = {
      quick: ["quick", "passive", "close_only"],
      activation_pending: ["activation_pending", "activation_failed", "activated", "passive", "close_only"],
      activation_failed: ["activation_pending", "activation_failed", "activated", "passive", "close_only"],
      activated: ["activated", "passive", "close_only"],
      passive: ["passive"],
      close_only: ["close_only"],
    };
    if (!transitions[this.interaction]?.includes(interaction)) return;
    if (this.interaction === interaction) return;
    const enteringPassive = interaction === "passive" && this.interaction !== "passive";
    const selectedId = enteringPassive ? this.currentItem?.id : undefined;
    this.interaction = interaction;
    this.clearDomChoice(true);
    this.clearSearchTimer();
    this.searchStarting = false;
    this.searchStartFailed = false;
    this.cancelDomFocus();
    this.domFocus = "idle";
    if (interaction !== "quick") {
      this.searchRequest += 1;
      this.mode = "quick";
      this.searchBoxFocus = false;
      this.composing = false;
      this.firstInput = null;
      if (interaction !== "activated") this.input?.blur();
    }
    if (interaction === "activated") this.prepareDomFocus();
    if (enteringPassive) {
      const index = this.visibleItems.findIndex((item) => item.id === selectedId);
      this.selected = Math.max(0, index);
      this.busy = true; this.message = "正在确认复制结果…";
    }
  }

  move = (offset: number) => {
    if (this.holdDelete) this.clearHold();
    this.selected = Math.max(0, Math.min(this.filtered.length - 1, this.selected + offset));
  };

  choose = async (index = this.selected, pasteMode: PasteMode = "paste", event?: KeyboardEvent | MouseEvent) => {
    if (!this.visible || !this.interactive || this.busy || this.pendingDom || !this.filtered[index]) return;
    // 第一次 await 之前固定全部请求字段；绝不在 IPC/读库返回后换用更新的条目或 revision。
    const session = this.session;
    const id = this.filtered[index].id;
    const mode = pasteMode;
    const selection = this.mode === "search" ? this.captureSelection(event) : undefined;
    if (selection === null) {
      this.message = this.foregroundSnapshot ? "无法确认选择与前台状态的先后，请重新选择条目。" : "前台状态不可用，请关闭面板后重新呼出。";
      return;
    }
    if (this.interaction === "activated") {
      if (!event || event.isTrusted !== true || !this.inputHasFocus()) return;
      if (event.type === "keydown") {
        const key = event as KeyboardEvent;
        if (key.repeat || key.isComposing || this.composing || key.keyCode === 229
          || performance.now() - this.compositionEnded < 50 || key.timeStamp - this.compositionEnded < 50) return;
        this.observeModifiers(key);
        this.heldDomKeys.set(this.domCode(key), key.timeStamp);
        this.armDomChoice({ session, panelMode: this.mode, id, mode, selection, code: this.domCode(key), downAt: key.timeStamp, released: false });
        return;
      }
      if (event.type !== "click" || (event as MouseEvent).button !== 0 || (event as MouseEvent).detail <= 0 || this.composing
        || performance.now() - this.compositionEnded < 50 || event.timeStamp - this.compositionEnded < 50) return;
      this.observeModifiers(event as MouseEvent);
      if (this.heldDomKeys.size) {
        this.armDomChoice({ session, panelMode: this.mode, id, mode, selection, code: null, downAt: event.timeStamp, released: true });
        return;
      }
    }
    this.selected = index;
    this.message = "";
    this.busy = true;
    await this.command(() => ipc.pasteItem(session, id, mode, selection), session);
  };

  navigate = (key: string, event?: KeyboardEvent) => {
    if (!this.visible || !this.interactive) return;
    switch (key) {
      case "Space": this.togglePreview(); break;
      case "ArrowUp": this.move(-1); break;
      case "ArrowDown": this.move(1); break;
      case "ArrowLeft": if (this.preferences.layout === "c" && this.mode === "quick") this.move(-1); break;
      case "ArrowRight": if (this.preferences.layout === "c" && this.mode === "quick") this.move(1); break;
      case "PageUp": this.move(this.preferences.layout === "c" ? -3 : -7); break;
      case "PageDown": this.move(this.preferences.layout === "c" ? 3 : 7); break;
      case "Home": this.selected = 0; break;
      case "End": this.selected = Math.max(0, this.filtered.length - 1); break;
      case "Enter": void this.choose(this.selected, "paste", event); break;
      case "Shift+Enter": void this.choose(this.selected, "copy", event); break;
      case "Ctrl+Enter": void this.choose(this.selected, "text", event); break;
      case "Ctrl+D": if (this.currentItem) void this.toggleFavorite(this.currentItem.id); break;
      // 钩子在按下时送 Delete:down、松开时送 Delete。
      case "Delete:down": if (this.mode === "quick") this.pressDelete(); break;
      case "Delete": if (this.mode === "quick") this.releaseDelete(); break;
      case "Ctrl+Z": void this.undoDelete(); break;
      case "Escape":
        if (this.previewOpen) { this.previewOpen = false; break; }
        if (this.interaction === "activated" && event) this.armDomChoice({ session: this.session, panelMode: this.mode,
          id: null, mode: "copy", code: this.domCode(event), downAt: event.timeStamp, released: false });
        else this.close();
        break;
      default: if (/^[1-9]$/.test(key)) void this.choose(this.slotIndex(Number(key)), "paste", event);
    }
  };

  // 普通 Quick 走钩子；Activated 在同一真实输入框中处理快速键和自然搜索输入。
  keydown = (event: KeyboardEvent) => {
    this.setModifiers(event.ctrlKey, event.shiftKey);
    if (this.interaction === "close_only") {
      if (this.visible && event.isTrusted === true && event.type === "keydown" && event.key === "Escape"
        && !event.repeat && !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey
        && !event.isComposing && !this.composing && Number.isFinite(event.timeStamp) && event.timeStamp <= performance.now()
        && event.timeStamp >= 0 && event.timeStamp - this.compositionEnded >= 50 && document.hasFocus()) {
        event.preventDefault(); this.closeOnlyEscape = event.timeStamp;
      }
      return;
    }
    if (this.interaction === "activated") {
      if (!this.ensureDomInput(event) || event.type !== "keydown" || this.domEvents.has(event)
        || !Number.isFinite(event.timeStamp) || event.timeStamp < 0 || event.timeStamp > performance.now()) return;
      this.domEvents.add(event);
      this.heldDomKeys.set(this.domCode(event), event.timeStamp);
      this.observeModifiers(event);
      if (event.isComposing || this.composing || event.keyCode === 229) { this.clearDomChoice(); return; }
      if (event.key === "Escape") {
        event.preventDefault();
        if (this.previewOpen && !event.repeat && performance.now() - this.compositionEnded >= 50 && event.timeStamp - this.compositionEnded >= 50) {
          this.clearDomChoice();
          this.previewOpen = false;
          return;
        }
        if (!event.repeat && performance.now() - this.compositionEnded >= 50 && event.timeStamp - this.compositionEnded >= 50) {
          this.clearDomChoice();
          this.armDomChoice({ session: this.session, panelMode: this.mode, id: null, mode: "copy",
            code: this.domCode(event), downAt: event.timeStamp, released: false });
        }
        return;
      }
      if (this.busy || this.searchStartFailed) { event.preventDefault(); return; }
      if (this.mode === "quick") {
        if (this.searchStarting) return;
        if ((!event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "/")
          || (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key.toLowerCase() === "f")) {
          event.preventDefault();
          if (!event.repeat) this.requestActivatedSearch();
          return;
        }
        const unmodified = !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
        const horizontal = this.preferences.layout === "c" && (event.key === "ArrowLeft" || event.key === "ArrowRight");
        const ctrlOnly = event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
        const action = ctrlOnly && event.key.toLowerCase() === "d"
          ? "Ctrl+D"
          : ctrlOnly && event.key.toLowerCase() === "z"
          ? "Ctrl+Z"
          : unmodified && event.key === "Delete"
          ? "Delete:down"
          : event.key === "Enter" && !event.altKey && !event.metaKey && !(event.ctrlKey && event.shiftKey)
          ? event.ctrlKey ? "Ctrl+Enter" : event.shiftKey ? "Shift+Enter" : "Enter"
          : unmodified && (horizontal || /^[1-9]$/.test(event.key) || event.key === " " || ["Escape", "ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"].includes(event.key)) ? event.key === " " ? "Space" : event.key : null;
        if (action) {
          event.preventDefault();
          const navigation = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"].includes(action);
          if (performance.now() - this.compositionEnded < 50 || event.timeStamp - this.compositionEnded < 50 || (!navigation && event.repeat)) return;
          this.navigate(action, event);
        } else if ((event.ctrlKey && ["p", "r", "+", "-", "=", "0"].includes(event.key.toLowerCase())) || ["F5", "F7"].includes(event.key)) event.preventDefault();
        return;
      }
    }
    if (!this.visible || !this.interactive || this.mode !== "search") return;
    if (event.isComposing || this.composing || event.keyCode === 229) return;
    // 提交动作忽略长按重复；保留原 50ms 组合上屏保护和方向键长按导航。
    if (event.ctrlKey && !event.altKey && !event.metaKey && /^[1-9]$/.test(event.key)) {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) void this.choose(this.slotIndex(Number(event.key)), "paste", event);
      return;
    }
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "Enter") {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) this.navigate("Ctrl+Enter", event);
      return;
    }
    // 搜索模式删除当前结果：Delete 留给搜索框删字，这里用 Ctrl+Delete。
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "Delete") {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) this.pressDelete();
      return;
    }
    if ((event.ctrlKey && ["f", "p", "r", "+", "-", "=", "0"].includes(event.key.toLowerCase())) || event.key === "F5" || event.key === "F7") {
      event.preventDefault();
      if (event.key.toLowerCase() === "f") this.input?.focus({ preventScroll: true });
      return;
    }
    if (!event.ctrlKey && !event.altKey && !event.metaKey && (event.target === this.input || event.key === "Escape") && ["Escape", "Enter", "ArrowUp", "ArrowDown"].includes(event.key)) {
      event.preventDefault();
      if (performance.now() - this.compositionEnded < 50 || (event.key === "Enter" && event.repeat)) return;
      this.navigate(event.key === "Enter" && event.shiftKey ? "Shift+Enter" : event.key, event);
    }
  };

  // 诊断：补发后搜索框收到的第一个输入，是直接上屏（insertText）还是进了输入法（insertCompositionText）。只报类型，不报内容。
  reportFirstInput = (event: InputEvent) => {
    if (this.firstInput?.session !== this.session) return;
    const afterReadyMs = performance.now() - this.firstInput.readyAt;
    this.firstInput = null;
    void ipc.reportSearchInput(this.session, event.inputType, event.isComposing, afterReadyMs).catch(() => {});
  };

  beforeInput = (event: InputEvent) => {
    this.reportFirstInput(event);
    if (!this.visible || this.interaction !== "activated" || this.mode !== "quick") return;
    if (this.busy || this.searchStartFailed || !this.ensureDomInput(event)) { event.preventDefault(); return; }
    if (event.inputType.startsWith("insert")) this.requestActivatedSearch();
  };

  searchPointer = (event: PointerEvent) => {
    if (this.visible && this.interaction === "activated" && !this.busy && !this.searchStartFailed) {
      if (event.isTrusted !== true) return;
      event.preventDefault();
      if (!this.inputHasFocus() || this.domFocus !== "ready") this.prepareDomFocus(true);
      else this.requestActivatedSearch();
      return;
    }
    if (this.visible && this.interactive && this.mode === "quick" && !this.busy) {
      event.preventDefault();
      const session = this.session;
      void this.command(() => ipc.startSearch(session), session);
    }
  };

  retryActivation = async (event: MouseEvent) => {
    if (!this.visible || this.interaction !== "activation_failed" || this.activationRetrying
      || event.isTrusted !== true || event.type !== "click" || event.button !== 0 || event.detail <= 0) return;
    const session = this.session;
    this.activationRetrying = true;
    try { await this.command(() => ipc.retryActivation(session), session); }
    finally { if (this.session === session) this.activationRetrying = false; }
  };

  close = (event?: MouseEvent) => {
    if (!this.visible) return;
    const session = this.session;
    this.clearDomChoice();
    if (this.interaction === "activated" && event?.isTrusted === true && this.heldDomKeys.size && this.inputHasFocus()) {
      this.armDomChoice({ session, panelMode: this.mode, id: null, mode: "copy", code: null, downAt: event.timeStamp, released: true });
      return;
    }
    void this.command(() => ipc.hidePanel(session), session);
  };

  startComposition = (event?: CompositionEvent) => {
    this.composing = true;
    this.imeActive = true;
    this.clearDomChoice();
    if (event && this.mode === "quick" && this.ensureDomInput(event)) this.requestActivatedSearch();
  };

  endComposition = () => { this.composing = false; this.imeActive = false; this.compositionEnded = performance.now(); this.searchRevision += 1; };

  private async prewarm() {
    try {
      const snapshot = await ipc.getHistory?.();
      if (!snapshot || this.visible) return;
      this.history = snapshot;
      this.warming = true;
      await tick();
      // 读一次布局尺寸，强制样式计算与排版；隐藏窗口里 requestAnimationFrame 不一定会跑，不能等帧。
      void document.body?.offsetHeight;
      await new Promise((resolve) => setTimeout(resolve, 30));
    } catch {
      // 预热只是优化，失败不影响就绪。
    } finally {
      this.warming = false;
    }
  }

  /** onMount 时调用：读版本号、订阅后端事件，全部订阅好后再发 frontend_ready；返回卸载时的清理函数。 */
  connect = () => {
    let disposed = false;
    void ipc.appVersion().then((value) => { if (!disposed) this.version = value; }).catch(() => {});
    const settingsReady = Promise.resolve(ipc.getSettings?.()).then((settings) => { if (!disposed && settings) this.applySettings(settings); }).catch(() => {});
    void this.refreshFavorites();
    const unlisten: UnlistenFn[] = [];
    const frames = new Set<number>();
    let fetching = false;
    let refreshPending = false;
    const refreshHistory = async () => {
      refreshPending = true;
      if (fetching) return;
      fetching = true;
      try {
        while (refreshPending && this.visible && !disposed) {
          refreshPending = false;
          const current = this.session;
          const snapshot = await ipc.getHistory();
          if (!disposed && this.visible && current === this.session) {
            const selectedId = this.filtered[this.selected]?.id;
            this.history = snapshot;
            if (this.mode === "search") this.searchRevision += 1;
            const index = this.visibleItems.findIndex((item) => item.id === selectedId);
            this.selected = index >= 0 ? index : 0;
            void this.refreshFavorites();
          }
        }
      } catch { if (!disposed && this.interaction !== "close_only") this.message = "读取历史失败，请查看 G0 日志。"; }
      finally { fetching = false; }
    };
    const frame = (callback: () => void) => {
      const id = requestAnimationFrame(() => { frames.delete(id); if (!disposed) callback(); });
      frames.add(id);
    };
    const subscribe = async <T>(on: (handler: (payload: T) => void) => Promise<UnlistenFn>, handler: (payload: T) => void) => {
      const stop = await on((payload) => { if (!disposed) handler(payload); });
      if (disposed) stop(); else unlisten.push(stop);
    };
    const initialize = async () => {
      if (ipc.onSettingsChanged) await subscribe(ipc.onSettingsChanged, (settings) => this.applySettings(settings));
      await subscribe(ipc.onPanelShown, (payload) => {
        const receivedAt = performance.now();
        if (!Number.isSafeInteger(payload.session) || payload.session < this.session) return;
        if (payload.terminal_feedback === true) {
          // 同一通用请求已隐藏后，只允许恢复关闭提示，不开启新选择或搜索许可。
          if (payload.session !== this.session || payload.session !== this.lastShownSession
            || this.visible || this.interaction === "close_only" || payload.interaction !== "close_only") return;
        } else if (payload.session <= this.lastShownSession) return;
        this.lastShownSession = payload.session;
        this.searchRequest += 1;
        this.session = payload.session;
        this.visible = true;
        this.mode = "quick";
        this.interaction = payload.interaction;
        this.query = "";
        // 新呼出优先选中最近区第一条；没有最近记录时才落到第一条收藏。
        this.history = payload.history;
        const firstRecent = this.history.items.find((item) => !item.favorite);
        this.selected = firstRecent ? Math.max(0, this.filtered.findIndex((item) => item.id === firstRecent.id)) : 0;
        this.busy = false;
        this.message = "";
        this.clearFeedback();
        this.previewOpen = false;
        this.setModifiers(false, false);
        this.imeActive = false;
        this.favoritePending = null;
        this.pinnedIds = null;
        this.clearUndo(); this.clearHold();
        void this.refreshFavorites();
        this.activationRetrying = false;
        this.composing = false;
        this.compositionEnded = -Infinity;
        this.firstInput = null;
        this.clearDomChoice(true);
        this.domEvents = new WeakSet();
        this.cancelDomFocus();
        this.domFocus = "idle";
        this.clearSearchTimer();
        this.searchStarting = false;
        this.searchStartFailed = false;
        this.foregroundRevision = -1;
        this.foregroundSnapshot = null;
        this.selectionNonce = 0;
        this.selectionEvents = new WeakSet();
        this.receiveForegroundRevision(payload.foregroundRevision, receivedAt);
        this.searchBoxFocus = this.interaction === "quick" && payload.experiments?.searchBoxFocus === true;
        this.timing = { session: payload.session, hotkeyToShowMs: payload.hotkeyToShowMs };
        const current = this.session;
        // 实验开着时等 readonly 摘掉再聚焦，让焦点一直停在搜索框；关着时照旧 blur，按键全交给钩子。
        if (this.interaction === "activated") this.prepareDomFocus();
        else if (this.searchBoxFocus) void tick().then(() => { if (!disposed && this.visible && this.session === current && this.interaction === "quick" && this.mode === "quick") this.input?.focus({ preventScroll: true }); });
        else this.input?.blur();
        frame(() => frame(() => {
          if (this.visible && this.session === current) void this.command(() => ipc.reportPainted(current));
        }));
      });
      await subscribe(ipc.onPanelInteraction, (payload) => {
        if (this.visible && payload.session === this.session) this.setInteraction(payload.interaction);
      });
      await subscribe(ipc.onForegroundRevision, (payload) => {
        const receivedAt = performance.now();
        if (this.visible && payload.session === this.session) this.receiveForegroundRevision(payload.revision, receivedAt);
      });
      await subscribe(ipc.onPanelKey, (payload) => {
        if (!this.visible || payload.session !== this.session) return;
        // 钩子放行修饰键给目标程序，同时把按住状态报过来，只用于页脚提示。
        if (payload.key.startsWith("Mod:")) {
          if (this.interaction !== "activated") this.setModifiers(payload.key.includes("Ctrl"), payload.key.includes("Shift"));
          return;
        }
        if (this.interaction === "quick" && this.mode === "quick") this.navigate(payload.key);
        // 搜索模式不导航，但收藏键全局可用（钩子已吞键，必须经 panel:key 送达）。
        else if (payload.key === "Ctrl+D" && this.currentItem) void this.toggleFavorite(this.currentItem.id);
      });
      await subscribe(ipc.onHistoryChanged, (payload) => {
        if (this.visible && payload.session === this.session) void refreshHistory();
      });
      await subscribe(ipc.onPanelMode, async (payload) => {
        if (!this.visible || payload.session !== this.session || !this.inputSurface) return;
        const activated = this.interaction === "activated";
        if (this.pendingDom?.id !== null) this.clearDomChoice();
        this.clearSearchTimer();
        this.searchStarting = false;
        this.searchStartFailed = false;
        this.mode = payload.mode;
        this.previewOpen = false;
        await tick();
        if (disposed || !this.visible || payload.session !== this.session || !this.inputSurface || this.mode !== "search") return;
        if (!activated) this.input?.focus({ preventScroll: true });
        else if (!this.inputHasFocus()) { this.domFocus = "failed"; return; }
        // 输入法要等 Blink 把“这是搜索框”报给浏览器进程、TSF 切到可编辑文档后才接管按键；没有用户手势时
        // 这份报告要等下一帧。等两帧再让后端补发首键，否则首字母可能绕过输入法直接上屏。
        await new Promise<void>((resolve) => frame(resolve));
        await new Promise<void>((resolve) => frame(resolve));
        if (!disposed && this.visible && payload.session === this.session && this.interactive && this.mode === "search" && document.activeElement === this.input && (!activated || document.hasFocus())) {
          this.firstInput = { session: payload.session, readyAt: performance.now() };
          const session = payload.session;
          await this.command(() => ipc.searchReady(session), session);
        }
      });
      await subscribe(ipc.onPanelHidden, (payload) => {
        if (payload.session !== this.session) return;
        this.searchRequest += 1;
        this.visible = false;
        this.previewOpen = false;
        this.setModifiers(false, false);
        this.clearUndo(); this.clearHold();
        this.imeActive = false;
        this.clearFeedback();
        this.mode = "quick";
        this.composing = false;
        this.firstInput = null;
        this.foregroundSnapshot = null;
        this.clearDomChoice(true);
        this.cancelDomFocus();
        this.domFocus = "idle";
        this.clearSearchTimer();
        this.searchStarting = false;
        this.searchStartFailed = false;
        // 实验开着时焦点留在搜索框，下次呼出不用再聚焦。
        if (!this.searchBoxFocus) this.input?.blur();
      });
      await subscribe(ipc.onPanelTiming, (payload) => { if (payload.session === this.session) this.timing = payload; });
      await subscribe(ipc.onPanelError, (payload) => {
        if (this.visible && payload.session === this.session && this.interaction !== "passive"
          && (this.interaction !== "close_only" || payload.close_only === true)) {
          if (payload.close_only) this.setInteraction("close_only");
          this.message = payload.message; this.busy = false;
          if (!payload.close_only && ["这条记录已不在历史里", "读取历史失败", "写入剪贴板失败"].some((text) => payload.message.includes(text))) {
            this.showFailure(payload.message);
          }
        }
      });
      await subscribe(ipc.onPasteResult, (payload) => {
        if (payload.session !== this.session || this.interaction === "close_only") return;
        // 粘贴成功不出声：结果就在目标程序里，用户看得到。下面只说需要用户知道的偏差。
        if (!this.visible) return;
        if (payload.paste_result === "native_submission_unknown") {
          this.clearFeedback();
          this.setInteraction("passive");
          this.message = "粘贴结果未确认，请先检查目标，避免重复粘贴";
          return;
        }
        // 被动提示已归还前台，不承诺按键送达；只展示后端确认的提示，留明确鼠标关闭入口。
        if (payload.hint_dwell_ms) {
          const administrator = payload.paste_result === "copied_only" && payload.reason === "target_may_be_elevated";
          const detail = administrator
            ? "目标程序以管理员身份运行。请切到它按 Ctrl+V。"
            : payload.hint_message || "粘贴前前台窗口变了，为免粘错，只复制到剪贴板。请按 Ctrl+V。";
          this.clearFeedback();
          this.setInteraction("passive");
          this.feedback = {
            kind: "copied", title: "已复制，未粘贴", detail,
            duration: payload.hint_dwell_ms, startedAt: Date.now(),
          };
          this.message = payload.hint_message || COPIED_HINT;
          return;
        }
        if (this.interaction === "passive") return;
        this.busy = false;
        const notes: string[] = [];
        if (payload.paste_result === "copied_only") notes.push(payload.hint_message || COPIED_HINT);
        if (payload.paste_result === "sent" && payload.layer2?.timed_out) notes.push("已发出粘贴，目标可能没收到");
        if (payload.as_text?.error) notes.push("转文字失败，已按原样写入");
        this.message = notes.join(" · ");
      });
      if (!disposed) await settingsReady;
      if (!disposed) await this.prewarm();
      if (!disposed) await this.command(() => ipc.frontendReady());
    };
    void initialize().catch(() => { this.message = "初始化失败，请重启样机并查看日志。"; });
    return () => {
      disposed = true; this.searchRequest += 1; this.foregroundSnapshot = null;
      this.clearDomChoice(true); this.cancelDomFocus(); this.clearSearchTimer();
      this.clearFeedback(); this.setModifiers(false, false); this.clearUndo(); this.clearHold();
      unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame);
    };
  };
}
