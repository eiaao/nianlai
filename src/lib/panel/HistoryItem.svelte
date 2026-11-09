<script lang="ts">
  // A/B 共用条目语义与鼠标取用；B 只改排印、行高和槽位显隐，不复制数据或选中状态。
  import { formatTime, type PanelState } from "../state.svelte";
  import type { HistoryItem } from "../ipc";
  import { linkParts, relativeTime, sourceName, typeLabel, visualKind } from "./content";
  import SourceIcon from "./SourceIcon.svelte";
  import { leave } from "./motion";

  let { panel, item, index, layout = "a" }: { panel: PanelState; item: HistoryItem; index: number; layout?: "a" | "b" } = $props();
  const selected = $derived(index === panel.selected);
  const kind = $derived(visualKind(item));
  const link = $derived(linkParts(item));
  const hold = $derived(panel.holdDelete?.id === item.id ? panel.holdDelete : null);
  // M2-3：文件失效标记。
  const mark = $derived(item.file_state ?? null);
  // M2-6：收藏有标题时第一行显示标题，原内容退到第二行。
  const titled = $derived(item.favorite && !!item.title);
  const originalLine = $derived(kind === "image" || kind === "files" ? typeLabel(item) : (item.preview || "（空文本）").replace(/\s+/g, " ").trim());
</script>

<li class:history-row={layout === "a"} class:command-row={layout === "b"} class:selected class:holding={hold?.phase === "holding"} class:file-missing={mark?.state === "missing"} data-history-index={index}
  out:leave={{ leaving: panel.leavingId === item.id, motion: panel.motion, collapse: true }}
  onoutroend={() => { panel.layoutRevision += 1; }}>
  {#if hold?.phase === "holding"}
    <!-- 删除收藏要按住：底色从左往右填满即移到回收站；说明文字换掉第二行，不遮标题。 -->
    {#key hold.startedAt}<i class="hold-fill" aria-hidden="true" style={`animation-duration: ${hold.duration}ms`}></i>{/key}
  {/if}
  <button
    id={`history-option-${item.id}`}
    class="row-select"
    type="button"
    role="option"
    aria-selected={selected}
    tabindex="-1"
    disabled={panel.busy || panel.awaitingRelease || !panel.interactive}
    onpointerdown={(event) => event.preventDefault()}
    onclick={(event) => panel.chooseByMouse(index, event)}
    onmousedown={(event) => { if (event.button === 1) event.preventDefault(); }}
    onauxclick={(event) => { if (event.button === 1) { event.preventDefault(); panel.chooseByMouse(index, event); } }}
  >
    <span class="slot" aria-hidden="true">{panel.slotLabel(index) ?? (index < panel.favoriteCount ? "" : "·")}</span>
    <SourceIcon {item} />
    <span class="row-copy">
      <span class="row-main" class:code={kind === "code" && !titled}>
        {#if mark?.state === "missing"}<span class="file-tag gone">已失效</span>{:else if mark?.state === "unknown"}<span class="file-tag unknown">未确认</span>{/if}
        {#if titled}
          <span class="label-title">{#each panel.highlights(item.title ?? "") as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
        {:else if kind === "link" && link}
          <span class="domain">{#each panel.highlights(link.host) as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span><span>{#each panel.highlights(link.url.href.slice(link.url.origin.length)) as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
        {:else}
          {#each panel.highlights(item.preview || "（空文本）") as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
        {/if}
      </span>
      {#if hold}
        <span class="row-sub hold-sub" class:hint={hold.phase === "hint"} role="status" aria-live="polite">{#if hold.phase === "holding"}继续按住，删除收藏{:else}要按住 <kbd>{hold.keys}</kbd> 才删除{/if}</span>
      {:else}
        {#if mark?.state === "missing"}
          <span class="row-sub">原文件已移走或删除</span>
        {:else if mark?.state === "unknown"}
          <span class="row-sub">网络位置暂时连不上</span>
        {:else if titled}
          <span class="row-sub original">{#each panel.highlights(originalLine) as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
        {:else}
        <span class="row-sub">{#if mark?.state === "partial"}<span class="file-tag gone">{mark.missing} 项失效</span>{/if}{sourceName(item)} · {typeLabel(item)}{item.copy_count > 1 ? ` · ${item.copy_count} 次` : ""}{item.partial ? " · 部分格式" : ""}</span>
        {/if}
      {/if}
    </span>
  </button>
  <button
    class="row-star"
    class:favorite={item.favorite}
    type="button"
    tabindex="-1"
    aria-label={item.favorite ? "取消收藏" : "收藏"}
    aria-pressed={item.favorite}
    disabled={!panel.interactive || panel.busy || panel.favoritePending !== null}
    onpointerdown={(event) => { event.preventDefault(); event.stopPropagation(); }}
    onclick={(event) => { event.stopPropagation(); void panel.toggleFavorite(item.id); }}
  >
    <svg viewBox="0 0 20 20" fill={item.favorite ? "currentColor" : "none"} aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z" stroke="currentColor" stroke-width="1.3"/></svg>
  </button>
  <span class="row-time" title={formatTime(item.copied_ms)}>{relativeTime(item.copied_ms)}</span>
</li>

<style>
  .history-row, .command-row { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) 20px 34px; align-items: center; gap: 7px; margin: 0; padding: 0 9px 0 0; border-bottom: 1px solid var(--line-soft); background: transparent; color: var(--muted); }
  .history-row { height: var(--size-row-a); }
  .history-row.selected, .command-row.selected { background: var(--selected); color: var(--text); }
  /* 悬停只给视觉提示，不改选中项：Enter 永远作用在键盘选中的那一条。 */
  .history-row:not(.selected):hover, .command-row:not(.selected):hover { background: color-mix(in srgb, var(--selected) 45%, transparent); }
  .row-select { min-width: 0; height: 100%; display: grid; grid-template-columns: 22px 30px minmax(0, 1fr); align-items: center; gap: 7px; padding: 0 0 0 10px; border: 0; border-radius: 0; background: transparent; color: inherit; text-align: left; cursor: default; }
  .slot { color: var(--faint); font: 11px var(--font-mono); }
  .selected .slot { color: var(--accent); }
  .row-copy { min-width: 0; display: grid; gap: 5px; }
  .row-main { overflow: hidden; color: var(--text); font-size: 12px; font-weight: 550; line-height: 1.25; text-overflow: ellipsis; white-space: nowrap; }
  .row-main.code { font-family: var(--font-mono); font-size: 11px; }
  .row-main .domain { font-weight: 650; }
  /* 有标题的收藏：第二行是原内容，比来源说明稍醒目。 */
  .row-sub.original { color: var(--muted); font-size: 10px; }
  .row-sub { overflow: hidden; color: var(--faint); font: 9px/1.25 var(--font-ui); text-overflow: ellipsis; white-space: nowrap; }
  .selected .row-sub, .selected .row-time { color: var(--muted); }
  .row-star { width: 20px; height: 24px; display: grid; place-items: center; padding: 0; border: 0; background: transparent; color: var(--star); opacity: 0; }
  .row-star svg { width: 18px; height: 18px; }
  .row-star.favorite, .history-row:hover .row-star, .history-row.selected .row-star, .command-row:hover .row-star, .command-row.selected .row-star, .row-star:focus-visible { opacity: 1; }
  .row-star:disabled { cursor: default; }
  .row-time { align-self: start; padding-top: 14px; overflow: hidden; color: var(--faint); font: 9px var(--font-mono); text-align: right; white-space: nowrap; }
  li { position: relative; isolation: isolate; }
  .file-tag { display: inline-block; margin-right: 5px; padding: 0 4px; border: 1px solid currentColor; border-radius: var(--radius-sm); font: 600 9px/14px var(--font-ui); vertical-align: 1px; }
  .file-tag.gone { color: var(--state-caution); }
  .file-tag.unknown { color: var(--faint); border-style: dashed; }
  .row-sub .file-tag { padding: 0 3px; font-size: 8px; line-height: 12px; }
  .file-missing .row-main { color: var(--muted); }
  .hold-fill { position: absolute; inset: 0; z-index: -1; background: color-mix(in srgb, var(--star) 14%, transparent); transform-origin: left; pointer-events: none; animation: hold-fill linear forwards; }
  .hold-sub, .selected .hold-sub { color: var(--star); animation: hold-sub var(--fade-duration) linear; }
  .hold-sub.hint, .selected .hold-sub.hint { color: var(--muted); }
  .hold-sub kbd { color: inherit; font: 600 9px/1.25 var(--font-mono); }
  @keyframes hold-sub { from { opacity: 0; } to { opacity: 1; } }
  .holding .row-star { animation: hold-star 220ms ease-in-out infinite alternate; }
  @keyframes hold-fill { from { transform: scaleX(0); } to { transform: scaleX(1); } }
  @keyframes hold-star { from { transform: rotate(-8deg); } to { transform: rotate(8deg); } }
  mark { background: transparent; color: var(--accent); text-decoration: underline; text-underline-offset: 3px; }

  .command-row { height: var(--size-row-b); grid-template-columns: minmax(0, 1fr) 18px 38px; gap: 8px; padding: 0 18px 0 15px; }
  .command-row .row-select { grid-template-columns: 29px 24px minmax(0, 1fr); gap: 8px; padding: 0; }
  .command-row .row-select :global(.source-icon) { width: 24px; height: 24px; }
  .command-row .slot { color: var(--accent); opacity: 0; transform: translateX(-4px); transition: opacity var(--fade-duration) linear, transform var(--fade-duration) linear; }
  .command-row.selected .slot, :global(.panel-b.ctrl-down) .command-row .slot { opacity: 1; transform: translateX(0); }
  .command-row .row-main { color: inherit; font-size: 14px; line-height: 1.3; letter-spacing: .005em; }
  .command-row.selected .row-main { font-weight: 650; letter-spacing: -.005em; }
  .command-row .row-main.code { font-size: 12px; }
  .command-row .row-main .domain { color: var(--text); }
  .command-row .row-star { width: 18px; height: 20px; }
  .command-row .row-star svg { width: 16px; height: 16px; }
  .command-row .row-time { padding-top: 10px; font-size: 9px; }

  @media (max-width: 599px) {
    .history-row { grid-template-columns: minmax(0, 1fr) 18px; gap: 4px; padding-right: 5px; }
    .history-row .row-select { grid-template-columns: 17px 25px minmax(0, 1fr); gap: 5px; padding-left: 6px; }
    .history-row .row-select :global(.source-icon) { width: 25px; height: 25px; }
    .history-row .row-time { display: none; }
    .history-row .row-main { font-size: 11px; }
    .history-row .row-sub { font-size: 8px; }
    .history-row .slot { font-size: 9px; }
    .command-row { grid-template-columns: minmax(0, 1fr) 18px 31px; gap: 5px; padding-right: 10px; padding-left: 10px; }
    .command-row .row-select { grid-template-columns: 23px 22px minmax(0, 1fr); gap: 6px; }
    .command-row .row-select :global(.source-icon) { width: 22px; height: 22px; }
    .command-row .row-main { font-size: 12px; }
    .command-row .row-time { font-size: 8px; }
  }
  /* 只在设置里选了“跟随系统”且系统开了“减少动画效果”时生效（data-reduce-motion）。 */
  /* 进度是信息（还要按多久），减少动画时保留；只去掉星标晃动。 */
  :global(html[data-reduce-motion="true"]) .holding .row-star { animation: none; }
  :global(html[data-reduce-motion="true"]) .command-row .slot { transition: opacity var(--duration-reduce) linear; transform: none; }
</style>
