// 面板状态和交互逻辑（Svelte 5 runes）。组件只负责显示和把 DOM 事件转到这里；和后端的往来都经过 ipc.ts。
// 这里的时序细节都是早期验证里踩过的坑，改之前先读 src/lib/README.md 的“不能动的细节”。
import { tick, untrack } from "svelte";
import * as ipc from "./ipc";
import type { HistoryItem, HistorySnapshot, HitRegion, Outline, PanelInteraction, PasteMode, QueueState, SearchSelection, Timing, UnlistenFn } from "./ipc";
import { applyPreferences, DEFAULT_PREFERENCES, normalizePreferences, type Preferences } from "./preferences";
import { cardKind, FAN, outlineFor, pageStep, spreadLayout, timeGroup, type SpreadLayout } from "./cards";

const HOLD_HINT_MS = 300;
/** 后端 start_queue 一次最多接受的条数。 */
export const STACK_LIMIT = 32;
const NOTICE_MS = 2600;
// 页脚撤销提示停留的时长。删除的条目在回收站里保留 7 天，提示消失后 Ctrl+Z 照样能撤销。
const UNDO_MS = 5000;
// 删除常用要在这段时间内再按一次 Delete 确认（B13）。
const HOLD_MS = 600;
const HINT_MS = 1600;
// 删掉后多留一会儿铺满的进度，让按住删除的确认状态在下一帧再清除。
const HOLD_CLEAR_MS = 100;

// 页脚删除标签里的条目摘要：最多 16 个字。
function deleteLabel(item: HistoryItem) {
  const text = [...(item.preview || item.file_names[0] || "").replace(/\s+/g, " ").trim()];
  return text.length > 16 ? `${text.slice(0, 16).join("")}…` : text.join("") || "1 条记录";
}
// 鼠标点条目的行为（M2-8）；“none”只选中该条。本地实现，原因同 applySettings：单文件 Node 回归不经打包器。
type MouseAction = PasteMode | "none";
type MouseActions = { click: MouseAction; middle: MouseAction };
const DEFAULT_MOUSE_ACTIONS: MouseActions = { click: "paste", middle: "text" };
const isMouseAction = (value: unknown): value is MouseAction => value === "paste" || value === "copy" || value === "text" || value === "none";
function normalizeMouseActions(interaction: import("./ipc").AppSettings["interaction"]): MouseActions {
  return {
    click: isMouseAction(interaction?.click) ? interaction.click : DEFAULT_MOUSE_ACTIONS.click,
    middle: isMouseAction(interaction?.middle_click) ? interaction.middle_click : DEFAULT_MOUSE_ACTIONS.middle,
  };
}


export type PasteFeedback = {
  kind: "copied" | "failed"; title: string; detail: string; duration: number; startedAt: number;
};

// 缺少后端确认时不猜权限或原输入控件是否恢复。
export const COPIED_HINT = "已复制，请先点击目标输入位置，再按 Ctrl+V";

type DomChoice = {
  session: number; panelMode: "quick" | "search"; id: number | null; mode: PasteMode;
  queueIds?: number[]; automatic?: boolean;
  selection?: SearchSelection; code: string | null; downAt: number; released: boolean;
};
type QueueSubmission = { session: number; ids: number[]; automatic: boolean };

export const formatTime = (millis: number) => new Date(millis).toLocaleTimeString("zh-CN", { hour12: false });

/** 两种姿态：fan 扇面（最近 7 张）；spread 摊开（同一批牌放平成一列）。搜索时总是摊开。 */
export type Pose = "fan" | "spread";

/** 逐张放正常结束（放完、取消、Esc）的停止原因；其余算异常停止。 */
const QUEUE_NORMAL_STOPS = new Set(["done", "cancelled", "escape"]);
const QUEUE_ERROR_DWELL_MS = 3200;

export class PanelState {
  history = $state<HistorySnapshot>({ items: [], total: 0, recording: false, last_recorded_ms: null, store_status: null });
  favorites = $state<HistoryItem[]>([]);
  favoritePending = $state<number | null>(null);
  // 刚删除的那一条，页脚显示撤销提示用；撤销本身由后端按删除顺序恢复，跨呼出有效。
  undo = $state<{ id: number; session: number; label: string; startedAt: number; duration: number } | null>(null);
  // 删除常用要按住 Delete：holding 时条目上走进度，提前松开转成 hint 提示一会儿。
  holdDelete = $state<{ id: number; session: number; phase: "holding" | "hint"; keys: string; startedAt: number; duration: number; leaving?: boolean } | null>(null);
  private holdTimer: ReturnType<typeof setTimeout> | null = null;
  private deleting = false;
  private undoTimer: ReturnType<typeof setTimeout> | null = null;
  preferences = $state<Preferences>({ ...DEFAULT_PREFERENCES });
  motion = $derived(this.preferences.motion);
  // M2-8：鼠标单击 / 中键点击条目的行为，保存设置后即时更新；键盘不受影响。
  mouseActions = $state<MouseActions>({ ...DEFAULT_MOUSE_ACTIONS });
  ctrlDown = $state(false);
  // 按住修饰键超过 HOLD_HINT_MS 才在页脚换成对应提示；快速组合键（Ctrl+D、Ctrl+Enter）不闪。
  heldModifier = $state<"" | "ctrl" | "shift">("");
  // 完整预览：开关时通知钩子（预览开着时 Z、Ctrl+C 交给面板）；每次打开图片都从“适应”开始。
  #previewOpen = $state(false);
  get previewOpen() { return this.#previewOpen; }
  set previewOpen(open: boolean) {
    if (open === this.#previewOpen) return;
    this.#previewOpen = open;
    if (open) this.previewZoom = "fit";
    void ipc.setPreviewKeys(open).catch(() => {});
  }
  previewZoom = $state<"fit" | "original">("fit");
  // 完整预览里全文的字数（由预览组件取到全文后填上）。
  fullChars = $state<number | null>(null);
  imeActive = $state(false);
  feedback = $state<PasteFeedback | null>(null);
  query = $state("");
  mode = $state<"quick" | "search">("quick");
  handList = $state<"recent" | "favorites">("recent");
  stack = $state<HistoryItem[]>([]);
  // 逐张放异常停止时没放出去的条目 id：下次呼出时放回已选并提示。
  private pendingRestore: number[] | null = null;
  private queueErrorTimer: ReturnType<typeof setTimeout> | null = null;
  // Esc 清空已选后留 5 秒，Ctrl+Z 可以恢复。
  private stackUndo: { items: HistoryItem[]; session: number; timer: ReturnType<typeof setTimeout> } | null = null;
  #pose = $state<Pose>("fan");
  /** 搜索词非空时才算在搜索；搜索模式下清空搜索词就回到扇面的两手牌。 */
  searching = $derived(this.mode === "search" && this.query !== "");
  pose = $derived<Pose>(this.searching ? "spread" : this.#pose);
  // 本次呼出的“现在”：时间分组和相对时间在一次呼出里保持稳定。
  openedAt = $state(Date.now());
  // 拖动一张牌期间为 true：Panel 把命中区域临时扩到整个窗口，牌不会被窗口区域裁掉。
  dragging = $state(false);
  // 只有真实 pointermove 之后悬停才改选中；键盘换牌后清零，避免静止的光标把选中抢回去。
  private pointerArmed = false;
  // 摊开的滚动位置只由选中项推动（逐条、翻屏），不是独立滚动条；见 spread。
  private spreadScroll = 0;
  queue = $state<QueueState | null>(null);
  // 等后端用 queue:changed 确认本次提交已入队，再清理仍与提交快照相同的 stack。
  private queueSubmission: QueueSubmission | null = null;
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
  locked = $derived(!this.inputSurface || (this.interaction === "quick" && this.mode === "quick")
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
    : !this.interactive ? [] : this.searching ? this.searchItems : this.history.items);
  // 快速模式的两手都在 panel:shown 时冻结 ID 顺序。新复制和复用置顶只更新条目内容，
  // 不把新条目插进本次手牌；删除或文件失效会通过当前 items 映射自然移除，其余牌不挪位置。
  private recentIds = $state<number[] | null>(null);
  private pinnedIds = $state<number[] | null>(null);
  // 常用顺序以条目上的 favorite_position 为准；同位次时按 get_favorites() 的先后。
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
    const append = (id: number, frozen: boolean) => {
      const item = byId.get(id);
      // 搜索不走这里；快速模式冻结后，完全失效的文件不再占当前手牌位置。
      if (!item || included.has(id) || (frozen && item.file_state?.state === "missing")) return;
      ordered.push(item); included.add(id);
    };
    const frozen = this.recentIds !== null && this.pinnedIds !== null;
    for (const id of this.pinnedIds ?? this.livePins(items)) append(id, frozen);
    for (const id of this.recentIds ?? items.map((item) => item.id)) append(id, frozen);
    return { items: ordered };
  }
  private liveOrderItems(items: HistoryItem[]) {
    const byId = new Map(items.map((item) => [item.id, item]));
    const pinned = this.livePins(items);
    const included = new Set(pinned);
    return { items: [
      ...pinned.flatMap((id) => { const item = byId.get(id); return item ? [item] : []; }),
      ...items.filter((item) => !included.has(item.id)),
    ] };
  }
  private ordered = $derived.by(() => this.searching
    ? this.liveOrderItems(this.resultItems)
    : this.orderItems(this.resultItems));
  filtered = $derived(this.ordered.items);
  // 被动提示只读展示旧历史，但可操作序列仍为空，继续封锁搜索、数字和鼠标取用。
  private visibleOrder = $derived.by(() => this.interaction === "passive" ? this.orderItems(this.history.items) : this.ordered);
  private recentSet = $derived(this.recentIds ? new Set(this.recentIds) : null);
  private pinnedSet = $derived(this.pinnedIds ? new Set(this.pinnedIds) : null);
  recentItems = $derived(this.filtered.filter((item) => this.recentSet ? this.recentSet.has(item.id) : !item.favorite));
  favoriteItems = $derived(this.filtered.filter((item) => this.pinnedSet ? this.pinnedSet.has(item.id) : item.favorite));
  // selected 始终相对当前来源：搜索时是实时结果，否则是冻结的最近 / 常用。
  handSource = $derived(this.interaction === "passive" ? []
    : this.searching ? this.filtered : this.handList === "favorites" ? this.favoriteItems : this.recentItems);
  visibleItems = $derived(this.interaction === "passive" ? this.visibleOrder.items : this.handSource);
  /** 扇面上的牌（最多 7 张）。 */
  fanItems = $derived(this.handSource.slice(0, FAN));
  backItems = $derived(this.pose === "spread" ? [] : (this.handList === "favorites" ? this.recentItems : this.favoriteItems).slice(0, FAN));
  currentItem = $derived(this.visibleItems[this.selected] ?? null);
  kinds = $derived(this.handSource.map((item) => cardKind(item)));
  /** 本次呼出期间新复制的条数：不插进冻结的手牌，只给轻提示，下次呼出再重排。 */
  arrivals = $derived.by(() => {
    if (!this.recentIds || !this.pinnedIds) return 0;
    const known = new Set([...this.recentIds, ...this.pinnedIds]);
    return this.history.items.filter((item) => !known.has(item.id)).length;
  });
  groupOf = (item: HistoryItem): string => this.searching && item.favorite ? "常用" : timeGroup(item.copied_ms, this.openedAt);
  /**
   * 摊开的版面（行位置、分组标题、滚动、看得见的行）。滚动只由选中项推动，
   * 所以这里顺手记下新的滚动量；同样的输入重复计算结果不变。
   */
  spread = $derived.by((): SpreadLayout | null => {
    if (this.pose !== "spread") return null;
    const groups = this.searching || this.handList === "recent" ? this.groupOf : null;
    const layout = spreadLayout(this.handSource, this.selected, this.spreadScroll, groups, this.kinds);
    this.spreadScroll = layout.scroll;
    return layout;
  });
  /**
   * 上报给后端的避让外形（见 cards.ts 的 fanOutline / spreadOutline）：同一姿态固定不变。
   * 拖牌期间是 undefined（不带 avoid，后端不因拖动挪窗口）；没有手牌画面（激活中、终止提示、被动提示）时为 null。
   */
  avoidOutline = $derived.by((): Outline | null | undefined => {
    if (this.dragging) return undefined;
    if (!this.visible || !this.interactive) return null;
    return outlineFor(this.pose);
  });
  /** 数字 n 对应的条目：扇面是第 n 张；摊开是当前看得见的第 n 条（键帽随滚动重排）。 */
  digitTarget(digit: number): number | undefined {
    if (!Number.isInteger(digit) || digit < 1 || digit > FAN) return undefined;
    if (this.pose === "spread") return this.spread?.visible[digit - 1];
    return digit - 1 < this.fanItems.length ? digit - 1 : undefined;
  }
  private sourceIndex(id: number | undefined) { return id === undefined ? -1 : this.visibleItems.findIndex((item) => item.id === id); }

  // 以下故意不是响应式状态：搜索副作用不能因为它们变化而重跑。
  composing = false;
  compositionEnded = -Infinity;
  firstInput: { session: number; readyAt: number } | null = null;
  // 由 SearchBox 用 bind:this 填进来。
  input!: HTMLInputElement;
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
      // 搜索词删空不发请求：回到扇面的两手牌，并让在途回复作废。
      if (this.visible && this.interactive && this.mode === "search" && !this.composing) untrack(() => {
        void revision;
        if (value) void this.search(value);
        else { this.searchRequest += 1; this.previousQuery = ""; }
      });
    });

    $effect(() => {
      const index = this.selected;
      const count = this.visibleItems.length;
      if (count && index >= count) this.selected = count - 1;
    });

    // 搜索词删空（Backspace 删到底）= 收回扇面，和 Esc 一致。
    let wasSearching = false;
    $effect(() => {
      const searching = this.searching;
      untrack(() => { if (wasSearching && !searching) this.leaveSearchResults(); wasSearching = searching; });
    });
  }

  private leaveSearchResults() {
    this.#pose = "fan";
    this.spreadScroll = 0;
    this.handList = "recent";
    this.selected = 0;
  }

  applySettings = (settings: import("./ipc").AppSettings) => {
    this.preferences = normalizePreferences(settings);
    this.mouseActions = normalizeMouseActions(settings.interaction);
    applyPreferences(this.preferences);
  };

  refreshFavorites = async () => {
    const session = this.session;
    try {
      const favorites = await ipc.getFavorites();
      if (!favorites) return;
      // 按回来那一刻的选中项追踪：请求期间用户可能已经移动过，不能拉回请求前的位置。
      const selectedId = this.currentItem?.id;
      this.favorites = favorites;
      if (this.visible && session === this.session && selectedId !== undefined) {
        const index = this.sourceIndex(selectedId);
        this.selected = Math.max(0, index);
      }
    } catch {
      // 读不到就按条目上的常用标记排，结果仍可用，不打扰用户。
    }
  };

  refreshRecordingStatus = async () => {
    const session = this.session;
    const selectedId = this.currentItem?.id;
    try {
      const snapshot = await ipc.getHistory();
      if (!this.visible || session !== this.session) return;
      this.history = snapshot;
      const index = this.sourceIndex(selectedId);
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
    if (this.pinnedIds === null || this.recentIds === null) {
      this.pinnedIds = this.livePins(this.history.items);
      const pinned = new Set(this.pinnedIds);
      this.recentIds = this.history.items.filter((entry) => !pinned.has(entry.id)).map((entry) => entry.id);
    }
    this.favoritePending = id;
    try {
      const updated = await ipc.setFavorite(id, !item.favorite);
      if (session !== this.session) return;
      if (!updated) { this.message = "这条记录已不在历史里。"; return; }
      // 只补丁更新返回项；常用区已冻结，这条只换星标、不换位置。
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
      const index = this.sourceIndex(selectedId);
      this.selected = Math.max(0, index);
    } catch {
      if (session === this.session) this.message = "常用状态未保存，请重试。";
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
   * 常用的要按住不放，条目上的进度走满才删，中途松开就取消。按键重复不算新的按下。
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
      const removed = await ipc.deleteItem(item.id);
      if (session !== this.session || !this.visible) return;
      if (!removed) { this.message = "这条记录已经不在了。"; return; }
      this.message = "";
      this.previewOpen = false;
      this.history = { ...this.history, items: this.history.items.filter((entry) => entry.id !== item.id), total: Math.max(0, this.history.total - 1) };
      this.searchItems = this.searchItems.filter((entry) => entry.id !== item.id);
      this.favorites = this.favorites.filter((entry) => entry.id !== item.id);
      // 删掉的牌如果在已选里，同步移除，免得一起粘贴时带上已进回收站的条目。
      if (this.isTaken(item.id)) this.stack = this.stack.filter((entry) => entry.id !== item.id);
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
      const restored = await ipc.undoDelete();
      if (session !== this.session || !this.visible) return;
      if (!restored) { this.message = "没有可以撤销的删除。"; return; }
      const snapshot = await ipc.getHistory();
      if (session !== this.session || !this.visible) return;
      this.history = snapshot;
      if (restored.favorite) {
        const favorites = await ipc.getFavorites();
        if (favorites && session === this.session) this.favorites = favorites;
      }
      if (this.mode === "search") this.searchRevision += 1;
      this.message = "";
      const index = this.sourceIndex(restored.id);
      if (index >= 0) this.selected = index;
    } catch {
      if (session === this.session && this.visible) this.message = "撤销失败，请重试。";
    }
  };

  /** 完整预览里 Ctrl+C：只复制选中的文字（面板不关，页脚提示）。 */
  copySelection = async () => {
    const item = this.currentItem;
    const text = window.getSelection?.()?.toString() ?? "";
    if (!item || !this.previewOpen) return;
    if (!text) { this.message = "先用鼠标选中要复制的文字。"; return; }
    try {
      await ipc.copySelection(item.id, text);
      this.message = "";
      this.showNotice(`已复制选中的 ${[...text].length} 字`);
    } catch {
      this.message = "复制没有完成，请重试。";
    }
  };
  notice = $state("");
  private noticeTimer: ReturnType<typeof setTimeout> | null = null;
  /** 轻提示：替换页脚位置的说明，几秒后消失；不是错误。 */
  showNotice(text: string, duration = NOTICE_MS) {
    this.notice = text;
    if (this.noticeTimer) clearTimeout(this.noticeTimer);
    this.noticeTimer = setTimeout(() => { this.notice = ""; this.noticeTimer = null; }, duration);
  }
  private clearNotice() {
    this.notice = "";
    if (this.noticeTimer) clearTimeout(this.noticeTimer);
    this.noticeTimer = null;
  }

  togglePreview = () => {
    if (!this.visible || !this.interactive || this.busy || this.awaitingRelease) return;
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
      const selectedId = this.currentItem?.id;
      this.searchItems = reply.items;
      this.searchedQuery = value;
      const index = reset ? 0 : this.sourceIndex(selectedId);
      this.selected = Math.max(0, index);
      if (reset) this.spreadScroll = 0;
    } catch {
      if (requestId === this.searchRequest && this.visible && currentSession === this.session) this.message = "搜索没有完成，请再试一次。";
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
  private async command(call: () => Promise<unknown>, session = this.session, showReason = false): Promise<boolean> {
    try { await call(); return true; }
    catch (error) {
      if (this.session !== session || !this.visible || this.interaction === "passive" || this.interaction === "close_only") return false;
      // start_queue 会直接返回给用户看的中文原因（空、超过 32 张、重复、复制模式），原样显示。
      this.message = showReason && typeof error === "string" && error.trim() ? error : "操作没有完成，请再试一次；仍不行就关闭面板后重新呼出。";
      this.busy = false;
      return false;
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
    if (["请松开按键以完成取用。", "请松开按键以收起面板。", "请松开按键以开始逐张放。"].includes(this.message)) this.message = "";
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
    this.message = choice.id === null ? "请松开按键以收起面板。" : choice.queueIds ? "请松开按键以开始逐张放。" : "请松开按键以完成取用。";
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
    if (choice.queueIds) {
      // 已选可以来自上一手或搜索前的列表，不要求出现在当前结果里；条目是否还在由后端核对。
      this.busy = true;
      void this.submitQueue(choice.session, choice.queueIds, choice.automatic === true,
        () => ipc.startQueue(choice.session, choice.queueIds!, choice.automatic === true, choice.mode, choice.selection));
      return;
    }
    const index = this.sourceIndex(choice.id);
    if (index < 0) { this.message = "这条记录刚刚变了，请重新选择。"; return; }
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

  private captureSelection(event?: KeyboardEvent | MouseEvent, forQueue = false): SearchSelection | null {
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
      if (forQueue) {
        // 一叠的普通 Enter / Ctrl+Enter / Ctrl+Shift+Enter 都是有效提交；Shift+Enter 始终只复制单张。
        if (!(key.key === "Enter" && (!key.shiftKey || key.ctrlKey))) return null;
      } else if (!(key.key === "Enter" && !(key.ctrlKey && key.shiftKey)) && !(key.ctrlKey && /^[1-7]$/.test(key.key))
        // 搜索框空着（回到扇面）时数字不再是输入，直接取用对应的牌。
        && !(!this.searching && !key.ctrlKey && !key.shiftKey && /^[1-7]$/.test(key.key))) return null;
    } else if (event.type === "click" || event.type === "auxclick" || event.type === "pointerup") {
      const pointer = event as MouseEvent;
      if (pointer.button !== (event.type === "auxclick" ? 1 : 0) || (event.type !== "pointerup" && pointer.detail <= 0)) return null;
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
      this.composing = false;
      this.firstInput = null;
      if (interaction !== "activated") this.input?.blur();
    }
    if (interaction === "activated") this.prepareDomFocus();
    if (enteringPassive) {
      const index = this.sourceIndex(selectedId);
      this.selected = Math.max(0, index);
      this.busy = true; this.message = "正在确认复制结果…";
    }
  }

  /** ← → 和摊开里的 ↑ ↓：上一张 / 下一张。扇面在第 7 张再往后 = 摊开到第 8 条。 */
  move = (offset: number) => {
    if (this.holdDelete) this.clearHold();
    const count = this.handSource.length;
    if (!count) return;
    if (this.pose === "fan") {
      const limit = Math.min(FAN, count);
      const next = this.selected + offset;
      if (next >= limit) {
        if (offset > 0 && count > FAN && !this.previewOpen) this.spreadOpen(FAN);
        else this.selected = limit - 1;
        return;
      }
      this.selected = Math.max(0, next);
      return;
    }
    this.selected = Math.max(0, Math.min(count - 1, this.selected + offset));
  };

  /** 扇面 → 摊开；index 是摊开后选中的条目。越界（没有那么多条）时不动。 */
  spreadOpen = (index = this.selected) => {
    if (this.previewOpen || this.pose === "spread") return;
    const count = this.handSource.length;
    if (!count || index < 0 || index >= count) return;
    if (this.holdDelete) this.clearHold();
    this.spreadScroll = 0;
    this.#pose = "spread";
    this.selected = index;
  };

  /** 摊开 → 扇面：原来选中的在前 7 张里就保持选中，否则回到第 1 张。 */
  collapse = () => {
    if (this.#pose !== "spread" || this.searching) return;
    if (this.holdDelete) this.clearHold();
    this.#pose = "fan";
    this.spreadScroll = 0;
    if (this.selected >= FAN) this.selected = 0;
  };

  switchHandList = (list: "recent" | "favorites") => {
    if (this.searching || this.pose !== "fan" || this.handList === list || this.previewOpen) return;
    if (list === "favorites" && !this.favoriteItems.length) return;
    this.clearHold();
    this.handList = list;
    this.selected = 0;
  };

  /** 摊开里翻一屏（可见条数 − 1）；已在首尾时不动。 */
  page = (direction: 1 | -1) => {
    if (this.pose !== "spread" || this.previewOpen) return;
    const count = this.handSource.length;
    if (!count) return;
    const target = Math.max(0, Math.min(count - 1, this.selected + direction * pageStep(this.spread)));
    if (target !== this.selected) { if (this.holdDelete) this.clearHold(); this.selected = target; }
  };

  /** 滚轮：扇面向下 = 摊开；摊开里逐条，第一条再向上 = 收回。 */
  wheel = (direction: 1 | -1) => {
    if (!this.visible || !this.interactive || this.busy || this.pendingDom || this.previewOpen || this.dragging) return;
    if (this.pose === "fan") { if (direction > 0) this.spreadOpen(this.selected); return; }
    if (direction < 0 && !this.searching && this.selected === 0) { this.collapse(); return; }
    this.move(direction);
  };

  /** 真实的鼠标移动之后，悬停才允许改选中。 */
  pointerMoved = () => { this.pointerArmed = true; };

  /** 扇面悬停拈起；忙、等待结果、看全打开、拖动或键盘刚换过牌时不改。摊开里悬停只加底色。 */
  hoverSelect = (index: number) => {
    if (!this.pointerArmed || !this.visible || !this.interactive || this.busy || this.pendingDom || this.awaitingRelease
      || this.previewOpen || this.dragging || this.pose !== "fan" || this.holdDelete?.phase === "holding") return;
    const item = this.handSource[index];
    if (item && index < FAN && !this.isTaken(item.id)) this.selected = index;
  };

  // 鼠标取用入口：按设置把左键 / 中键换成粘贴模式，再走与键盘相同的 choose；“不做额外操作”只选中该条。
  chooseByMouse = (index: number, event: MouseEvent) => {
    const item = this.handSource[index];
    if (!item) return;
    if (this.isTaken(item.id)) { this.takenNotice(); return; }
    const action = event.button === 1 ? this.mouseActions.middle : this.mouseActions.click;
    if (action === "none") {
      if (this.visible && this.interactive && !this.busy && this.handSource[index]) this.selected = index;
      return;
    }
    void this.choose(index, action, event);
  };

  choose = async (index = this.selected, pasteMode: PasteMode = "paste", event?: KeyboardEvent | MouseEvent) => {
    const item = this.handSource[index];
    if (!this.visible || !this.interactive || this.busy || this.pendingDom || !item) return;
    // 已选的牌只跟着已选一起粘贴，不响应数字或单击单独取用。
    if (this.isTaken(item.id)) { this.takenNotice(); return; }
    // 第一次 await 之前固定全部请求字段；绝不在 IPC/读库返回后换用更新的条目或 revision。
    const session = this.session;
    const id = item.id;
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
      if ((event.type !== "click" && event.type !== "pointerup" || (event as MouseEvent).button !== 0) && (event.type !== "auxclick" || (event as MouseEvent).button !== 1)
        || (event.type !== "pointerup" && (event as MouseEvent).detail <= 0) || this.composing
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

  isTaken = (id: number) => this.stack.some((item) => item.id === id);
  private takenNotice() { if (this.visible) this.showNotice("这张已在已选里：↵ 一起粘贴，Backspace 放回最后一张"); }

  private submitQueue(session: number, ids: number[], automatic: boolean, call: () => Promise<unknown>): Promise<void> {
    const submission: QueueSubmission = { session, ids: [...ids], automatic };
    this.queueSubmission = submission;
    return this.command(call, session, true).then((started) => {
      // IPC 失败表示没有可等待的入队确认；保留 stack，不能让后续无关事件误清。
      if (!started && this.queueSubmission === submission) this.queueSubmission = null;
    });
  }

  takeCurrent = () => {
    const item = this.currentItem;
    if (!this.visible || !this.interactive || this.busy || this.awaitingRelease || this.previewOpen || !item) return;
    if (this.isTaken(item.id)) return;
    if (this.stack.length >= STACK_LIMIT) {
      this.showNotice(`最多选 ${STACK_LIMIT} 张：先按 ↵ 一起粘贴，或 Backspace 放回`, 3600);
      return;
    }
    this.dropStackUndo();
    this.stack = [...this.stack, item];
    const limit = this.pose === "fan" ? Math.min(FAN, this.handSource.length) : this.handSource.length;
    const next = this.handSource.findIndex((entry, index) => index > this.selected && index < limit && !this.isTaken(entry.id));
    if (next >= 0) this.selected = next;
  };

  untake = () => {
    const item = this.stack.at(-1);
    if (!item) return;
    this.dropStackUndo();
    this.stack = this.stack.slice(0, -1);
    const index = this.sourceIndex(item.id);
    if (index >= 0 && (this.pose === "spread" || index < FAN)) this.selected = index;
  };

  /** Esc 清空已选：留 5 秒可用 Ctrl+Z 恢复。 */
  private clearStack() {
    const items = this.stack;
    if (!items.length) return;
    this.dropStackUndo();
    this.stack = [];
    const timer = setTimeout(() => { if (this.stackUndo?.timer === timer) this.stackUndo = null; }, 5000);
    this.stackUndo = { items, session: this.session, timer };
    this.showNotice(`已清空已选的 ${items.length} 张 · Ctrl+Z 恢复`, 5000);
  }

  private dropStackUndo() {
    if (this.stackUndo) clearTimeout(this.stackUndo.timer);
    this.stackUndo = null;
  }

  /** Ctrl+Z：先恢复刚清空的已选，没有时才撤销删除。返回是否恢复了已选。 */
  private restoreStack() {
    const undo = this.stackUndo;
    if (!undo || this.stack.length) return false;
    this.dropStackUndo();
    this.stack = undo.items.slice(0, STACK_LIMIT);
    this.clearNotice();
    return true;
  }

  /** Ctrl+Shift+Enter 只负责逐张放；Shift+Enter 永远只复制当前一张，不再按上下文换义。 */
  startStackQueue = async (automatic: boolean, pasteMode: "paste" | "text", event?: KeyboardEvent) => {
    if (!this.visible || !this.interactive || this.busy || this.pendingDom || !this.stack.length) {
      if (!this.stack.length) this.message = "先按空格多选几张，再按 Ctrl+Shift+Enter 逐张放";
      return;
    }
    const session = this.session;
    const ids = this.stack.map((item) => item.id);
    const selection = this.mode === "search" ? this.captureSelection(event, true) : undefined;
    if (selection === null) {
      this.message = this.foregroundSnapshot ? "无法确认选择与前台状态的先后，请重新选择。" : "前台状态不可用，请关闭面板后重新呼出。";
      return;
    }
    if (this.interaction === "activated") {
      if (!event || event.isTrusted !== true || event.type !== "keydown" || !this.inputHasFocus()
        || event.repeat || event.isComposing || this.composing || event.keyCode === 229
        || performance.now() - this.compositionEnded < 50 || event.timeStamp - this.compositionEnded < 50) return;
      this.observeModifiers(event);
      this.heldDomKeys.set(this.domCode(event), event.timeStamp);
      this.armDomChoice({ session, panelMode: this.mode, id: ids[0], mode: pasteMode, queueIds: ids, automatic,
        selection, code: this.domCode(event), downAt: event.timeStamp, released: false });
      return;
    }
    this.busy = true;
    this.message = "";
    await this.submitQueue(session, ids, automatic, () => ipc.startQueue(session, ids, automatic, pasteMode, selection));
  };

  /** 完整预览开着时方向键、翻页键用来滚动预览（M2-7）；返回是否已处理。 */
  private scrollPreview(key: string) {
    const keys = ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"];
    if (!this.previewOpen || !keys.includes(key)) return false;
    if (typeof document === "undefined" || typeof document.querySelector !== "function") return true;
    // 看原尺寸图片时滚动的是图片区，其余滚动整个预览。
    const box = document.querySelector<HTMLElement>(".preview-overlay .image-stage.original")
      ?? document.querySelector<HTMLElement>(".preview-overlay .overlay-scroll");
    if (!box) return true;
    const line = 56, page = Math.max(line, box.clientHeight * 0.85);
    const behavior: ScrollBehavior = this.motion === "none" ? "auto" : "smooth";
    const by = (left: number, top: number) => box.scrollBy({ left, top, behavior });
    switch (key) {
      case "ArrowUp": by(0, -line); break;
      case "ArrowDown": by(0, line); break;
      case "ArrowLeft": by(-line, 0); break;
      case "ArrowRight": by(line, 0); break;
      case "PageUp": by(0, -page); break;
      case "PageDown": by(0, page); break;
      case "Home": box.scrollTo({ top: 0, left: 0, behavior }); break;
      case "End": box.scrollTo({ top: box.scrollHeight, behavior }); break;
    }
    return true;
  }

  private clearSearchForEscape() {
    if (this.mode !== "search" || !this.query) return false;
    this.clearDomChoice();
    this.query = "";
    this.searchRevision += 1;
    this.leaveSearchResults();
    return true;
  }

  /** Esc 逐层退出：看全 → 搜索（清空并收回扇面）→ 已选（清空，可撤销）→ 摊开（收回扇面）。返回是否消耗了这次 Esc。 */
  private escapeLayer() {
    if (this.previewOpen) { this.previewOpen = false; return true; }
    if (this.clearSearchForEscape()) return true;
    if (this.stack.length) { this.clearStack(); return true; }
    if (this.pose === "spread") { this.collapse(); return true; }
    return false;
  }

  navigate = (key: string, event?: KeyboardEvent) => {
    if (!this.visible || !this.interactive) return;
    // 键盘换牌后，静止的鼠标光标不能把选中抢回去。
    this.pointerArmed = false;
    if (key === "Tab") { this.togglePreview(); return; }
    if (this.previewOpen && (key === "ArrowLeft" || key === "ArrowRight")) { this.move(key === "ArrowLeft" ? -1 : 1); return; }
    if (this.scrollPreview(key)) return;
    const spread = this.pose === "spread";
    switch (key) {
      case "Space": this.takeCurrent(); break;
      case "ArrowLeft": this.move(-1); break;
      case "ArrowRight": this.move(1); break;
      case "ArrowUp":
        if (spread) { if (!this.searching && this.selected === 0) this.collapse(); else this.move(-1); }
        else this.switchHandList(this.handList === "favorites" ? "recent" : "favorites");
        break;
      case "ArrowDown":
        // ↓ 在常用扇面里只做“回到最近”，在最近扇面里才摊开。
        if (spread) this.move(1);
        else if (this.handList === "favorites") this.switchHandList("recent");
        else this.spreadOpen(this.selected);
        break;
      case "PageUp": if (spread) this.page(-1); break;
      case "PageDown": if (spread) this.page(1); else this.spreadOpen(FAN); break;
      case "Home":
        if (!spread) this.selected = 0;
        else if (this.searching) this.selected = 0;
        else this.collapse();
        break;
      case "End":
        this.selected = Math.max(0, (spread ? this.handSource.length : Math.min(FAN, this.handSource.length)) - 1);
        break;
      case "Enter": if (this.stack.length) void this.startStackQueue(true, "paste", event); else void this.choose(this.selected, "paste", event); break;
      case "Shift+Enter":
        // 有已选时不执行：不能悄悄丢掉已选的牌。
        if (this.stack.length) this.showNotice(`已选 ${this.stack.length} 张，Shift+↵ 不会丢掉它们：↵ 一起粘贴，Ctrl+Shift+↵ 逐张放`, 3600);
        else void this.choose(this.selected, "copy", event);
        break;
      case "Ctrl+Shift+Enter": void this.startStackQueue(false, "paste", event); break;
      case "Ctrl+Enter": if (this.stack.length) void this.startStackQueue(true, "text", event); else void this.choose(this.selected, "text", event); break;
      case "Backspace": if (!this.searching && this.stack.length) this.untake(); break;
      case "Ctrl+D": if (this.currentItem) void this.toggleFavorite(this.currentItem.id); break;
      // 钩子在按下时送 Delete:down、松开时送 Delete。
      case "Delete:down": if (!this.searching) this.pressDelete(); break;
      case "Delete": if (!this.searching) this.releaseDelete(); break;
      case "Ctrl+Z": if (!this.restoreStack()) void this.undoDelete(); break;
      case "Z": if (this.previewOpen) this.previewZoom = this.previewZoom === "fit" ? "original" : "fit"; break;
      case "Ctrl+C": if (this.previewOpen) void this.copySelection(); break;
      case "Escape":
        if (this.escapeLayer()) break;
        if (this.interaction === "activated" && event) this.armDomChoice({ session: this.session, panelMode: this.mode,
          id: null, mode: "copy", code: this.domCode(event), downAt: event.timeStamp, released: false });
        else this.close();
        break;
      default:
        if (/^[1-7]$/.test(key)) {
          const index = this.digitTarget(Number(key));
          if (index !== undefined) void this.choose(index, "paste", event);
        }
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
          if (this.escapeLayer()) { this.clearDomChoice(); return; }
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
        const horizontal = event.key === "ArrowLeft" || event.key === "ArrowRight";
        const ctrlOnly = event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
        const ctrlShiftOnly = event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey;
        const action = ctrlShiftOnly && event.key === "Enter"
          ? "Ctrl+Shift+Enter"
          : ctrlOnly && event.key.toLowerCase() === "d"
          ? "Ctrl+D"
          : ctrlOnly && event.key.toLowerCase() === "z"
          ? "Ctrl+Z"
          : this.previewOpen && ctrlOnly && event.key.toLowerCase() === "c"
          ? "Ctrl+C"
          : this.previewOpen && unmodified && event.key.toLowerCase() === "z"
          ? "Z"
          : unmodified && event.key === "Delete"
          ? "Delete:down"
          : event.key === "Enter" && !event.altKey && !event.metaKey && !(event.ctrlKey && event.shiftKey)
          ? event.ctrlKey ? "Ctrl+Enter" : event.shiftKey ? "Shift+Enter" : "Enter"
          : unmodified && (horizontal || /^[1-7]$/.test(event.key) || event.key === " " || ["Tab", "Backspace", "Escape", "ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"].includes(event.key)) ? event.key === " " ? "Space" : event.key : null;
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
    const settled = performance.now() - this.compositionEnded >= 50;
    const ctrlOnly = event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
    const unmodified = !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
    const navigation = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"];
    // 看全开着：Z 切换原图、Ctrl+C 复制选中文字，方向和翻页键用于看全，不进输入框。
    if (this.previewOpen) {
      if (unmodified && event.key.toLowerCase() === "z") { event.preventDefault(); if (!event.repeat) this.navigate("Z", event); return; }
      if (ctrlOnly && event.key.toLowerCase() === "c") { event.preventDefault(); if (!event.repeat) this.navigate("Ctrl+C", event); return; }
    }
    // 提交动作忽略长按重复；保留原 50ms 组合上屏保护和方向键长按导航。
    if (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.key === "Enter") {
      event.preventDefault();
      if (!event.repeat && settled) this.navigate("Ctrl+Shift+Enter", event);
      return;
    }
    // Ctrl+数字取用：只认 1–7，对应键帽上标的那一张（看得见的第 n 条）；Ctrl+8/9 只拦截。
    if (event.ctrlKey && !event.altKey && !event.metaKey && /^[0-9]$/.test(event.key)) {
      event.preventDefault();
      if (!event.repeat && /^[1-7]$/.test(event.key) && settled) {
        const index = this.digitTarget(Number(event.key));
        if (index !== undefined) void this.choose(index, event.shiftKey ? "copy" : "paste", event);
      }
      return;
    }
    if (ctrlOnly && event.key === "Enter") {
      event.preventDefault();
      if (!event.repeat && settled) this.navigate("Ctrl+Enter", event);
      return;
    }
    // 搜索模式删除当前结果：Delete 留给搜索框删字，这里用 Ctrl+Delete。
    if (ctrlOnly && event.key === "Delete") {
      event.preventDefault();
      if (!event.repeat && settled) this.pressDelete();
      return;
    }
    if (ctrlOnly && event.key.toLowerCase() === "z" && this.stackUndo && !this.stack.length) {
      event.preventDefault();
      if (!event.repeat) this.restoreStack();
      return;
    }
    if ((event.ctrlKey && ["f", "p", "r", "+", "-", "=", "0"].includes(event.key.toLowerCase())) || event.key === "F5" || event.key === "F7") {
      event.preventDefault();
      if (event.key.toLowerCase() === "f") this.input?.focus({ preventScroll: true });
      return;
    }
    // Tab 是看全，不移动焦点。
    if (event.key === "Tab" && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault();
      if (!event.repeat && settled) this.navigate("Tab", event);
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey || (event.target !== this.input && event.key !== "Escape")) return;
    // ← → 也是上一张 / 下一张，不移动输入光标；Shift+方向同样拦住，免得选中搜索框里的字。
    if (navigation.includes(event.key)) {
      event.preventDefault();
      if (settled) this.navigate(event.key, event);
      return;
    }
    if (event.key === "Escape" || event.key === "Enter") {
      event.preventDefault();
      if (!settled || (event.key === "Enter" && event.repeat)) return;
      this.navigate(event.key === "Enter" && event.shiftKey ? "Shift+Enter" : event.key, event);
      return;
    }
    // 搜索框空着时回到了扇面：空格多选、Backspace 放回、数字直接取用、Delete 删除，都不进输入框。
    if (!this.searching && unmodified) {
      if (event.key === " ") { event.preventDefault(); if (!event.repeat) this.navigate("Space", event); return; }
      if (event.key === "Backspace") { event.preventDefault(); if (!event.repeat) this.navigate("Backspace", event); return; }
      if (event.key === "Delete") { event.preventDefault(); if (!event.repeat) this.pressDelete(); return; }
      if (/^[1-7]$/.test(event.key)) { event.preventDefault(); if (!event.repeat && settled) this.navigate(event.key, event); return; }
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

  cancelQueue = () => {
    if (!this.queue) return;
    void this.command(() => ipc.cancelQueue(this.session), this.session);
  };

  reportHitRegions = (regions: HitRegion[], scale: number, armed: boolean, avoid?: Outline | null) => {
    void ipc.setHitRegions(regions, scale, armed, this.session, avoid).catch(() => {});
  };

  /** 面板里的回收站入口：收起面板，打开设置里的回收站（列表和管理都在设置里）。 */
  openTrash = () => {
    if (!this.visible) return;
    const session = this.session;
    void this.command(() => ipc.hidePanel(session), session);
    void ipc.openSettings("trash").catch(() => {});
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
      const snapshot = await ipc.getHistory();
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

  /**
   * 呼出时：上次逐张放异常停止剩下的条目按原顺序放回已选（过滤已不存在的、已在已选里的，最多 32 张）并轻提示；
   * 否则已选跨呼出保留时提示还有几张。
   */
  private restoreQueueLeftovers(items: HistoryItem[]) {
    const ids = this.pendingRestore;
    this.pendingRestore = null;
    if (ids?.length) {
      const byId = new Map(items.map((item) => [item.id, item]));
      const taken = new Set(this.stack.map((item) => item.id));
      const back = ids.flatMap((id) => { const item = byId.get(id); if (!item || taken.has(id)) return []; taken.add(id); return [item]; });
      if (back.length) {
        this.stack = [...back, ...this.stack].slice(0, STACK_LIMIT);
        this.showNotice(`上次还剩 ${back.length} 张没粘贴，已放回已选`, 5000);
        return;
      }
    }
    if (this.stack.length) this.showNotice(`还有 ${this.stack.length} 张已选：↵ 一起粘贴，Esc 清空`, 4000);
  }

  /** onMount 时调用：读版本号、订阅后端事件，全部订阅好后再发 frontend_ready；返回卸载时的清理函数。 */
  connect = () => {
    let disposed = false;
    void ipc.appVersion().then((value) => { if (!disposed) this.version = value; }).catch(() => {});
    const settingsReady = ipc.getSettings().then((settings) => { if (!disposed && settings) this.applySettings(settings); }).catch(() => {});
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
            const selectedId = this.currentItem?.id;
            // 新复制不插进本次的手牌和搜索结果（顺序冻结，只计“新来 N 张”），下次呼出或改搜索词才出现。
            this.history = snapshot;
            const index = this.sourceIndex(selectedId);
            this.selected = index >= 0 ? index : 0;
            void this.refreshFavorites();
          }
        }
      } catch { if (!disposed && this.interaction !== "close_only") this.message = "读取历史失败，请关闭面板后重新呼出。"; }
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
        if (payload.session !== this.session) this.queueSubmission = null;
        this.session = payload.session;
        this.visible = true;
        this.mode = "quick";
        this.interaction = payload.interaction;
        this.query = "";
        this.handList = "recent";
        this.#pose = "fan";
        this.spreadScroll = 0;
        this.openedAt = Date.now();
        this.pointerArmed = false;
        this.dragging = false;
        this.clearNotice();
        this.queue = null;
        this.history = payload.history;
        this.selected = 0;
        this.busy = false;
        this.message = "";
        this.clearFeedback();
        this.previewOpen = false;
        this.setModifiers(false, false);
        this.imeActive = false;
        this.favoritePending = null;
        this.pinnedIds = this.livePins(payload.history.items);
        const pinned = new Set(this.pinnedIds);
        this.recentIds = payload.history.items.filter((item) => !pinned.has(item.id)).map((item) => item.id);
        // 最近为空而常用有内容时，直接落到常用。
        if (!this.recentIds.length && this.pinnedIds.length) this.handList = "favorites";
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
        this.timing = { session: payload.session, hotkeyToShowMs: payload.hotkeyToShowMs };
        this.restoreQueueLeftovers(payload.history.items);
        const current = this.session;
        if (this.interaction === "activated") this.prepareDomFocus();
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
        // 搜索模式不导航，但常用键全局可用（钩子已吞键，必须经 panel:key 送达）。
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
        // 搜索开始时姿态回到扇面：一打字（搜索词非空）就摊开；删空或 Esc 回扇面。
        if (!this.query) { this.#pose = "fan"; this.spreadScroll = 0; }
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
        // 异常停止的小指示要停留约 3 秒显示原因；其余情况收起时清掉。
        if (payload.queue !== true && !this.queue?.stopped) this.queue = null;
        // 清掉手牌画面：下次呼出在 panel:shown 带来新历史之前不渲染任何旧牌（避免首帧闪出旧画面）。
        this.history = { ...this.history, items: [] };
        this.searchItems = [];
        this.query = "";
        this.#pose = "fan";
        this.spreadScroll = 0;
        this.selected = 0;
        this.busy = false;
        this.previewOpen = false;
        this.setModifiers(false, false);
        this.clearUndo(); this.clearHold();
        this.imeActive = false;
        this.clearFeedback();
        this.clearNotice();
        this.dragging = false;
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
        this.input?.blur();
      });
      await subscribe(ipc.onQueueChanged, (payload) => {
        if (payload.session !== this.session) return;
        const submission = this.queueSubmission;
        if (submission && submission.session === payload.session && payload.remaining > 0
          && payload.total === submission.ids.length && payload.automatic === submission.automatic) {
          // 只消费一次匹配的本会话确认；旧会话、后续进度或无关队列事件都不能清 stack。
          this.queueSubmission = null;
          if (this.stack.length === submission.ids.length
            && this.stack.every((item, index) => item.id === submission.ids[index])) this.stack = [];
        } else if (submission && submission.session === payload.session && payload.remaining <= 0) {
          // 后端若在进入队列前拒绝请求，终态事件同时结束本次等待。
          this.queueSubmission = null;
        }
        if (this.queueErrorTimer !== null) clearTimeout(this.queueErrorTimer);
        this.queueErrorTimer = null;
        const abnormal = payload.remaining <= 0 && !!payload.stopped && !QUEUE_NORMAL_STOPS.has(payload.stopped);
        if (abnormal) {
          // 异常停止：小指示显示后端给的原因，停留约 3 秒；没放出去的条目下次呼出时放回已选。
          this.queue = payload;
          // 新的 queue:changed 会先清掉这个计时器，所以到点时显示的一定还是这次的停止原因。
          this.queueErrorTimer = setTimeout(() => { this.queueErrorTimer = null; this.queue = null; }, QUEUE_ERROR_DWELL_MS);
          const left = (payload.remaining_ids ?? []).filter((id) => Number.isSafeInteger(id));
          if (left.length) this.pendingRestore = left;
        } else this.queue = payload.remaining > 0 ? payload : null;
        // 后端拒绝或中途停止时 error 是给用户看的原文（面板可见时显示）。
        if (payload.remaining <= 0 && payload.error && this.visible) this.message = payload.error;
        this.busy = false;
      });
      await subscribe(ipc.onPanelTiming, (payload) => { if (payload.session === this.session) this.timing = payload; });
      await subscribe(ipc.onPanelError, (payload) => {
        if (payload.session === this.session) this.queueSubmission = null;
        if (this.visible && payload.session === this.session && this.interaction !== "passive"
          && (this.interaction !== "close_only" || payload.close_only === true)) {
          if (payload.close_only) this.setInteraction("close_only");
          this.message = payload.message; this.busy = false;
          if (!payload.close_only && ["这条记录已不在历史里", "读取历史失败", "写入剪贴板失败", "原文件不在了"].some((text) => payload.message.includes(text))) {
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
    void initialize().catch(() => { this.message = "拈来没有准备好，请从托盘退出后重新打开。"; });
    return () => {
      disposed = true; this.searchRequest += 1; this.foregroundSnapshot = null;
      this.clearDomChoice(true); this.cancelDomFocus(); this.clearSearchTimer();
      this.clearFeedback(); this.setModifiers(false, false); this.clearUndo(); this.clearHold();
      unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame);
    };
  };
}
