<script lang="ts">
  import { convertFileSrc } from "@tauri-apps/api/core";
  import type { HistoryItem } from "../ipc";
  import { sourceName } from "./content";

  let { item }: { item: HistoryItem } = $props();
  let failed = $state(false);
  const name = $derived(sourceName(item));
  const glyph = $derived([...name][0]?.toLocaleUpperCase() || "?");
  const iconUrl = $derived(item.source_icon ? convertFileSrc(item.source_icon) : null);

  $effect(() => {
    item.source_icon;
    failed = false;
  });
</script>

<span class="source-icon" aria-hidden="true">
  {#if iconUrl && !failed}
    <img src={iconUrl} alt="" loading="lazy" decoding="async" onerror={() => { failed = true; }} />
  {:else}
    <span class="fallback">{glyph}</span>
  {/if}
</span>

<style>
  .source-icon { width: 30px; height: 30px; display: grid; place-items: center; overflow: hidden; flex: none; border: 1px solid var(--line-soft); border-radius: var(--radius-md); background: var(--panel-raised); color: var(--muted); }
  img { display: block; width: 100%; height: 100%; object-fit: contain; }
  .fallback { font: 600 12px/1 var(--font-ui); }
</style>
