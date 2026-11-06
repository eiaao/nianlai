<script lang="ts">
  // 三布局共享同一个真实输入框：切换布局只改呈现，不重建 G0 的首键、IME 与焦点握手节点。
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
  const placeholder = $derived(panel.preferences.layout === "b"
    ? panel.mode === "quick" ? "输入任何字，开始查找" : "搜索全文和文件名"
    : panel.preferences.layout === "c"
      ? panel.mode === "quick" ? "打字检索视觉历史" : "输入关键词，Esc 返回"
      : panel.mode === "quick" ? "打字搜索；数字直接取用" : "搜索全文和文件名");
  const prompt = $derived(panel.preferences.layout === "b"
    ? panel.mode === "quick" ? "→" : "?"
    : panel.mode === "quick" ? "/" : "›");
</script>

<div class="search-box" class:active={panel.mode === "search"}>
  <svg class="search-icon" viewBox="0 0 20 20" fill="none" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" stroke="currentColor" stroke-width="1.4"/><path d="m13 13 4 4" stroke="currentColor" stroke-width="1.4"/></svg>
  <span class="search-prompt" aria-hidden="true">{prompt}</span>
  <input
    bind:this={panel.input}
    bind:value={panel.query}
    type="search"
    aria-label="搜索全文和文件名"
    placeholder={placeholder}
    readonly={panel.locked}
    tabindex={panel.locked ? -1 : 0}
    class:quiet={panel.searchBoxFocus && panel.mode === "quick"}
    spellcheck="false"
    autocomplete="off"
    onpointerdown={panel.searchPointer}
    onbeforeinput={panel.beforeInput}
    onblur={panel.inputBlur}
    oncompositionstart={panel.startComposition}
    oncompositionend={panel.endComposition}
  />
  <span class="search-hint">{panel.imeActive ? "拼写中 · 暂不搜索" : panel.mode === "quick" ? "数字直接取用 · 选中项或按住 Ctrl 显示槽位" : "Ctrl+数字取用 · Esc 返回"}</span>
  <span class="search-count">{panel.preferences.layout === "c"
    ? `FRAME ${panel.visibleItems.length ? String(panel.selected + 1).padStart(2, "0") : "00"} / ${String(panel.visibleItems.length).padStart(2, "0")}`
    : panel.mode === "search" ? `${panel.visibleItems.length} 条结果` : `${panel.history.total} 条`}</span>
</div>

<style>
  .search-box { min-width: 0; height: 100%; display: flex; align-items: center; gap: 10px; padding: 0 16px; }
  .search-box.active { background: var(--field); box-shadow: inset 0 -2px 0 var(--accent); }
  .search-icon { width: 16px; height: 16px; flex: none; color: var(--faint); }
  .active .search-icon { color: var(--accent); }
  .search-prompt, .search-hint { display: none; }
  input { width: 100%; min-width: 0; height: 100%; padding: 0; border: 0; outline: 0; background: transparent; color: var(--text); caret-color: transparent; font-size: 13px; }
  .active input { caret-color: var(--accent); }
  input::placeholder { color: var(--faint); opacity: 1; }
  input::-webkit-search-cancel-button { display: none; }
  .search-count { color: var(--faint); font: 10px var(--font-mono); white-space: nowrap; }
  /* NIANLAI_G0_SEARCHBOX_FOCUS 实验仍藏掉 quick 焦点外观。 */
  input.quiet { caret-color: transparent; }

  :global(html[data-layout="b"]) .search-box {
    height: auto; display: grid; grid-template-columns: auto minmax(0, 1fr) auto; grid-template-rows: auto auto;
    column-gap: 13px; row-gap: 8px; padding: 10px 21px 17px; border-bottom: 1px solid var(--text);
  }
  :global(html[data-layout="b"]) .search-box.active { padding-bottom: 15px; border-bottom: 3px solid var(--accent); background: transparent; box-shadow: none; }
  :global(html[data-layout="b"]) .search-icon { display: none; }
  :global(html[data-layout="b"]) .search-prompt { display: block; grid-column: 1; grid-row: 1; color: var(--faint); font: 300 27px/1 var(--font-display); }
  :global(html[data-layout="b"]) .active .search-prompt { color: var(--accent); }
  :global(html[data-layout="b"]) input { grid-column: 2; grid-row: 1; height: auto; font: 500 26px/1.35 var(--font-display); letter-spacing: -.025em; }
  :global(html[data-layout="b"]) input::placeholder { font-weight: 400; opacity: .8; }
  :global(html[data-layout="b"]) .search-hint { display: block; grid-column: 1 / 3; grid-row: 2; min-width: 0; overflow: hidden; color: var(--faint); font: 10px/1.4 var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
  :global(html[data-layout="b"]) .search-count { grid-column: 3; grid-row: 2; align-self: center; }

  :global(html[data-layout="c"]) .search-box { display: grid; grid-template-columns: auto minmax(0, 1fr) 112px; gap: 9px; padding: 0 15px 0 16px; }
  :global(html[data-layout="c"]) .search-box.active { box-shadow: inset 3px 0 0 var(--accent); }
  :global(html[data-layout="c"]) .search-icon { display: none; }
  :global(html[data-layout="c"]) .search-prompt { display: block; color: var(--faint); font: 15px var(--font-mono); }
  :global(html[data-layout="c"]) .active .search-prompt, :global(html[data-layout="c"]) .active .search-count { color: var(--accent); }
  :global(html[data-layout="c"]) .search-count { padding-left: 8px; color: var(--muted); font-size: 9px; text-align: right; letter-spacing: .04em; }

  @media (max-width: 599px) {
    .search-box { gap: 6px; padding: 0 8px; }
    .search-count { font-size: 8px; }
    input { font-size: 11px; }
    .search-icon { width: 14px; height: 14px; }
    :global(html[data-layout="b"]) .search-box { padding: 9px 12px 14px; column-gap: 9px; }
    :global(html[data-layout="b"]) .search-box.active { padding-bottom: 12px; }
    :global(html[data-layout="b"]) input { font-size: 21px; }
    :global(html[data-layout="b"]) .search-hint { max-width: 250px; font-size: 8px; }
    :global(html[data-layout="c"]) .search-box { grid-template-columns: auto minmax(0, 1fr) 82px; padding: 0 8px; }
    :global(html[data-layout="c"]) .search-count { font-size: 7px; }
  }
</style>
