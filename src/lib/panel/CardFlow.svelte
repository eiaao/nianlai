<script lang="ts">
  // C：只挂载选中项前后各两张卡；轨道与 RECENT 分隔线共享同一弹簧位置。
  import { onDestroy, tick, untrack } from "svelte";
  import type { PanelState } from "../state.svelte";
  import FlowCard from "./FlowCard.svelte";
  import PreviewOverlay from "./PreviewOverlay.svelte";
  import { relativeTime, sourceName, typeLabel } from "./content";
  import { SpringValue } from "./motion";

  let { panel }: { panel: PanelState } = $props();
  let viewport!: HTMLDivElement;
  let cards!: HTMLDivElement;
  let divider!: HTMLDivElement;
  let destroyed = false;
  let previousQuery: string | undefined;
  const STEP = 218;
  const rendered = $derived(panel.visibleItems
    .map((item, index) => ({ item, index }))
    .filter(({ index }) => Math.abs(index - panel.selected) <= 2));

  function renderTrack(position: number) {
    if (!cards) return;
    cards.querySelectorAll<HTMLElement>("[data-flow-index]").forEach((card) => {
      const index = Number(card.dataset.flowIndex);
      const relative = index - position;
      const distance = Math.abs(relative);
      const x = relative * STEP;
      const scale = distance <= 1 ? 1 - distance * .2 : .8 - Math.min(.08, (distance - 1) * .08);
      const opacity = distance > 2.4 ? 0 : distance <= 1 ? 1 - distance * .42 : Math.max(0, .58 - (distance - 1) * .34);
      card.style.transform = `translate3d(${x}px, 0, 0) scale(${scale})`;
      card.style.opacity = String(opacity);
      card.style.zIndex = String(100 - Math.round(distance * 10));
      card.style.pointerEvents = distance > 1.45 ? "none" : "auto";
    });
    if (!divider || !viewport) return;
    const favorites = panel.favoriteCount;
    const dx = (favorites - .5 - position) * STEP;
    const half = viewport.clientWidth / 2;
    divider.style.transform = `translate3d(${dx}px, 0, 0)`;
    // 线或标签到视口外就整体隐藏；在两侧时标签始终朝视口内延伸，不盖住外侧卡片。
    divider.hidden = !(favorites > 0 && favorites < panel.visibleItems.length && Math.abs(dx) < half - 4);
    const label = divider.querySelector<HTMLElement>("span");
    if (label) {
      label.style.left = dx > 0 ? "auto" : "7px";
      label.style.right = dx > 0 ? "7px" : "auto";
    }
  }

  const track = new SpringValue(untrack(() => panel.selected), renderTrack, () => panel.motion);

  $effect(() => {
    const target = panel.selected;
    const query = panel.query;
    const signature = panel.visibleItems.map((item) => item.id).join(",");
    const favorites = panel.favoriteCount;
    panel.motion;
    const reset = previousQuery === undefined || query !== previousQuery;
    previousQuery = query;
    void signature;
    void favorites;
    void tick().then(() => {
      if (destroyed) return;
      if (reset) track.reset(target);
      else track.to(target, panel.motion === "spring" ? .8 : 1);
    });
  });

  onDestroy(() => { destroyed = true; track.destroy(); });
</script>

<div class="flow-host">
  <div class="flow-viewport" bind:this={viewport}>
    <div class="flow-axis" aria-hidden="true"></div>
    <div class="flow-cards" bind:this={cards} role="listbox" tabindex="-1" aria-label="横向剪贴板历史" aria-busy={panel.busy || panel.favoritePending !== null} aria-activedescendant={panel.currentItem ? `flow-option-${panel.currentItem.id}` : undefined}>
      {#each rendered as entry (entry.item.id)}
        <FlowCard {panel} item={entry.item} index={entry.index} />
      {:else}
        <div class="flow-empty"><strong>{panel.query ? `没有找到“${panel.query}”` : "还没有记录"}</strong><span>{panel.query ? "试试更短的关键词；Esc 返回完整历史。" : "复制文字、图片或文件，它们会按时间出现在这里。"}</span></div>
      {/each}
    </div>
    <div class="track-divider" bind:this={divider} hidden aria-hidden="true"><span>RECENT / 最近</span></div>
  </div>

  <section class="info-strip" aria-label="选中项信息">
    <div class="info-copy">
      <div class="info-title">{panel.currentItem?.preview.replace(/\n/g, " · ") || "没有匹配内容"}</div>
      <div class="info-meta">{panel.currentItem ? `${typeLabel(panel.currentItem)} / ${sourceName(panel.currentItem)} / ${relativeTime(panel.currentItem.copied_ms)}` : "NO MATCH"}</div>
    </div>
    <div class="info-actions">
      <button class="c-icon-button" type="button" tabindex="-1" title="完整预览（Space）" aria-label="完整预览" disabled={!panel.currentItem || !panel.interactive || panel.mode !== "quick"} onpointerdown={(event) => event.preventDefault()} onclick={panel.togglePreview}>
        <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M3 7V3h4M13 3h4v4M17 13v4h-4M7 17H3v-4" stroke="currentColor" stroke-width="1.4"/></svg>
      </button>
      <button class="c-icon-button" class:active={panel.currentItem?.favorite} type="button" tabindex="-1" title="收藏 / 取消收藏" aria-label={panel.currentItem?.favorite ? "取消收藏" : "收藏"} aria-pressed={!!panel.currentItem?.favorite} disabled={!panel.currentItem || !panel.interactive || panel.favoritePending !== null} onpointerdown={(event) => event.preventDefault()} onclick={() => { if (panel.currentItem) void panel.toggleFavorite(panel.currentItem.id); }}>
        <svg viewBox="0 0 20 20" fill={panel.currentItem?.favorite ? "currentColor" : "none"} aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z" stroke="currentColor" stroke-width="1.3"/></svg>
      </button>
    </div>
  </section>
  <PreviewOverlay {panel} compact />
</div>

<style>
  .flow-host { display: contents; }
  .flow-viewport { min-height: 0; position: relative; overflow: hidden; background: var(--c-stage); }
  .flow-viewport::before, .flow-viewport::after { content: "+"; position: absolute; z-index: 150; top: 10px; color: var(--c-faint); font: 14px var(--font-mono); pointer-events: none; }
  .flow-viewport::before { left: 13px; }
  .flow-viewport::after { right: 13px; }
  .flow-axis { position: absolute; left: 18px; right: 18px; top: 50%; height: 1px; background: var(--c-hairline); pointer-events: none; }
  .flow-cards { position: absolute; inset: 0; }
  .flow-empty { position: absolute; inset: 0; display: grid; align-content: center; justify-items: center; gap: 8px; padding: 24px; color: var(--c-muted); font-size: 13px; line-height: 1.8; text-align: center; }
  .flow-empty strong { max-width: 100%; color: var(--c-text); overflow-wrap: anywhere; }
  .track-divider { position: absolute; z-index: 120; left: 50%; top: 12px; width: 1px; height: 26px; background: var(--c-accent); color: var(--c-accent); pointer-events: none; will-change: transform; }
  .track-divider span { position: absolute; top: -2px; font: 9px var(--font-mono); letter-spacing: .1em; white-space: nowrap; }
  .info-strip { min-width: 0; display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 14px; padding: 0 15px 0 18px; border-top: 1px solid var(--c-line); border-bottom: 1px solid var(--c-line); background: var(--c-bg); }
  .info-copy { min-width: 0; }
  .info-title { overflow: hidden; color: var(--c-text); font-size: 13px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
  .info-meta { margin-top: 5px; overflow: hidden; color: var(--c-faint); font: 9px var(--font-mono); letter-spacing: .04em; text-overflow: ellipsis; white-space: nowrap; }
  .info-actions { display: flex; gap: 4px; }
  .c-icon-button { width: 34px; height: 34px; display: grid; place-items: center; border: 1px solid var(--c-line); border-radius: var(--radius-xs); background: transparent; color: var(--c-muted); }
  .c-icon-button:hover:not(:disabled) { border-color: var(--c-muted); color: var(--c-text); }
  .c-icon-button.active { border-color: var(--c-star); color: var(--c-star); }
  .c-icon-button:disabled { opacity: .45; }
  .c-icon-button svg { width: 17px; height: 17px; }
  @media (max-width: 520px) {
    .info-strip { gap: 8px; padding-left: 12px; padding-right: 10px; }
    .info-meta { font-size: 8px; }
  }
</style>
