<script lang="ts">
  // 同一真实输入框继续承载 G0 首键、IME 与焦点握手；这里只增加 v7 的搜索态呈现。
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<div class="search-box" class:active={panel.mode === "search"}>
  <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5" stroke="currentColor" stroke-width="1.4"/><path d="m13 13 4 4" stroke="currentColor" stroke-width="1.4"/></svg>
  <input
    bind:this={panel.input}
    bind:value={panel.query}
    type="search"
    aria-label="搜索全文和文件名"
    placeholder={panel.mode === "quick" ? "打字搜索；数字直接取用" : "搜索全文和文件名"}
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
  <span class="search-count">{panel.mode === "search" ? `${panel.visibleItems.length} 条结果` : `${panel.history.total} 条`}</span>
</div>

<style>
  .search-box { min-width: 0; height: 100%; display: flex; align-items: center; gap: 10px; padding: 0 16px; }
  .search-box.active { background: var(--field); box-shadow: inset 0 -2px 0 var(--accent); }
  svg { width: 16px; height: 16px; flex: none; color: var(--faint); }
  .active svg { color: var(--accent); }
  input { width: 100%; min-width: 0; height: 100%; padding: 0; border: 0; outline: 0; background: transparent; color: var(--text); caret-color: transparent; font-size: 13px; }
  .active input { caret-color: var(--accent); }
  input::placeholder { color: var(--faint); opacity: 1; }
  input::-webkit-search-cancel-button { display: none; }
  .search-count { color: var(--faint); font: 10px var(--font-mono); white-space: nowrap; }
  /* NIANLAI_G0_SEARCHBOX_FOCUS 实验仍藏掉 quick 焦点外观。 */
  input.quiet { caret-color: transparent; }
  @media (max-width: 599px) {
    .search-box { gap: 6px; padding: 0 8px; }
    .search-count { font-size: 8px; }
    input { font-size: 11px; }
    svg { width: 14px; height: 14px; }
  }
</style>
