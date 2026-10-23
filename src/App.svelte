<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type HistoryItem = { id: number; kind: string; preview: string; file_names: string[]; source: string; copied_ms: number; copy_count: number; partial: boolean };
  type HistorySnapshot = { items: HistoryItem[]; total: number; recording: boolean; last_recorded_ms: number | null };
  type SessionEvent = { session: number };
  type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
  type PasteResult = SessionEvent & {
    paste_result: "copied" | "sent" | "confirmed" | "copied_only";
    reason: string | null;
    actual_key: string | null;
    layer2: { status: string; timed_out: boolean } | null;
    layer3: { conclusion: string };
    selected_to_result_ms: number;
  };
  let history = $state<HistorySnapshot>({ items: [], total: 0, recording: false, last_recorded_ms: null });
  let query = $state("");
  let mode = $state<"quick" | "search">("quick");
  let selected = $state(0);
  let session = $state(0);
  let visible = $state(false);
  let busy = $state(false);
  let composing = false;
  let compositionEnded = -Infinity;
  let input: HTMLInputElement;
  let list: HTMLUListElement;
  let timing = $state<Timing | null>(null);
  let message = $state("");
  const filtered = $derived(history.items.filter((item) => [item.preview, ...item.file_names].some((text) => text.toLocaleLowerCase().includes(query.toLocaleLowerCase()))));
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

  async function choose(index = selected, pasteMode: "paste" | "copy" = "paste") {
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
      await subscribe<Timing & { history: HistorySnapshot }>("panel:shown", (payload) => {
        if (payload.session < session) return;
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
        input?.blur();
        timing = { session: payload.session, hotkeyToShowMs: payload.hotkeyToShowMs };
        const current = session;
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
        if (!disposed && visible && payload.session === session) {
          input.focus({ preventScroll: true });
          if (document.activeElement === input) await command("search_ready", { session });
        }
      });
      await subscribe<SessionEvent>("panel:hidden", (payload) => {
        if (payload.session !== session) return;
        visible = false;
        mode = "quick";
        composing = false;
        input?.blur();
      });
      await subscribe<Timing>("panel:timing", (payload) => { if (payload.session === session) timing = payload; });
      await subscribe<SessionEvent & { message: string }>("panel:error", (payload) => {
        if (payload.session === session) { message = payload.message; busy = false; }
      });
      await subscribe<PasteResult>("panel:paste-result", (payload) => {
        if (payload.session !== session) return;
        busy = false;
        const labels = {
          copied: "已复制",
          sent: payload.layer2?.timed_out ? "已发送粘贴指令，可能没粘进去" : "已发送粘贴指令",
          confirmed: "确认已粘贴",
          copied_only: payload.reason === "target_may_be_elevated"
            ? "目标程序以管理员身份运行，已复制，请手动粘贴" : "已复制，请到目标位置粘贴",
        };
        const evidence: string[] = [labels[payload.paste_result]];
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
    return () => { disposed = true; unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame); };
  });
</script>

<svelte:window onkeydown={keydown} oncontextmenu={(event) => event.preventDefault()} />

<main aria-label="拈来剪贴板样机">
  <header>
    <div class="heading"><strong>拈来</strong><span>G0 · 样机三</span><span class="count">{query ? `${filtered.length} / ` : ""}{history.total} 条</span></div>
    <div class="recording" class:inactive={!history.recording} role="status">{history.recording ? "记录中" : "未在记录"}{history.last_recorded_ms ? ` · 上次 ${time(history.last_recorded_ms)}` : ""}{history.total > 300 ? " · 显示最近 300 条" : ""}</div>
    <input
      bind:this={input}
      bind:value={query}
      type="search"
      aria-label="搜索预览和文件名"
      placeholder={mode === "quick" ? "直接打字或按 / 搜索" : "搜索预览和文件名"}
      readonly={mode === "quick"}
      tabindex={mode === "quick" ? -1 : 0}
      spellcheck="false"
      autocomplete="off"
      onpointerdown={searchPointer}
      oninput={() => { selected = 0; }}
      oncompositionstart={() => { composing = true; }}
      oncompositionend={() => { composing = false; compositionEnded = performance.now(); }}
    />
    <div class="hint">{mode === "quick" ? "↑ ↓ 选择 · 1–9 取用 · Enter 粘贴 · Shift+Enter 复制 · Esc 收起" : "↑ ↓ 选择 · Ctrl+1–9 取用 · Enter 粘贴 · Shift+Enter 复制 · Esc 返回"}</div>
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
            <span class="preview">{item.preview || "（空文本）"}</span>
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
