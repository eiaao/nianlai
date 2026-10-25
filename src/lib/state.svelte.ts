// 面板状态和交互逻辑（Svelte 5 runes）。组件只负责显示和把 DOM 事件转到这里；和后端的往来都经过 ipc.ts。
// 这里的时序细节都是 G0 踩过的坑，改之前先读 src/lib/README.md 的“不能动的细节”。
import { tick, untrack } from "svelte";
import * as ipc from "./ipc";
import type { HistoryItem, HistorySnapshot, PasteMode, Timing, UnlistenFn } from "./ipc";

// 目标程序以管理员身份运行时粘贴降级为只复制，页脚当场给这句提示（M0-6）。
export const ELEVATED_HINT = "目标程序以管理员身份运行，已复制到剪贴板，请手动粘贴（Ctrl+V）";

export const formatTime = (millis: number) => new Date(millis).toLocaleTimeString("zh-CN", { hour12: false });

export class PanelState {
  history = $state<HistorySnapshot>({ items: [], total: 0, recording: false, last_recorded_ms: null, store_status: null });
  query = $state("");
  mode = $state<"quick" | "search">("quick");
  // 实验 NIANLAI_G0_SEARCHBOX_FOCUS（后端随 panel:shown 下发）：开着时快速模式也让搜索框持有页面内焦点。
  // 快速模式下面板窗口没有系统焦点，真实按键到不了页面，字不会漏进搜索框；钩子那边不用改。
  searchBoxFocus = $state(false);
  locked = $derived(this.mode === "quick" && !this.searchBoxFocus);
  selected = $state(0);
  session = $state(0);
  visible = $state(false);
  busy = $state(false);
  timing = $state<Timing | null>(null);
  message = $state("");
  version = $state("");
  searchItems = $state<HistoryItem[]>([]);
  searchedQuery = $state("");
  searchRevision = $state(0);
  filtered = $derived(this.mode === "search" ? this.searchItems : this.history.items);

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

  /** 登记面板的两个副作用（搜索、选中项夹紧和滚动）。必须在根组件初始化期间调用，顺序不要换。 */
  watch() {
    $effect(() => {
      const value = this.query;
      const revision = this.searchRevision;
      // 输入法组合中框里是拼音，先不搜；上屏后 compositionend 会再触发一次。
      if (this.visible && this.mode === "search" && !this.composing) untrack(() => { void revision; void this.search(value); });
    });

    $effect(() => {
      const index = this.selected;
      const count = this.filtered.length;
      if (count && index >= count) this.selected = count - 1;
      void tick().then(() => this.list?.querySelector<HTMLElement>('[aria-pressed="true"]')?.scrollIntoView({ block: "nearest" }));
    });
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
      if (reply.request_id !== this.searchRequest || !this.visible || this.mode !== "search" || this.session !== currentSession) return;
      const selectedId = this.filtered[this.selected]?.id;
      const scrollTop = this.list?.scrollTop ?? 0;
      this.searchItems = reply.items;
      this.searchedQuery = value;
      const index = reset ? 0 : this.searchItems.findIndex((item) => item.id === selectedId);
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

  // 发命令；失败时提示并解除 busy。call 要直接返回 ipc 的 Promise。
  private async command(call: () => Promise<unknown>) {
    try { await call(); }
    catch { this.message = "操作未完成，请查看 G0 日志。"; this.busy = false; }
  }

  move = (offset: number) => {
    this.selected = Math.max(0, Math.min(this.filtered.length - 1, this.selected + offset));
  };

  choose = async (index = this.selected, pasteMode: PasteMode = "paste") => {
    if (!this.visible || this.busy || !this.filtered[index]) return;
    this.selected = index;
    this.busy = true;
    await this.command(() => ipc.pasteItem(this.session, this.filtered[index].id, pasteMode));
  };

  navigate = (key: string) => {
    switch (key) {
      case "ArrowUp": this.move(-1); break;
      case "ArrowDown": this.move(1); break;
      case "PageUp": this.move(-7); break;
      case "PageDown": this.move(7); break;
      case "Home": this.selected = 0; break;
      case "End": this.selected = Math.max(0, this.filtered.length - 1); break;
      case "Enter": void this.choose(); break;
      case "Shift+Enter": void this.choose(this.selected, "copy"); break;
      case "Ctrl+Enter": void this.choose(this.selected, "text"); break;
      case "Escape": void this.command(() => ipc.hidePanel(this.session)); break;
      default: if (/^[1-9]$/.test(key)) void this.choose(Number(key) - 1);
    }
  };

  // 搜索模式下面板有焦点，按键从页面来；快速模式的按键由钩子经 panel:key 转来，见 connect()。
  keydown = (event: KeyboardEvent) => {
    if (!this.visible || this.mode !== "search") return;
    if (event.isComposing || this.composing || event.keyCode === 229) return;
    // 提交动作忽略长按重复；保留原 50ms 组合上屏保护和方向键长按导航。
    if (event.ctrlKey && !event.altKey && !event.metaKey && /^[1-9]$/.test(event.key)) {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) void this.choose(Number(event.key) - 1);
      return;
    }
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "Enter") {
      event.preventDefault();
      if (!event.repeat && performance.now() - this.compositionEnded >= 50) this.navigate("Ctrl+Enter");
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
      this.navigate(event.key === "Enter" && event.shiftKey ? "Shift+Enter" : event.key);
    }
  };

  // 诊断：补发后搜索框收到的第一个输入，是直接上屏（insertText）还是进了输入法（insertCompositionText）。只报类型，不报内容。
  reportFirstInput = (event: InputEvent) => {
    if (this.firstInput?.session !== this.session) return;
    const afterReadyMs = performance.now() - this.firstInput.readyAt;
    this.firstInput = null;
    void ipc.reportSearchInput(this.session, event.inputType, event.isComposing, afterReadyMs).catch(() => {});
  };

  searchPointer = (event: PointerEvent) => {
    if (this.mode === "quick") {
      event.preventDefault();
      void this.command(() => ipc.startSearch(this.session));
    }
  };

  startComposition = () => { this.composing = true; };

  endComposition = () => { this.composing = false; this.compositionEnded = performance.now(); this.searchRevision += 1; };

  /** onMount 时调用：读版本号、订阅后端事件，全部订阅好后再发 frontend_ready；返回卸载时的清理函数。 */
  connect = () => {
    let disposed = false;
    void ipc.appVersion().then((value) => { if (!disposed) this.version = value; }).catch(() => {});
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
            const index = this.filtered.findIndex((item) => item.id === selectedId);
            this.selected = index >= 0 ? index : 0;
          }
        }
      } catch { if (!disposed) this.message = "读取历史失败，请查看 G0 日志。"; }
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
      await subscribe(ipc.onPanelShown, (payload) => {
        if (payload.session < this.session) return;
        this.searchRequest += 1;
        this.session = payload.session;
        this.visible = true;
        this.mode = "quick";
        this.query = "";
        // 每次呼出都从最新一条开始；只有面板开着时来了新复制，才保持原来的选中项。
        this.history = payload.history;
        this.selected = 0;
        this.busy = false;
        this.composing = false;
        this.compositionEnded = -Infinity;
        this.searchBoxFocus = payload.experiments?.searchBoxFocus === true;
        this.timing = { session: payload.session, hotkeyToShowMs: payload.hotkeyToShowMs };
        const current = this.session;
        // 实验开着时等 readonly 摘掉再聚焦，让焦点一直停在搜索框；关着时照旧 blur，按键全交给钩子。
        if (this.searchBoxFocus) void tick().then(() => { if (!disposed && this.visible && this.session === current) this.input?.focus({ preventScroll: true }); });
        else this.input?.blur();
        frame(() => frame(() => {
          if (this.visible && this.session === current) void this.command(() => ipc.reportPainted(current));
        }));
      });
      await subscribe(ipc.onPanelKey, (payload) => {
        if (this.visible && payload.session === this.session && this.mode === "quick") this.navigate(payload.key);
      });
      await subscribe(ipc.onHistoryChanged, (payload) => {
        if (this.visible && payload.session === this.session) void refreshHistory();
      });
      await subscribe(ipc.onPanelMode, async (payload) => {
        if (!this.visible || payload.session !== this.session) return;
        this.mode = payload.mode;
        await tick();
        if (disposed || !this.visible || payload.session !== this.session) return;
        this.input.focus({ preventScroll: true });
        // 输入法要等 Blink 把“这是搜索框”报给浏览器进程、TSF 切到可编辑文档后才接管按键；没有用户手势时
        // 这份报告要等下一帧。等两帧再让后端补发首键，否则首字母可能绕过输入法直接上屏。
        await new Promise<void>((resolve) => frame(resolve));
        await new Promise<void>((resolve) => frame(resolve));
        if (!disposed && this.visible && payload.session === this.session && document.activeElement === this.input) {
          this.firstInput = { session: payload.session, readyAt: performance.now() };
          await this.command(() => ipc.searchReady(this.session));
        }
      });
      await subscribe(ipc.onPanelHidden, (payload) => {
        if (payload.session !== this.session) return;
        this.searchRequest += 1;
        this.visible = false;
        this.mode = "quick";
        this.composing = false;
        // 实验开着时焦点留在搜索框，下次呼出不用再聚焦。
        if (!this.searchBoxFocus) this.input?.blur();
      });
      await subscribe(ipc.onPanelTiming, (payload) => { if (payload.session === this.session) this.timing = payload; });
      await subscribe(ipc.onPanelError, (payload) => {
        if (payload.session === this.session) { this.message = payload.message; this.busy = false; }
      });
      await subscribe(ipc.onPasteResult, (payload) => {
        if (payload.session !== this.session) return;
        // 面板停留中（后端带了 hint_dwell_ms）：只显示提示，busy 保持到收起——停留期间 1–9、Enter、点条目
        // 都不再发起粘贴；Esc、呼出键、点面板外照旧收起。
        if (payload.hint_dwell_ms) { this.message = ELEVATED_HINT; return; }
        this.busy = false;
        const labels = {
          copied: "已复制",
          sent: payload.layer2?.timed_out ? "已发送粘贴指令，可能没粘进去" : "已发送粘贴指令",
          confirmed: "确认已粘贴",
          copied_only: payload.reason === "target_may_be_elevated" ? ELEVATED_HINT : "已复制，请到目标位置粘贴",
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
      if (!disposed) await this.command(() => ipc.frontendReady());
    };
    void initialize().catch(() => { this.message = "初始化失败，请重启样机并查看日志。"; });
    return () => { disposed = true; this.searchRequest += 1; unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame); };
  };
}
