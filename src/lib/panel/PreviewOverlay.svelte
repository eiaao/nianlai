<script lang="ts">
  import type { PanelState } from "../state.svelte";
  import Preview from "./Preview.svelte";
  import { typeLabel } from "./content";

  let { panel, compact = false }: { panel: PanelState; compact?: boolean } = $props();
  // 类型标签里的字数来自列表摘要（最多 300 字）；取到全文后换成全文字数。
  const fullLabel = (label: string) => panel.fullChars === null ? label : label.replace(/[\d,]+ 字/, `${panel.fullChars.toLocaleString("zh-CN")} 字`);
</script>

{#if panel.previewOpen}
  <div class="preview-overlay" class:detail-overlay={compact} role="dialog" tabindex="-1" aria-label="完整预览">
    <header>
      <span class="position">FULL / {String(panel.selected + 1).padStart(2, "0")}</span>
      <span class="kind">{panel.currentItem ? fullLabel(typeLabel(panel.currentItem)) : "无结果"}</span>
      {#if panel.currentItem?.kind === "图片" && !compact}
        <span class="zoom" role="group" aria-label="缩放">
          <button type="button" tabindex="-1" class:on={panel.previewZoom === "fit"} onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewZoom = "fit"; }}>适应</button>
          <button type="button" tabindex="-1" class:on={panel.previewZoom === "original"} onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewZoom = "original"; }}>原尺寸</button>
        </span>
      {/if}
      <button type="button" tabindex="-1" aria-label="关闭完整预览" title="关闭（Space / Esc）" onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewOpen = false; }}>
        <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="m5 5 10 10M15 5 5 15" stroke="currentColor" stroke-width="1.4"/></svg>
      </button>
    </header>
    <div class="overlay-scroll"><Preview {panel} item={panel.currentItem} expanded={!compact} compact /></div>
    {#if !compact}<footer><span>{#if panel.currentItem?.kind === "图片"}<kbd>Z</kbd> 适应 / 原尺寸 · 原尺寸时 <kbd>↑↓←→</kbd> 或拖动平移{:else if panel.currentItem?.kind === "文件"}<kbd>↑↓</kbd> 滚动 · 文件只记了路径{:else}<kbd>↑↓</kbd> <kbd>PgUp/PgDn</kbd> 滚动 · 选中文字后 <kbd>Ctrl+C</kbd> 只复制选中部分{/if}</span><span><kbd>↵</kbd> 粘贴整条　<kbd>Space</kbd> / <kbd>Esc</kbd> 返回</span></footer>{/if}
  </div>
{/if}

<style>
  .preview-overlay { position: absolute; z-index: 40; inset: 10px; display: flex; flex-direction: column; min-width: 0; min-height: 0; border: 1px solid var(--accent); border-radius: var(--radius-md); background: var(--preview); color: var(--text); animation: appear var(--fade-duration) linear; }
  header { height: 40px; flex: none; display: flex; align-items: center; gap: 10px; padding: 0 8px 0 14px; border-bottom: 1px solid var(--line-soft); }
  .position { color: var(--accent); font: 10px var(--font-mono); white-space: nowrap; }
  .kind { min-width: 0; overflow: hidden; color: var(--muted); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
  .zoom { display: inline-flex; margin-left: auto; overflow: hidden; border: 1px solid var(--line); border-radius: var(--radius-sm); }
  .zoom button { width: auto; height: 22px; margin: 0; padding: 0 9px; border: 0; border-radius: 0; color: var(--muted); font-size: 10px; }
  .zoom button.on { background: var(--selected); color: var(--text); font-weight: 600; }
  .zoom + button { margin-left: 4px; }
  button { width: 30px; height: 30px; display: grid; place-items: center; margin-left: auto; border: 1px solid transparent; border-radius: var(--radius-sm); background: transparent; color: var(--muted); }
  button:hover { border-color: var(--line); color: var(--text); }
  button svg { width: 17px; height: 17px; }
  .overlay-scroll { flex: 1; min-height: 0; overflow: auto; padding: 22px 26px; scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  footer { min-height: 30px; flex: none; display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); padding: 5px 12px; border-top: 1px solid var(--line-soft); color: var(--faint); font-size: 9px; }
  footer span:last-child { white-space: nowrap; }
  kbd { color: var(--text); font: 9px var(--font-mono); }
  .preview-overlay.detail-overlay { z-index: 220; left: 18px; right: 18px; top: auto; bottom: 45px; height: var(--size-detail-c); border-radius: var(--radius-xs); background: var(--panel-raised); }
  .detail-overlay header { height: 31px; padding: 0 12px; }
  .detail-overlay .position, .detail-overlay .kind { color: var(--faint); font: 9px var(--font-mono); }
  .detail-overlay .overlay-scroll { padding: 10px 13px; }
  .detail-overlay button { width: auto; height: 26px; padding: 0 0 0 8px; color: var(--accent); font-size: 10px; }
  .detail-overlay button svg { display: none; }
  .detail-overlay button::after { content: "收起 Space / Esc"; }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  @media (max-width: 599px) {
    .preview-overlay { inset: 6px; }
    .overlay-scroll { padding: var(--space-4); }
    footer span:first-child { display: none; }
    footer { justify-content: flex-end; }
  }
  /* 只在设置里选了“跟随系统”且系统开了“减少动画效果”时生效（data-reduce-motion）。 */
  :global(html[data-reduce-motion="true"]) .preview-overlay { animation-duration: var(--duration-reduce); }
</style>
