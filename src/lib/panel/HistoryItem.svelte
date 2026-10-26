<script lang="ts">
  // 一条历史：槽位号（前 9 条是 1–9）、类型、三行摘要（搜索命中高亮）、来源 · 时间。
  // 按下时 preventDefault，不让按钮抢走搜索框的焦点。
  import { formatTime, type PanelState } from "../state.svelte";
  import type { HistoryItem } from "../ipc";

  let { panel, item, index }: { panel: PanelState; item: HistoryItem; index: number } = $props();
</script>

<button
  class="entry"
  class:selected={index === panel.selected}
  aria-pressed={index === panel.selected}
  tabindex="-1"
  disabled={panel.busy || !panel.interactive}
  onpointerenter={() => { if (panel.interactive && !panel.busy) panel.selected = index; }}
  onpointerdown={(event) => event.preventDefault()}
  onclick={(event) => panel.choose(index, "paste", event)}
>
  <span class="badge" aria-hidden="true">{index < 9 ? index + 1 : "·"}</span>
  <span class="entry-content">
    <span class="entry-type">{item.kind}</span>
    <span class="preview">{#each panel.highlights(item.preview || "（空文本）") as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
    <span class="metadata">{item.source} · {formatTime(item.copied_ms)}{item.copy_count > 1 ? ` · 复制 ${item.copy_count} 次` : ""}</span>
    {#if item.partial}<span class="partial">部分格式未保存</span>{/if}
  </span>
</button>

<style>
  .entry { display: flex; align-items: flex-start; gap: var(--space-entry-gap); width: 100%; border: 1px solid transparent; border-radius: var(--radius-md); padding: var(--space-entry-padding-y) var(--space-entry-padding-x); text-align: left; background: transparent; }
  .entry.selected { background: var(--color-bg-selected); border-color: var(--color-border-selected); }
  .badge { flex: 0 0 20px; line-height: 21px; text-align: center; color: var(--color-keycap-text); font-size: var(--font-size-caption); border-radius: var(--radius-sm); background: var(--color-keycap-bg); }
  .entry-content { min-width: 0; flex: 1; }
  .entry-type { display: block; color: var(--color-accent); font-size: var(--font-size-caption); margin-bottom: var(--space-entry-type-after); }
  /* svelte-check 会提示“补上标准的 line-clamp”。故意不补：新版 WebView2 认标准属性后截断方式可能变，像素就不一致了；
     M1 按设计改成一条一行时，这几行会一起换掉。 */
  .preview { display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.55; }
  .metadata, .partial { display: block; font-size: var(--font-size-micro); line-height: 1.5; margin-top: var(--space-1); overflow-wrap: anywhere; }
  .metadata { color: var(--color-text-secondary); }
  .partial { color: var(--color-caution); }
  mark { background: var(--color-mark-bg); color: var(--color-mark-text); border-radius: var(--radius-xs); }
</style>
