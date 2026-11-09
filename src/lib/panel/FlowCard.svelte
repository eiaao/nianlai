<script lang="ts">
  import type { PanelState } from "../state.svelte";
  import type { HistoryItem } from "../ipc";
  import { colorValue, linkParts, relativeTime, sourceName, typeLabel, visualKind } from "./content";
  import SourceIcon from "./SourceIcon.svelte";
  import { leave } from "./motion";

  let { panel, item, index }: { panel: PanelState; item: HistoryItem; index: number } = $props();
  const selected = $derived(index === panel.selected);
  const kind = $derived(visualKind(item));
  const link = $derived(linkParts(item));
  const color = $derived(colorValue(item));
  const excerpt = $derived(kind === "code"
    ? item.preview.split("\n").slice(0, 8).join("\n")
    : [...(item.preview || "（空文本）")].slice(0, kind === "long" || kind === "rich" ? 126 : 84).join(""));
  const hold = $derived(panel.holdDelete?.id === item.id ? panel.holdDelete : null);
</script>

<button
  id={`flow-option-${item.id}`}
  class="flow-card"
  class:selected
  type="button"
  role="option"
  aria-selected={selected}
  tabindex="-1"
  data-flow-index={index}
  out:leave={{ leaving: panel.leavingId === item.id, motion: panel.motion, collapse: false }}
  disabled={panel.busy || panel.awaitingRelease || !panel.interactive}
  onpointerdown={(event) => event.preventDefault()}
  onclick={(event) => panel.choose(index, "paste", event)}
>
  <span class="card-surface" class:link-surface={kind === "link"} class:color-surface={kind === "color" && !!color} class:image-surface={kind === "image"} style={kind === "color" && color ? `background-color: ${color.value}` : undefined}>
    <span class="card-topline"><span>{item.favorite ? "PIN · " : ""}{typeLabel(item).toUpperCase()}{#if item.file_state}<em class="file-tag" class:unknown={item.file_state.state === "unknown"}>{item.file_state.state === "missing" ? "已失效" : item.file_state.state === "partial" ? `${item.file_state.missing} 项失效` : "未确认"}</em>{/if}</span>{#if item.favorite}<svg class="card-fav" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z"/></svg>{/if}</span>

    {#if kind === "link" && link}
      <svg class="link-arrow" viewBox="0 0 40 40" fill="none" aria-hidden="true"><path d="M8 32 32 8M13 8h19v19" stroke="currentColor" stroke-width="1.7"/></svg>
      <span class="link-domain">{link.host}</span>
      <span class="link-path">{link.url.href.slice(link.url.origin.length) || "/"}</span>
    {:else if kind === "color" && color}
      <svg class="color-cross" viewBox="0 0 74 74" fill="none" aria-hidden="true"><path d="M8 66 66 8M20 8h46v46" stroke="currentColor" stroke-width="1.5"/></svg>
      <span class="color-number">{color.value}</span>
      {#if color.rgb}<span class="color-rgb">RGB {color.rgb.join(" / ")}</span>{/if}
    {:else if kind === "image"}
      <span class="image-art" role="img" aria-label={item.preview || "图片记录"}>
        <svg viewBox="0 0 238 266" fill="none" aria-hidden="true"><rect width="238" height="266" fill="var(--c-card)"/><rect x="24" y="48" width="82" height="154" fill="var(--c-text)" opacity=".78"/><circle cx="166" cy="113" r="48" fill="var(--c-accent)" opacity=".78"/><path d="m88 213 91-119 43 119Z" fill="var(--c-star)" opacity=".72"/><path d="M34 66h48M34 78h35" stroke="var(--c-card)" stroke-width="3"/></svg>
      </span>
      <span class="image-caption">{item.preview || "图片记录"}</span>
    {:else if kind === "files"}
      <svg class="file-symbol" viewBox="0 0 62 74" fill="none" aria-hidden="true"><path d="M5 3h32l20 20v48H5V3Z M37 3v20h20M17 39h28M17 50h28M17 61h18" stroke="currentColor" stroke-width="1.5"/></svg>
      <span class="file-names">{#each item.file_names.slice(0, 3) as name}<span>{name}</span>{:else}<span>文件名称不可用</span>{/each}</span>
      <span class="file-size">{item.file_names.length} FILES / LOCAL REF</span>
    {:else}
      <span class="card-excerpt" class:long={kind === "long" || kind === "rich"} class:code={kind === "code"}>{excerpt}</span>
    {/if}

    {#if hold}
      <!-- 删除收藏要按住：卡片底色从左往右填满即移到回收站；提前松开换成提示。 -->
      {#key hold.startedAt}
        {#if hold.phase === "holding"}<i class="hold-fill" aria-hidden="true" style={`animation-duration: ${hold.duration}ms`}></i>{/if}
        <span class="hold-tip" class:hint={hold.phase === "hint"} role="status" aria-live="polite">{#if hold.phase === "holding"}继续按住 <kbd>{hold.keys}</kbd>，删除收藏{:else}收藏的记录要按住 <kbd>{hold.keys}</kbd> 才删除{/if}</span>
      {/key}
    {/if}
    <span class="card-index">{panel.slotLabel(index) ?? (index < panel.favoriteCount ? "PIN" : "—")} / FRAME</span>
    <span class="card-source"><SourceIcon {item} /><span>{sourceName(item)} · {relativeTime(item.copied_ms)}</span></span>
  </span>
</button>

<style>
  .flow-card { position: absolute; left: 50%; top: 50%; width: var(--size-card-width); height: var(--size-card-height); margin: -133px 0 0 -119px; padding: 0; border: 0; border-radius: 0; background: transparent; color: var(--c-card-text); text-align: left; cursor: default; opacity: 0; will-change: transform, opacity; }
  .card-surface { position: relative; width: 100%; height: 100%; display: block; overflow: hidden; border: 1px solid var(--c-line); border-radius: var(--radius-xs); background: var(--c-card); }
  .flow-card.selected .card-surface { border: 2px solid var(--c-accent); }
  .card-topline { position: absolute; z-index: 3; left: 14px; right: 13px; top: 13px; display: flex; align-items: center; justify-content: space-between; gap: 8px; color: var(--c-card-muted); font: 9px var(--font-mono); letter-spacing: .06em; }
  .card-topline > span:first-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .card-fav { width: 16px; height: 16px; flex: none; color: var(--c-star); }
  .card-index { position: absolute; z-index: 3; left: 14px; bottom: 12px; color: var(--c-card-muted); font: 10px var(--font-mono); letter-spacing: .06em; }
  .card-source { position: absolute; z-index: 3; right: 12px; bottom: 10px; max-width: 142px; display: flex; align-items: center; justify-content: flex-end; gap: 5px; color: var(--c-card-muted); font: 9px var(--font-mono); }
  .card-source > span:last-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .card-source :global(.source-icon) { width: 16px; height: 16px; border-radius: var(--radius-xs); }
  .card-source :global(.fallback) { font-size: 8px; }
  .card-excerpt { position: absolute; left: 19px; right: 19px; top: 54px; bottom: 43px; display: block; overflow: hidden; font: 600 23px/1.38 var(--font-display); letter-spacing: -.025em; overflow-wrap: anywhere; white-space: pre-wrap; }
  .card-excerpt.long { font-size: 17px; font-weight: 500; line-height: 1.55; }
  .card-excerpt.code { font: 12px/1.72 var(--font-mono); letter-spacing: 0; white-space: pre; }
  .link-surface { background: var(--c-accent); color: var(--c-accent-ink); }
  .link-surface .card-topline, .link-surface .card-index, .link-surface .card-source { color: inherit; }
  .link-arrow { position: absolute; right: 18px; top: 44px; width: 37px; height: 37px; }
  .link-domain { position: absolute; left: 18px; right: 18px; top: 85px; font: 650 29px/1.1 var(--font-display); letter-spacing: -.04em; overflow-wrap: anywhere; }
  .link-path { position: absolute; left: 19px; right: 19px; top: 139px; max-height: 68px; overflow: hidden; font: 11px/1.6 var(--font-mono); overflow-wrap: anywhere; white-space: pre-wrap; }
  .color-cross { position: absolute; width: 74px; height: 74px; right: 24px; top: 65px; }
  .color-number { position: absolute; left: 18px; right: 14px; bottom: 56px; overflow: hidden; font: 600 24px var(--font-mono); letter-spacing: -.04em; text-overflow: ellipsis; white-space: nowrap; }
  .color-rgb { position: absolute; left: 19px; bottom: 38px; font: 9px var(--font-mono); }
  .image-art { position: absolute; inset: 0; display: block; }
  .image-art svg { width: 100%; height: 100%; display: block; }
  .image-caption { position: absolute; z-index: 2; left: 0; right: 0; bottom: 38px; height: 34px; display: flex; align-items: center; padding: 0 14px; overflow: hidden; background: var(--c-raised); color: var(--c-text); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
  .file-symbol { position: absolute; left: 18px; top: 65px; width: 62px; height: 74px; color: var(--c-accent); }
  .file-names { position: absolute; left: 94px; right: 15px; top: 70px; display: grid; font: 12px/2 var(--font-ui); }
  .file-names > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .file-size { position: absolute; left: 19px; bottom: 53px; color: var(--c-card-muted); font: 10px var(--font-mono); }
  .file-tag { margin-left: 6px; padding: 0 4px; border: 1px solid currentColor; border-radius: var(--radius-xs); color: var(--state-caution); font: 600 9px/13px var(--font-ui); font-style: normal; letter-spacing: 0; }
  .file-tag.unknown { border-style: dashed; color: var(--c-card-muted); }
  .hold-fill { position: absolute; inset: 0; z-index: 2; background: color-mix(in srgb, var(--c-star) 18%, transparent); transform-origin: left; pointer-events: none; animation: hold-fill linear forwards; }
  .hold-tip { position: absolute; z-index: 4; left: 14px; right: 14px; bottom: 34px; display: flex; align-items: center; justify-content: center; gap: 4px; padding: 5px 8px; border-radius: var(--radius-xs); background: var(--c-raised); box-shadow: 0 0 0 1px color-mix(in srgb, var(--c-star) 45%, transparent); color: var(--c-star); font: 11px var(--font-ui); white-space: nowrap; animation: hold-tip var(--fade-duration) linear; }
  .hold-tip.hint { box-shadow: 0 0 0 1px var(--c-line); color: var(--c-card-muted); }
  .hold-tip kbd { color: inherit; font: 600 10px/1.4 var(--font-mono); }
  @keyframes hold-fill { from { transform: scaleX(0); } to { transform: scaleX(1); } }
  @keyframes hold-tip { from { opacity: 0; } to { opacity: 1; } }
  @media (max-width: 520px) {
    .card-source { max-width: 124px; }
    .card-source :global(.source-icon) { display: none; }
  }
</style>
