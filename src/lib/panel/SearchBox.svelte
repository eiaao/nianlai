<script lang="ts">
  // 手牌只有一个真实输入框；首键、输入法与焦点握手仍由 PanelState 独占。扇面时它看不见但仍在，摊开时在一列顶部。
  import type { PanelState } from "../state.svelte";
  let { panel }: { panel: PanelState } = $props();
</script>

<div class="search-box" class:active={panel.mode === "search"}>
  <span class="label" aria-hidden="true">{panel.searching ? "搜索" : panel.handList === "favorites" ? "常用" : "最近"}</span>
  <input
    bind:this={panel.input}
    bind:value={panel.query}
    type="search"
    aria-label="搜索最近和常用"
    placeholder="打字搜索最近和常用"
    readonly={panel.locked}
    tabindex={panel.locked ? -1 : 0}
    spellcheck="false"
    autocomplete="off"
    onpointerdown={panel.searchPointer}
    onbeforeinput={panel.beforeInput}
    onblur={panel.inputBlur}
    oncompositionstart={panel.startComposition}
    oncompositionend={panel.endComposition}
  />
  {#if panel.imeActive}<span class="label">拼写中</span>{/if}
</div>

<style>
  .search-box { width: 100%; height: 100%; display: flex; align-items: center; gap: 8px; padding: 0 12px; border-radius: 8px; background: var(--hand-card); box-shadow: inset 0 0 0 1px var(--hand-line-strong); color: var(--hand-ink-3); }
  .label { flex: none; color: var(--hand-ink-3); font-size: 12px; white-space: nowrap; }
  input { width: 100%; min-width: 0; height: 100%; padding: 0; border: 0; outline: 0; background: transparent; color: var(--hand-ink); caret-color: transparent; font-size: 15px; }
  .active input { caret-color: var(--hand-ink); }
  input::placeholder { color: var(--hand-ink-3); font-size: 13px; opacity: 1; }
  input::-webkit-search-cancel-button { display: none; }
</style>
