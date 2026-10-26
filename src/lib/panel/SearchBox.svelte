<script lang="ts">
  // 普通 Quick 只读且走钩子；Activated 先挂载同一可编辑输入框，真实焦点就绪后开放取用。
  // 数字等命令键由 PanelState 阻止默认输入，普通首字和输入法自然保留在此框中。
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
  onbeforeinput={panel.beforeInput}
  onblur={panel.inputBlur}
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
