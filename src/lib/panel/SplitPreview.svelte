<script lang="ts">
  import type { PanelState } from "../state.svelte";
  import HistoryList from "./HistoryList.svelte";
  import Preview from "./Preview.svelte";
  import PreviewOverlay from "./PreviewOverlay.svelte";
  import { typeLabel } from "./content";

  let { panel }: { panel: PanelState } = $props();
</script>

<div class="split-host">
<div class="workspace">
  <aside class="index-pane" aria-label="历史索引">
    <div class="index-heading"><span>HISTORY / INDEX</span><span>↑↓</span></div>
    <HistoryList {panel} />
  </aside>
  <section class="preview-pane" aria-label="常驻预览">
    <div class="preview-toolbar">
      <span class="preview-position">FRAME {panel.visibleItems.length ? String(panel.selected + 1).padStart(2, "0") : "00"} / {String(panel.visibleItems.length).padStart(2, "0")}</span>
      <span class="preview-kind">{panel.currentItem ? typeLabel(panel.currentItem) : "无结果"}</span>
      <div class="preview-actions">
        <button class="icon-button" type="button" tabindex="-1" title="完整预览（Space）" aria-label="完整预览" disabled={!panel.currentItem || !panel.interactive || panel.mode !== "quick"} onpointerdown={(event) => event.preventDefault()} onclick={panel.togglePreview}>
          <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M3 7V3h4M13 3h4v4M17 13v4h-4M7 17H3v-4" stroke="currentColor" stroke-width="1.4"/></svg>
        </button>
        <button class="icon-button" class:active={panel.currentItem?.favorite} type="button" tabindex="-1" title="收藏 / 取消收藏" aria-label={panel.currentItem?.favorite ? "取消收藏" : "收藏"} aria-pressed={!!panel.currentItem?.favorite} disabled={!panel.currentItem || !panel.interactive || panel.favoritePending !== null} onpointerdown={(event) => event.preventDefault()} onclick={() => { if (panel.currentItem) void panel.toggleFavorite(panel.currentItem.id); }}>
          <svg viewBox="0 0 20 20" fill={panel.currentItem?.favorite ? "currentColor" : "none"} aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z" stroke="currentColor" stroke-width="1.3"/></svg>
        </button>
      </div>
    </div>
    <div class="preview-scroll"><Preview {panel} item={panel.currentItem} /></div>
  </section>
</div>
<PreviewOverlay {panel} />
</div>

<style>
  .split-host { position: relative; min-height: 0; display: flex; }
  .workspace { flex: 1; min-height: 0; display: grid; grid-template-columns: var(--size-a-index) minmax(0, 1fr); }
  .index-pane { min-width: 0; min-height: 0; display: flex; flex-direction: column; border-right: 1px solid var(--line); }
  .index-heading { height: 34px; flex: none; display: flex; align-items: center; justify-content: space-between; padding: 0 13px; border-bottom: 1px solid var(--line-soft); color: var(--faint); font: 10px var(--font-mono); letter-spacing: .09em; }
  .preview-pane { min-width: 0; min-height: 0; display: flex; flex-direction: column; background: var(--preview); }
  .preview-toolbar { height: 40px; flex: none; display: flex; align-items: center; gap: 10px; padding: 0 8px 0 17px; border-bottom: 1px solid var(--line-soft); }
  .preview-position { color: var(--accent); font: 10px var(--font-mono); white-space: nowrap; }
  .preview-kind { min-width: 0; overflow: hidden; color: var(--muted); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
  .preview-actions { margin-left: auto; display: flex; gap: 2px; }
  .icon-button { width: 29px; height: 28px; display: grid; place-items: center; flex: none; border: 1px solid transparent; border-radius: var(--radius-sm); background: transparent; color: var(--muted); }
  .icon-button:hover:not(:disabled) { border-color: var(--line); color: var(--text); }
  .icon-button.active { color: var(--star); }
  .icon-button:disabled { opacity: .45; }
  .icon-button svg { width: 17px; height: 17px; }
  .preview-scroll { min-height: 0; flex: 1; overflow: auto; padding: 23px 25px; scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  @media (max-width: 599px) {
    .workspace { grid-template-columns: var(--size-a-index-narrow) minmax(0, 1fr); }
    .index-heading { padding: 0 9px; font-size: 8px; }
    .preview-toolbar { gap: 5px; padding-left: 9px; }
    .preview-position { font-size: 8px; }
    .preview-kind { display: none; }
    .preview-scroll { padding: var(--space-4) var(--space-3); }
  }
</style>
