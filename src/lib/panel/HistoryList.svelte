<script lang="ts">
  // 历史列表：快速模式显示最近的历史，搜索模式显示搜索结果（PanelState.filtered）。
  import type { PanelState } from "../state.svelte";
  import HistoryItem from "./HistoryItem.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<ul bind:this={panel.list} aria-label="剪贴板历史" aria-busy={panel.busy}>
  {#each panel.filtered as item, index (item.id)}
    <li><HistoryItem {panel} {item} {index} /></li>
  {:else}
    <li class="empty">{panel.query ? "没有匹配的历史" : "复制内容后会显示在这里"}</li>
  {/each}
</ul>

<style>
  ul { flex: 1; min-height: 0; list-style: none; margin: 0; padding: var(--space-list-padding-top) var(--space-list-padding) var(--space-list-padding); overflow-y: auto; scrollbar-width: thin; scrollbar-color: var(--color-scrollbar-thumb) transparent; }
  li { margin: 0; padding: 0; }
  .empty { padding: var(--space-empty-padding-y) var(--space-3); text-align: center; color: var(--color-text-secondary); }
</style>
