<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type HistoryItem = { id: number; kind: string; preview: string; file_names: string[]; source: string; copied_ms: number; copy_count: number; partial: boolean };
  type HistorySnapshot = { items: HistoryItem[]; total: number; recording: boolean; last_recorded_ms: number | null; store_status: string | null };
  type SessionEvent = { session: number };
  type Experiments = { searchBoxFocus?: boolean };
  type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
  type PasteResult = SessionEvent & {
    paste_result: "copied" | "sent" | "confirmed" | "copied_only";
    reason: string | null;
    actual_key: string | null;
    layer2: { status: string; timed_out: boolean } | null;
    layer3: { conclusion: string };
    as_text: { conversion: "paths" | "image_path" | "plain_text" | null; error?: string } | null;
    selected_to_result_ms: number;
    // 后端判定面板要停留（目标程序是管理员、只复制了）时带上停留毫秒数；其他结论没有这个字段。
    hint_dwell_ms?: number;
  };
  // 目标程序以管理员身份运行时粘贴降级为只复制，页脚当场给这句提示（M0-6）。
  const ELEVATED_HINT = "目标程序以管理员身份运行，已复制到剪贴板，请手动粘贴（Ctrl+V）";
  let history = $state<HistorySnapshot>({ items: [], total: 0, recording: false, last_recorded_ms: null, store_status: null });
  let query = $state("");
  let mode = $state<"quick" | "search">("quick");
  // 实验 NIANLAI_G0_SEARCHBOX_FOCUS（后端随 panel:shown 下发）：开着时快速模式也让搜索框持有页面内焦点。
  // 快速模式下面板窗口没有系统焦点，真实按键到不了页面，字不会漏进搜索框；钩子那边不用改。
  let searchBoxFocus = $state(false);
  const locked = $derived(mode === "quick" && !searchBoxFocus);
  let selected = $state(0);
  let session = $state(0);
  let visible = $state(false);
  let busy = $state(false);
  let composing = false;
  let compositionEnded = -Infinity;
  let firstInput: { session: number; readyAt: number } | null = null;
  let input: HTMLInputElement;
  let list: HTMLUListElement;
  let timing = $state<Timing | null>(null);
  let message = $state("");
  let searchItems = $state<HistoryItem[]>([]);
  let searchedQuery = $state("");
  let searchRevision = $state(0);
  // 只在 search() 里递增。真实按键的 input 事件在 bind:value 和委托的 oninput 两个监听器之间会先跑微任务、
  // 发出搜索；若在 oninput 里再递增，刚发出的请求就过期了，回复全被丢掉，列表一直空着。
  let searchRequest = 0;
  let previousQuery = "";
  let previousSession = -1;
  const filtered = $derived(mode === "search" ? searchItems : history.items);

  async function search(value: string) {
    const requestId = ++searchRequest;
    const currentSession = session;
    const reset = value !== previousQuery || currentSession !== previousSession;
    previousQuery = value;
    previousSession = currentSession;
    if (reset) { selected = 0; searchItems = []; }
    try {
      const reply = await invoke<{ request_id: number; items: HistoryItem[] }>("search_history", { query: value, requestId });
      if (reply.request_id !== searchRequest || !visible || mode !== "search" || session !== currentSession) return;
      const selectedId = filtered[selected]?.id;
      const scrollTop = list?.scrollTop ?? 0;
      searchItems = reply.items;
      searchedQuery = value;
      const index = reset ? 0 : searchItems.findIndex((item) => item.id === selectedId);
      selected = Math.max(0, index);
      await tick();
      if (!reset && requestId === searchRequest && list) list.scrollTop = scrollTop;
    } catch {
      if (requestId === searchRequest && visible && currentSession === session) message = "搜索失败，请查看 G0 日志。";
    }
  }

  $effect(() => {
    const value = query;
    const revision = searchRevision;
    // 输入法组合中框里是拼音，先不搜；上屏后 compositionend 会再触发一次。
    if (visible && mode === "search" && !composing) untrack(() => { void revision; void search(value); });
  });

  function highlights(text: string) {
    const lower = (value: string) => value.replace(/[A-Z]/g, (c) => c.toLowerCase());
    const normalized = lower(text);
    const hits = new Set<number>();
    if (mode === "search") for (const word of searchedQuery.split(/\s+/).filter(Boolean).map(lower)) {
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
  const time = (millis: number) => new Date(millis).toLocaleTimeString("zh-CN", { hour12: false });

  async function command(name: string, args: Record<string, unknown> = {}) {
    try { await invoke(name, args); }
    catch { message = "操作未完成，请查看 G0 日志。"; busy = false; }
  }

  $effect(() => {
    const index = selected;
    const count = filtered.length;
    if (count && index >= count) selected = count - 1;
    void tick().then(() => list?.querySelector<HTMLElement>('[aria-pressed="true"]')?.scrollIntoView({ block: "nearest" }));
  });

  function move(offset: number) {
    selected = Math.max(0, Math.min(filtered.length - 1, selected + offset));
  }

  async function choose(index = selected, pasteMode: "paste" | "copy" | "text" = "paste") {
    if (!visible || busy || !filtered[index]) return;
    selected = index;
    busy = true;
    await command("paste_item", { session, id: filtered[index].id, mode: pasteMode });
  }

  function navigate(key: string) {
    switch (key) {
      case "ArrowUp": move(-1); break;
      case "ArrowDown": move(1); break;
      case "PageUp": move(-7); break;
      case "PageDown": move(7); break;
      case "Home": selected = 0; break;
      case "End": selected = Math.max(0, filtered.length - 1); break;
      case "Enter": void choose(); break;
      case "Shift+Enter": void choose(selected, "copy"); break;
      case "Ctrl+Enter": void choose(selected, "text"); break;
      case "Escape": void command("hide_panel", { session }); break;
      default: if (/^[1-9]$/.test(key)) void choose(Number(key) - 1);
    }
  }

  function keydown(event: KeyboardEvent) {
    if (!visible || mode !== "search") return;
    if (event.isComposing || composing || event.keyCode === 229) return;
    if (event.ctrlKey && !event.altKey && !event.metaKey && /^[1-9]$/.test(event.key)) {
      event.preventDefault();
      if (performance.now() - compositionEnded >= 50) void choose(Number(event.key) - 1);
      return;
    }
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey && event.key === "Enter") {
      event.preventDefault();
      if (performance.now() - compositionEnded >= 50) navigate("Ctrl+Enter");
      return;
    }
    if ((event.ctrlKey && ["f", "p", "r", "+", "-", "=", "0"].includes(event.key.toLowerCase())) || event.key === "F5" || event.key === "F7") {
      event.preventDefault();
      if (event.key.toLowerCase() === "f") input?.focus({ preventScroll: true });
      return;
    }
    if (!event.ctrlKey && !event.altKey && !event.metaKey && (event.target === input || event.key === "Escape") && ["Escape", "Enter", "ArrowUp", "ArrowDown"].includes(event.key)) {
      event.preventDefault();
      if (performance.now() - compositionEnded < 50) return;
      navigate(event.key === "Enter" && event.shiftKey ? "Shift+Enter" : event.key);
    }
  }

  // 诊断：补发后搜索框收到的第一个输入，是直接上屏（insertText）还是进了输入法（insertCompositionText）。只报类型，不报内容。
  function reportFirstInput(event: InputEvent) {
    if (firstInput?.session !== session) return;
    const afterReadyMs = performance.now() - firstInput.readyAt;
    firstInput = null;
    void invoke("report_search_input", { session, inputType: event.inputType, composing: event.isComposing, afterReadyMs }).catch(() => {});
  }

  function searchPointer(event: PointerEvent) {
    if (mode === "quick") {
      event.preventDefault();
      void command("start_search", { session });
    }
  }

  onMount(() => {
    let disposed = false;
    const unlisten: UnlistenFn[] = [];
    const frames = new Set<number>();
    let fetching = false;
    let refreshPending = false;
    async function refreshHistory() {
      refreshPending = true;
      if (fetching) return;
      fetching = true;
      try {
        while (refreshPending && visible && !disposed) {
          refreshPending = false;
          const current = session;
          const snapshot = await invoke<HistorySnapshot>("get_history");
          if (!disposed && visible && current === session) {
            const selectedId = filtered[selected]?.id;
            history = snapshot;
            if (mode === "search") searchRevision += 1;
            const index = filtered.findIndex((item) => item.id === selectedId);
            selected = index >= 0 ? index : 0;
          }
        }
      } catch { if (!disposed) message = "读取历史失败，请查看 G0 日志。"; }
      finally { fetching = false; }
    }
    function frame(callback: () => void) {
      const id = requestAnimationFrame(() => { frames.delete(id); if (!disposed) callback(); });
      frames.add(id);
    }
    async function subscribe<T>(name: string, handler: (payload: T) => void) {
      const stop = await listen<T>(name, ({ payload }) => { if (!disposed) handler(payload); });
      if (disposed) stop(); else unlisten.push(stop);
    }
    async function initialize() {
      await subscribe<Timing & { history: HistorySnapshot; experiments?: Experiments }>("panel:shown", (payload) => {
        if (payload.session < session) return;
        searchRequest += 1;
        session = payload.session;
        visible = true;
        mode = "quick";
        query = "";
        // 每次呼出都从最新一条开始；只有面板开着时来了新复制，才保持原来的选中项。
        history = payload.history;
        selected = 0;
        busy = false;
        composing = false;
        compositionEnded = -Infinity;
        searchBoxFocus = payload.experiments?.searchBoxFocus === true;
        timing = { session: payload.session, hotkeyToShowMs: payload.hotkeyToShowMs };
        const current = session;
        // 实验开着时等 readonly 摘掉再聚焦，让焦点一直停在搜索框；关着时照旧 blur，按键全交给钩子。
        if (searchBoxFocus) void tick().then(() => { if (!disposed && visible && session === current) input?.focus({ preventScroll: true }); });
        else input?.blur();
        frame(() => frame(() => {
          if (visible && session === current) void command("report_painted", { session: current });
        }));
      });
      await subscribe<SessionEvent & { key: string }>("panel:key", (payload) => {
        if (visible && payload.session === session && mode === "quick") navigate(payload.key);
      });
      await subscribe<SessionEvent>("panel:history-changed", (payload) => {
        if (visible && payload.session === session) void refreshHistory();
      });
      await subscribe<SessionEvent & { mode: "search" }>("panel:mode", async (payload) => {
        if (!visible || payload.session !== session) return;
        mode = payload.mode;
        await tick();
        if (disposed || !visible || payload.session !== session) return;
        input.focus({ preventScroll: true });
        // 输入法要等 Blink 把“这是搜索框”报给浏览器进程、TSF 切到可编辑文档后才接管按键；没有用户手势时
        // 这份报告要等下一帧。等两帧再让后端补发首键，否则首字母可能绕过输入法直接上屏。
        await new Promise<void>((resolve) => frame(resolve));
        await new Promise<void>((resolve) => frame(resolve));
        if (!disposed && visible && payload.session === session && document.activeElement === input) {
          firstInput = { session: payload.session, readyAt: performance.now() };
          await command("search_ready", { session });
        }
      });
      await subscribe<SessionEvent>("panel:hidden", (payload) => {
        if (payload.session !== session) return;
        searchRequest += 1;
        visible = false;
        mode = "quick";
        composing = false;
        // 实验开着时焦点留在搜索框，下次呼出不用再聚焦。
        if (!searchBoxFocus) input?.blur();
      });
      await subscribe<Timing>("panel:timing", (payload) => { if (payload.session === session) timing = payload; });
      await subscribe<SessionEvent & { message: string }>("panel:error", (payload) => {
        if (payload.session === session) { message = payload.message; busy = false; }
      });
      await subscribe<PasteResult>("panel:paste-result", (payload) => {
        if (payload.session !== session) return;
        // 面板停留中（后端带了 hint_dwell_ms）：只显示提示，busy 保持到收起——停留期间 1–9、Enter、点条目
        // 都不再发起粘贴；Esc、呼出键、点面板外照旧收起。
        if (payload.hint_dwell_ms) { message = ELEVATED_HINT; return; }
        busy = false;
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
        message = evidence.join(" · ");
      });
      if (!disposed) await command("frontend_ready");
    }
    void initialize().catch(() => { message = "初始化失败，请重启样机并查看日志。"; });
    return () => { disposed = true; searchRequest += 1; unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame); };
  });
</script>

<svelte:window onkeydown={keydown} oncontextmenu={(event) => event.preventDefault()} />

<main aria-label="拈来剪贴板样机">
  <header>
    <div class="heading"><strong>拈来</strong><span>G0 · 样机四</span><span class="count">{query ? `${filtered.length} / ` : ""}{history.total} 条</span></div>
    <div class="recording" class:inactive={!history.recording} role="status">{history.recording ? "记录中" : "未在记录"}{history.last_recorded_ms ? ` · 上次 ${time(history.last_recorded_ms)}` : ""}{mode === "quick" && history.total > 300 ? " · 显示最近 300 条" : ""}</div>
    {#if history.store_status}<div class="store-status" role="status">{history.store_status}</div>{/if}
    <input
      bind:this={input}
      bind:value={query}
      type="search"
      aria-label="搜索全文和文件名"
      placeholder={mode === "quick" ? "直接打字或按 / 搜索" : "搜索全文和文件名"}
      readonly={locked}
      tabindex={locked ? -1 : 0}
      class:quiet={searchBoxFocus && mode === "quick"}
      spellcheck="false"
      autocomplete="off"
      onpointerdown={searchPointer}
      onbeforeinput={reportFirstInput}
      oncompositionstart={() => { composing = true; }}
      oncompositionend={() => { composing = false; compositionEnded = performance.now(); searchRevision += 1; }}
    />
    <div class="hint">{mode === "quick" ? "↑ ↓ 选择 · 1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 收起" : "↑ ↓ 选择 · Ctrl+1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 返回"}</div>
  </header>

  <ul bind:this={list} aria-label="剪贴板历史" aria-busy={busy}>
    {#each filtered as item, index (item.id)}
      <li>
        <button
          class="entry"
          class:selected={index === selected}
          aria-pressed={index === selected}
          tabindex="-1"
          disabled={busy}
          onpointerenter={() => { selected = index; }}
          onpointerdown={(event) => event.preventDefault()}
          onclick={() => choose(index)}
        >
          <span class="badge" aria-hidden="true">{index < 9 ? index + 1 : "·"}</span>
          <span class="entry-content">
            <span class="entry-type">{item.kind}</span>
            <span class="preview">{#each highlights(item.preview || "（空文本）") as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
            <span class="metadata">{item.source} · {time(item.copied_ms)}{item.copy_count > 1 ? ` · 复制 ${item.copy_count} 次` : ""}</span>
            {#if item.partial}<span class="partial">部分格式未保存</span>{/if}
          </span>
        </button>
      </li>
    {:else}
      <li class="empty">{query ? "没有匹配的历史" : "复制内容后会显示在这里"}</li>
    {/each}
  </ul>

  <footer>
    {#if message}<p class="result" role="status">{message}</p>{/if}
    <div class="status">
      <span class="mode"><i class:search={mode === "search"}></i>{mode === "quick" ? "快速取用" : "搜索模式"}</span>
      <span class="timing" title="快捷键到显示 / 显示到前端首帧">{timing ? timing.hotkeyToShowMs.toFixed(1) : "—"} / {timing?.showToPaintMs?.toFixed(1) ?? "—"} ms</span>
      <!-- 面板常压在输入框上方，按钮容易被误点，退出只留快捷键。 -->
      <span class="quit">退出：Ctrl+Alt+Shift+Q</span>
    </div>
  </footer>
</main>
