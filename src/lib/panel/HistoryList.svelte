<script lang="ts">
  // 可见序列由 PanelState 统一提供：收藏区在前，最近区保持原历史/搜索顺序。
  import type { PanelState } from "../state.svelte";
  import HistoryItem from "./HistoryItem.svelte";
  import SelectionRail from "./SelectionRail.svelte";

  let { panel, layout = "a" }: { panel: PanelState; layout?: "a" | "b" } = $props();
</script>

<ul
  class:command-list={layout === "b"}
  bind:this={panel.list}
  role="listbox"
  tabindex="-1"
  aria-label="剪贴板历史"
  aria-busy={panel.busy || panel.favoritePending !== null}
  aria-activedescendant={panel.currentItem ? `history-option-${panel.currentItem.id}` : undefined}
>
  <SelectionRail {panel} {layout} />
  {#if panel.favoriteCount > 0 && panel.visibleItems.length > 0}
    <li class="section-heading" role="presentation"><svg viewBox="0 0 20 20" fill="currentColor" aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z"/></svg><span>收藏 {panel.favoriteCount} / PINNED</span></li>
  {/if}
  {#each panel.visibleItems as item, index (item.id)}
    {#if panel.favoriteCount > 0 && index === panel.favoriteCount}
      <li class="section-heading" role="presentation"><span>最近 {panel.visibleItems.length - panel.favoriteCount} / RECENT</span></li>
    {/if}
    <HistoryItem {panel} {item} {index} {layout} />
  {:else}
    <li class="empty">
      <strong>{panel.query ? `没有找到“${panel.query}”` : "还没有记录"}</strong>
      <span>{panel.query ? "试试更短的关键词；Esc 返回完整历史。" : "复制文字、图片或文件，它们会按时间出现在这里。"}</span>
    </li>
  {/each}
</ul>

<style>
  ul { position: relative; flex: 1; min-height: 0; list-style: none; margin: 0; padding: 5px 0; overflow-y: auto; scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  .section-heading { height: 29px; display: flex; align-items: center; gap: 7px; margin: 0; padding: 0 12px; color: var(--faint); font: 9px var(--font-mono); letter-spacing: .08em; }
  .section-heading svg { width: 14px; height: 14px; color: var(--star); }
  .empty { display: grid; gap: var(--space-2); margin: 0; padding: 38px 18px; color: var(--muted); font-size: 12px; line-height: 1.8; }
  .empty strong { color: var(--text); font-weight: 600; overflow-wrap: anywhere; }
  .command-list { padding: 4px 0; scrollbar-color: var(--line) transparent; }
  .command-list .section-heading { height: 28px; padding: 0 21px; }
  .command-list .empty { padding: 45px 22px; font-size: 13px; }
</style>
