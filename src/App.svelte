<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type Sample = { id: number; text: string };
  type SessionEvent = { session: number };
  type Timing = SessionEvent & { hotkeyToShowMs: number; showToPaintMs?: number };
  let samples = $state<Sample[]>([]);
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
  const filtered = $derived(samples.filter((sample) => sample.text.toLocaleLowerCase().includes(query.toLocaleLowerCase())));

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

  async function choose(index = selected) {
    if (!visible || busy || !filtered[index]) return;
    selected = index;
    busy = true;
    await command("paste_sample", { session, id: filtered[index].id });
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
      case "Escape": void command("hide_panel", { session }); break;
      default: if (/^[1-9]$/.test(key)) void choose(Number(key) - 1);
    }
  }

  function keydown(event: KeyboardEvent) {
    if (!visible || mode !== "search") return;
    if (event.isComposing || composing || event.keyCode === 229) return;
    if ((event.ctrlKey && ["f", "p", "r", "+", "-", "=", "0"].includes(event.key.toLowerCase())) || event.key === "F5" || event.key === "F7") {
      event.preventDefault();
      if (event.key.toLowerCase() === "f") input?.focus({ preventScroll: true });
      return;
    }
    if (!event.ctrlKey && !event.altKey && !event.metaKey && (event.target === input || event.key === "Escape") && ["Escape", "Enter", "ArrowUp", "ArrowDown"].includes(event.key)) {
      event.preventDefault();
      if (performance.now() - compositionEnded < 50) return;
      navigate(event.key);
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
    function frame(callback: () => void) {
      const id = requestAnimationFrame(() => { frames.delete(id); if (!disposed) callback(); });
      frames.add(id);
    }
    async function subscribe<T>(name: string, handler: (payload: T) => void) {
      const stop = await listen<T>(name, ({ payload }) => { if (!disposed) handler(payload); });
      if (disposed) stop(); else unlisten.push(stop);
    }
    async function initialize() {
      await subscribe<Timing>("panel:shown", (payload) => {
        if (payload.session < session) return;
        session = payload.session;
        visible = true;
        mode = "quick";
        query = "";
        selected = 0;
        busy = false;
        composing = false;
        compositionEnded = -Infinity;
        input?.blur();
        timing = payload;
        const current = session;
        frame(() => frame(() => {
          if (visible && session === current) void command("report_painted", { session: current });
        }));
      });
      await subscribe<SessionEvent & { key: string }>("panel:key", (payload) => {
        if (visible && payload.session === session && mode === "quick") navigate(payload.key);
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
        if (payload.session === session) message = payload.message;
      });
      await subscribe<SessionEvent & { paste_result: string }>("panel:paste-result", (payload) => {
        if (payload.session !== session) return;
        busy = false;
        message = payload.paste_result === "sent" ? "已发送粘贴指令" : payload.paste_result === "clipboard_failed" ? "写入剪贴板失败，请重试" : "已复制，请到目标位置粘贴";
      });
      samples = await invoke<Sample[]>("get_samples");
      if (!disposed) await command("frontend_ready");
    }
    void initialize().catch(() => { message = "初始化失败，请重启样机并查看日志。"; });
    return () => { disposed = true; unlisten.forEach((stop) => stop()); frames.forEach(cancelAnimationFrame); };
  });
</script>

<svelte:window onkeydown={keydown} oncontextmenu={(event) => event.preventDefault()} />

<main aria-label="拈来剪贴板样机">
  <header>
    <div class="heading"><strong>拈来</strong><span>G0 · 样机一</span><span class="count">{filtered.length} 条示例</span></div>
    <input
      bind:this={input}
      bind:value={query}
      type="search"
      aria-label="搜索示例文本"
      placeholder={mode === "quick" ? "直接打字或按 / 搜索" : "搜索示例文本"}
      readonly={mode === "quick"}
      tabindex={mode === "quick" ? -1 : 0}
      spellcheck="false"
      autocomplete="off"
      onpointerdown={searchPointer}
      oninput={() => { selected = 0; }}
      oncompositionstart={() => { composing = true; }}
      oncompositionend={() => { composing = false; compositionEnded = performance.now(); }}
    />
    <div class="hint">{mode === "quick" ? "↑ ↓ 选择 · 1–9 取用 · Enter 粘贴 · Esc 收起" : "输入关键词 · ↑ ↓ 选择 · Enter 粘贴 · Esc 返回"}</div>
  </header>

  <ul bind:this={list} aria-label="示例文本" aria-busy={busy}>
    {#each filtered as sample, index (sample.id)}
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
          <span class="preview">{sample.text}</span>
        </button>
      </li>
    {:else}
      <li class="empty">没有匹配的示例文本</li>
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
