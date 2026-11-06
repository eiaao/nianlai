// 面板状态和交互逻辑（Svelte 5 runes）。组件只负责显示和把 DOM 事件转到这里；和后端的往来都经过 ipc.ts。
// 这里的时序细节都是 G0 踩过的坑，改之前先读 src/lib/README.md 的“不能动的细节”。
import { tick, untrack } from "svelte";
import * as ipc from "./ipc";
import type { HistoryItem, HistorySnapshot, PanelInteraction, PasteMode, SearchSelection, Timing, UnlistenFn } from "./ipc";
import type { Preferences } from "./preferences";

const DEFAULT_PREFERENCES: Preferences = { layout: "a", theme: "system", motion: "soft", density: "compact" };

export type PasteFeedback = {
  kind: "copied" | "failed"; title: string; detail: string; duration: number; startedAt: number;
};
export type LastPasteResult = "confirmed" | "sent" | "copied";

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
  preferences = $state<Preferences>({ ...DEFAULT_PREFERENCES });
  motion = $derived(this.preferences.motion);
  ctrlDown = $state(false);
  previewOpen = $state(false);
  imeActive = $state(false);
  lastResult = $state<LastPasteResult | null>(null);
  feedback = $state<PasteFeedback | null>(null);
  query = $state("");
  mode = $state<"quick" | "search">("quick");
  interaction = $state<PanelInteraction>("quick");
  visible = $state(false);
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
  private resultItems = $derived(!this.interactive ? [] : this.mode === "search" ? this.searchItems : this.history.items);
  private orderItems(items: HistoryItem[]) {
    const byId = new Map(items.map((item) => [item.id, item]));
    const ordered: HistoryItem[] = [];
    const included = new Set<number>();
    // get_favorites() 是收藏区顺序的唯一来源；后端暂时拿不到时按项目字段降级。
    for (const favorite of this.favorites) {
      const item = byId.get(favorite.id);
      if (item?.favorite && !included.has(item.id)) { ordered.push(item); included.add(item.id); }
    }
    const missing = items.filter((item) => item.favorite && !included.has(item.id))
      .sort((a, b) => (a.favorite_position ?? Number.MAX_SAFE_INTEGER) - (b.favorite_position ?? Number.MAX_SAFE_INTEGER));
    for (const item of missing) { ordered.push(item); included.add(item.id); }
    for (const item of items) if (!item.favorite && !included.has(item.id)) { ordered.push(item); included.add(item.id); }
    return ordered;
  }
  filtered = $derived.by(() => this.orderItems(this.resultItems));
  // 被动提示只读展示旧历史，但可操作序列仍为空，继续封锁搜索、数字和鼠标取用。
  visibleItems = $derived.by(() => this.interaction === "passive" ? this.orderItems(this.history.items) : this.filtered);
  favoriteCount = $derived(this.visibleItems.reduce((count, item) => count + (item.favorite ? 1 : 0), 0));
  currentItem = $derived(this.visibleItems[this.selected] ?? null);

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
  private lastResultTimer: ReturnType<typeof setTimeout> | null = null;
  private feedbackTimer: ReturnType<typeof setTimeout> | null = null;

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
    const selectedId = this.currentItem?.id;
    const session = this.session;
    try {
      const favorites = await ipc.getFavorites?.();
      if (!favorites) return;
      this.favorites = favorites;
      if (this.visible && session === this.session && selectedId !== undefined) {
        const index = this.visibleItems.findIndex((item) => item.id === selectedId);
        this.selected = Math.max(0, index);
      }
    } catch {
      if (this.visible) this.message = "收藏顺序读取失败，已按记录中的收藏标记显示。";
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
    this.dismissLastResult();
    this.favoritePending = id;
    try {
      const updated = await ipc.setFavorite(id, !item.favorite);
      if (session !== this.session) return;
      if (!updated) { this.message = "这条记录已不在历史里。"; return; }
      // 只补丁更新返回项；原始最近/搜索数组不重载、不重新排序，其他收藏的位置不变。
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

  togglePreview = () => {
    if (!this.visible || !this.interactive || this.busy || this.awaitingRelease || this.mode !== "quick") return;
    this.dismissLastResult();
    if (this.previewOpen) this.previewOpen = false;
    else if (this.currentItem) this.previewOpen = true;
  };

  dismissLastResult = () => {
    this.lastResult = null;
    if (this.lastResultTimer !== null) clearTimeout(this.lastResultTimer);
    this.lastResultTimer = null;
  };

  private showLastResult() {
    if (this.lastResultTimer !== null) clearTimeout(this.lastResultTimer);
    this.lastResultTimer = setTimeout(() => { this.lastResult = null; this.lastResultTimer = null; }, 4000);
  }

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
    if (value) this.dismissLastResult();
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

  windowBlur = () => { this.ctrlDown = false; this.inputBlur(); };

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
    this.ctrlDown = event.ctrlKey;
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
    this.dismissLastResult();
    this.selected = Math.max(0, Math.min(this.filtered.length - 1, this.selected + offset));
  };

  choose = async (index = this.selected, pasteMode: PasteMode = "paste", event?: KeyboardEvent | MouseEvent) => {
    if (!this.visible || !this.interactive || this.busy || this.pendingDom || !this.filtered[index]) return;
    this.dismissLastResult();
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
    this.dismissLastResult();
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
      case "Escape":
        if (this.previewOpen) { this.previewOpen = false; break; }
        if (this.interaction === "activated" && event) this.armDomChoice({ session: this.session, panelMode: this.mode,
          id: null, mode: "copy", code: this.domCode(event), downAt: event.timeStamp, released: false });
        else this.close();
        break;
      default: if (/^[1-9]$/.test(key)) void this.choose(Number(key) - 1, "paste", event);
    }
  };

  // 普通 Quick 走钩子；Activated 在同一真实输入框中处理快速键和自然搜索输入。
  keydown = (event: KeyboardEvent) => {
    this.ctrlDown = event.ctrlKey;
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
        const action = event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key.toLowerCase() === "d"
          ? "Ctrl+D"
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
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) void this.choose(Number(event.key) - 1, "paste", event);
      return;
    }
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "Enter") {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) this.navigate("Ctrl+Enter", event);
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
        this.ctrlDown = false;
        this.imeActive = false;
        this.favoritePending = null;
        if (this.lastResult) this.showLastResult();
        else this.dismissLastResult();
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
        this.ctrlDown = false;
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
        // 通用路径先收起再回执：只保留下次呼出的一次性结论，不把“发键”推断成“已粘贴”。
        if (payload.paste_result === "confirmed" || payload.paste_result === "sent" || payload.paste_result === "copied") {
          this.lastResult = payload.paste_result;
        } else if (payload.paste_result === "copied_only" && !payload.hint_dwell_ms) {
          this.lastResult = "copied";
        }
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
        const labels = {
          copied: "已复制",
          sent: payload.layer2?.timed_out ? "已发送粘贴指令，可能没粘进去" : "已发送粘贴指令",
          confirmed: "确认已粘贴",
          copied_only: payload.hint_message || COPIED_HINT,
          native_submission_unknown: "粘贴结果未确认，请先检查目标，避免重复粘贴",
        };
        const evidence: string[] = [labels[payload.paste_result]];
        const conversion = payload.as_text?.conversion;
        if (payload.as_text?.error) evidence.push("转文字失败，按原样写入");
        else if (conversion) evidence.push({ paths: "已转为路径", image_path: "已存为 PNG，粘贴路径", plain_text: "已去掉格式" }[conversion]);
        if (payload.actual_key) {
          evidence.push(payload.actual_key);
          const read = payload.layer2?.status;
          evidence.push(read === "target_read" ? "目标读取 ✓" : read === "other_process_first"
            ? "其他程序先读" : read === "before_send" ? "发键前已读取" : "目标读取未确认");
          evidence.push(payload.layer3.conclusion === "confirmed" ? "内容确认 ✓" : "内容未确认");
        }
        evidence.push(`${payload.selected_to_result_ms.toFixed(0)} ms`);
        this.message = evidence.join(" · ");
      });
      if (!disposed) await settingsReady;
      if (!disposed) await this.command(() => ipc.frontendReady());
    };
    void initialize().catch(() => { this.message = "初始化失败，请重启样机并查看日志。"; });
    return () => {
      disposed = true; this.searchRequest += 1; this.foregroundSnapshot = null;
      this.clearDomChoice(true); this.cancelDomFocus(); this.clearSearchTimer();
      this.dismissLastResult(); this.clearFeedback();
      unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame);
    };
  };
}
