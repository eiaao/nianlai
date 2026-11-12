<script lang="ts">
  import type { PanelState } from "../state.svelte";
  import Preview from "./Preview.svelte";
  import { cardKind, KIND_NAME } from "../cards";

  let { panel }: { panel: PanelState } = $props();
  const kind = $derived(panel.currentItem ? cardKind(panel.currentItem) : null);
  const label = $derived.by(() => {
    const item = panel.currentItem;
    if (!item || !kind) return "无结果";
    const chars = panel.fullChars ?? [...(item.preview ?? "")].length;
    if (kind === "file") return `文件 · ${item.file_names.length} 个`;
    if (kind === "image") return "图片";
    return `${item.title || KIND_NAME[kind]} · ${chars.toLocaleString("zh-CN")} 字`;
  });
</script>

{#if panel.previewOpen}
  <div class="preview-overlay" data-hit role="dialog" tabindex="-1" aria-label="看全">
    <header>
      <span class="kind">{label}</span>
      {#if kind === "image"}
        <span class="zoom" role="group" aria-label="缩放">
          <button type="button" tabindex="-1" class:on={panel.previewZoom === "fit"} onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewZoom = "fit"; }}>适应</button>
          <button type="button" tabindex="-1" class:on={panel.previewZoom === "original"} onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewZoom = "original"; }}>原图</button>
        </span>
      {/if}
      <button class="close" type="button" tabindex="-1" aria-label="合上看全" title="合上（Tab / Esc）" onpointerdown={(event) => event.preventDefault()} onclick={() => { panel.previewOpen = false; }}>×</button>
    </header>
    <div class="overlay-scroll"><Preview {panel} item={panel.currentItem} expanded /></div>
    <footer>
      <span><kbd>←</kbd><kbd>→</kbd> 换一张</span><span><kbd>↑</kbd><kbd>↓</kbd> 滚动</span>
      {#if kind === "image"}<span><kbd>Z</kbd> {panel.previewZoom === "fit" ? "查看原图" : "适应窗口"}</span>{/if}
      {#if kind && kind !== "image"}<span>选中文字后 <kbd>Ctrl+C</kbd> 复制</span>{/if}
      <span><kbd>↵</kbd> 粘贴</span><span><kbd>Tab</kbd> 合上</span>
    </footer>
  </div>
{/if}

<style>
  .preview-overlay { position: absolute; z-index: 200; left: 50%; top: 38px; width: 440px; height: 440px; display: flex; flex-direction: column; min-width: 0; min-height: 0; transform: translateX(-50%); overflow: hidden; border-radius: 12px; background: var(--hand-card); color: var(--hand-ink); box-shadow: var(--hand-lift); animation: appear 160ms ease-out; }
  header { height: 44px; flex: none; display: flex; align-items: center; gap: 8px; padding: 0 10px 0 16px; border-bottom: 1px solid var(--hand-line); }
  .kind { min-width: 0; overflow: hidden; color: var(--hand-ink-3); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
  .zoom { display: inline-flex; margin-left: auto; overflow: hidden; border-radius: 5px; box-shadow: inset 0 0 0 1px var(--hand-line); }
  .zoom button { height: 24px; padding: 0 8px; border: 0; background: transparent; color: var(--hand-ink-2); font-size: 12px; }
  .zoom button.on { background: var(--hand-card-back); color: var(--hand-ink); }
  .close { width: 28px; height: 28px; margin-left: auto; border: 0; background: transparent; color: var(--hand-ink-3); font-size: 20px; }
  .zoom + .close { margin-left: 0; }
  .overlay-scroll { flex: 1; min-height: 0; overflow: auto; padding: 16px 20px; scrollbar-width: thin; scrollbar-color: var(--hand-line) transparent; }
  footer { min-height: 36px; flex: none; display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; padding: 6px 16px; border-top: 1px solid var(--hand-line); color: var(--hand-ink-2); font-size: 12px; }
  footer span { display: flex; align-items: center; gap: 4px; white-space: nowrap; }
  kbd { color: var(--hand-ink); font: 600 12px/1.3 var(--font-ui); }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  :global(html[data-motion="none"]) .preview-overlay, :global(html[data-reduce-motion="true"]) .preview-overlay { animation-duration: 100ms; }
</style>
