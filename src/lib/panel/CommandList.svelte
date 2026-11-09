<script lang="ts">
  // B：排印列表与下预览抽屉；可见序列、收藏、搜索和选中 ID 仍由 PanelState 提供。
  import type { PanelState } from "../state.svelte";
  import HistoryList from "./HistoryList.svelte";
  import Preview from "./Preview.svelte";
  import { relativeTime, sourceName, typeLabel } from "./content";

  let { panel }: { panel: PanelState } = $props();
</script>

<div class="command-host">
  <section class="list-section" aria-label="极简历史列表">
    <div class="list-caption"><span>PINNED / RECENT</span><span>↑↓ 选择　SPACE 预览</span></div>
    <HistoryList {panel} layout="b" />
  </section>
  {#if panel.previewOpen}
    <section class="preview-drawer" aria-label="底部完整预览">
      <div class="preview-head">
        <span>{panel.currentItem ? `${typeLabel(panel.currentItem)} / ${sourceName(panel.currentItem)} / ${relativeTime(panel.currentItem.copied_ms)}` : "NO MATCH"}</span>
        <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewOpen = false; }}>收起预览 <kbd>Space</kbd></button>
      </div>
      <div class="preview-body"><Preview {panel} item={panel.currentItem} compact /></div>
    </section>
  {/if}
</div>

<style>
  .command-host { min-height: 0; display: flex; flex-direction: column; background: var(--panel); }
  .list-section { min-height: 0; flex: 1; display: flex; flex-direction: column; }
  .list-caption { height: 29px; flex: none; display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 0 21px; border-bottom: 1px solid var(--line-soft); color: var(--faint); font: 9px var(--font-mono); letter-spacing: .1em; }
  .list-caption span:last-child { white-space: nowrap; }
  .preview-drawer { height: var(--size-preview-b); flex: none; display: flex; flex-direction: column; border-top: 2px solid var(--text); background: var(--panel); animation: appear var(--fade-duration) linear; }
  .preview-head { height: 31px; flex: none; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 0 21px; color: var(--faint); font: 9px var(--font-mono); letter-spacing: .06em; }
  .preview-head > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .preview-head button { flex: none; padding: 3px 0 3px 8px; border: 0; background: transparent; color: var(--accent); font-size: 10px; }
  kbd { font: 9px var(--font-mono); }
  .preview-body { min-height: 0; flex: 1; overflow: auto; padding: 4px 22px 16px; scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  @media (max-width: 599px) {
    .list-caption { padding: 0 12px; font-size: 8px; letter-spacing: .04em; }
    .preview-head { padding: 0 12px; }
    .preview-body { padding-left: 13px; padding-right: 13px; }
  }
  /* 只在设置里选了“跟随系统”且系统开了“减少动画效果”时生效（data-reduce-motion）。 */
  :global(html[data-reduce-motion="true"]) .preview-drawer { animation-duration: var(--duration-reduce); }
</style>
