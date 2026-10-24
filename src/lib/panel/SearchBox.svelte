<script lang="ts">
  // 搜索框。快速模式下只读、不在 Tab 序列里（按键由钩子转发），点一下请求进入搜索模式；
  // 输入法组合、首个输入的上报都交给 PanelState。
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<input
  bind:this={panel.input}
  bind:value={panel.query}
  type="search"
  aria-label="搜索全文和文件名"
  placeholder={panel.mode === "quick" ? "直接打字或按 / 搜索" : "搜索全文和文件名"}
  readonly={panel.locked}
  tabindex={panel.locked ? -1 : 0}
  class:quiet={panel.searchBoxFocus && panel.mode === "quick"}
  spellcheck="false"
  autocomplete="off"
  onpointerdown={panel.searchPointer}
  onbeforeinput={panel.reportFirstInput}
  oncompositionstart={panel.startComposition}
  oncompositionend={panel.endComposition}
/>

<style>
  input { width: 100%; height: 38px; padding: var(--space-2) var(--space-search-field-padding-x); border: 1px solid var(--color-border-control); border-radius: var(--radius-search-field); background: var(--color-bg-layer); outline: none; }
  input:focus { border-color: var(--color-accent); }
  input::placeholder { color: var(--color-text-tertiary); }
  input::-webkit-search-cancel-button { display: none; }
  /* 实验 NIANLAI_G0_SEARCHBOX_FOCUS：快速模式下搜索框持有焦点但不该像在编辑，藏掉光标和焦点色，不改布局。 */
  input.quiet { caret-color: transparent; }
  input.quiet:focus { border-color: var(--color-border-control); }
</style>
