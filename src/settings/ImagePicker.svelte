<script lang="ts">
  // 手动挑选截图（M2-3）：存在历史库里的图片，缩略图网格，选中的移进回收站。收藏的不能选。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { ImageItem } from "../lib/ipc";
  import { message, shortTime, size } from "./format";

  let { disabled, onchange }: { disabled: boolean; onchange: () => void } = $props();

  let items = $state<ImageItem[] | null>(null);
  let error = $state("");
  let notice = $state("");
  let busy = $state(false);
  let order = $state<"size" | "time">("size");
  let range = $state<"all" | "30" | "90">("all");
  let selected = $state<Set<number>>(new Set());
  let anchor: number | null = null;
  let thumbs = $state<Record<number, string | null>>({});

  const DAY = 24 * 60 * 60 * 1000;
  const shown = $derived.by(() => {
    const cutoff = range === "all" ? Infinity : Date.now() - Number(range) * DAY;
    const list = (items ?? []).filter((item) => item.copied_ms < cutoff);
    return order === "size" ? list : [...list].sort((a, b) => b.copied_ms - a.copied_ms);
  });
  const picked = $derived((items ?? []).filter((item) => selected.has(item.id)));
  const pickedBytes = $derived(picked.reduce((sum, item) => sum + item.bytes, 0));

  async function load() {
    try {
      items = await ipc.getImages();
      error = "";
      const alive = new Set(items.map((item) => item.id));
      selected = new Set([...selected].filter((id) => alive.has(id)));
    } catch (value) {
      error = message(value);
    }
  }
  onMount(() => { void load(); });

  // 缩略图滚到视野里才取，一次只取几张，避免一开页面就解码几百张图。
  const queue: number[] = [];
  let running = 0;
  function pump() {
    while (running < 3 && queue.length) {
      const id = queue.shift()!;
      running += 1;
      ipc.getThumbnail(id)
        .then((url) => { thumbs[id] = url; })
        .catch(() => { thumbs[id] = null; })
        .finally(() => { running -= 1; pump(); });
    }
  }
  function lazy(node: HTMLElement, id: number) {
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting) && !(id in thumbs) && !queue.includes(id)) {
        queue.push(id);
        pump();
        observer.disconnect();
      }
    }, { rootMargin: "120px" });
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
  }

  function toggle(item: ImageItem, event: MouseEvent) {
    if (item.favorite) return;
    const next = new Set(selected);
    if (event.shiftKey && anchor !== null) {
      const from = shown.findIndex((entry) => entry.id === anchor);
      const to = shown.findIndex((entry) => entry.id === item.id);
      if (from >= 0 && to >= 0) {
        for (const entry of shown.slice(Math.min(from, to), Math.max(from, to) + 1)) {
          if (!entry.favorite) next.add(entry.id);
        }
        selected = next;
        return;
      }
    }
    if (next.has(item.id)) next.delete(item.id); else next.add(item.id);
    anchor = item.id;
    selected = next;
  }

  async function trash() {
    if (!picked.length) return;
    busy = true; error = ""; notice = "";
    try {
      const moved = await ipc.trashItems(picked.map((item) => item.id));
      notice = `已移到回收站 ${moved} 张，7 天内可以在回收站里恢复`;
      selected = new Set();
      await load();
      onchange();
    } catch (value) {
      error = message(value);
    } finally {
      busy = false;
    }
  }
</script>

<div><p class="page-intro">截图、浏览器里复制的图片没有源文件，存在历史库里。选中的移到回收站，7 天后彻底删除；收藏的不能选。</p></div>

<div class="card picker">
  <div class="toolbar">
    <label>排序 <select bind:value={order}><option value="size">从大到小</option><option value="time">从新到旧</option></select></label>
    <label>范围 <select bind:value={range}><option value="all">全部时间</option><option value="30">30 天前的</option><option value="90">90 天前的</option></select></label>
    <span class="grow"></span>
    <button type="button" disabled={busy || !shown.length} onclick={() => { selected = new Set(shown.filter((item) => !item.favorite).map((item) => item.id)); }}>全选当前</button>
    <button type="button" disabled={busy || !selected.size} onclick={() => { selected = new Set(); }}>清除选择</button>
  </div>
  {#if error}<p class="note error">{error}</p>{/if}
  {#if items === null}
    {#if !error}<p class="note">正在读取…</p>{/if}
  {:else if !shown.length}
    <p class="note">{items.length ? "这个范围里没有图片。" : "历史里没有存在库里的图片。"}</p>
  {:else}
    <div class="grid" role="listbox" aria-multiselectable="true" aria-label="图片">
      {#each shown as item (item.id)}
        <button type="button" class="thumb" class:on={selected.has(item.id)} class:fav={item.favorite} role="option" aria-selected={selected.has(item.id)}
          disabled={busy || disabled || item.favorite} title={item.favorite ? "收藏的图片不能清理" : `${size(item.bytes)} · ${shortTime(item.copied_ms)}`}
          use:lazy={item.id} onclick={(event) => toggle(item, event)}>
          {#if thumbs[item.id]}<img src={thumbs[item.id]} alt="" />{:else if thumbs[item.id] === null}<span class="broken">无法预览</span>{/if}
          {#if !item.favorite}<b>{selected.has(item.id) ? "✓" : ""}</b>{/if}
          <span class="label">{item.favorite ? "★ 收藏" : `${size(item.bytes)} · ${shortTime(item.copied_ms)}`}</span>
        </button>
      {/each}
    </div>
  {/if}
  <div class="toolbar foot">
    <span>{notice || (picked.length ? `已选 ${picked.length} 张 · ${size(pickedBytes)}` : `共 ${shown.length} 张`)}</span>
    <span class="grow"></span>
    <button type="button" class="primary" disabled={busy || disabled || !picked.length} onclick={trash}>{busy ? "正在移动…" : "移到回收站"}</button>
  </div>
</div>

<style>
  .picker { display: flex; flex-direction: column; }
  .toolbar { display: flex; align-items: center; gap: 8px; padding: 10px 16px; border-bottom: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 11px; }
  .toolbar.foot { border-top: 1px solid var(--settings-line); border-bottom: 0; }
  .toolbar label { display: flex; align-items: center; gap: 6px; }
  .toolbar select { min-width: 0; min-height: 28px; }
  .grow { flex: 1; }
  .note { margin: 0; padding: 14px 16px; color: var(--settings-muted); font-size: 11px; }
  .note.error { color: var(--state-critical); }
  .grid { max-height: 380px; overflow: auto; display: grid; grid-template-columns: repeat(auto-fill, minmax(108px, 1fr)); gap: 8px; padding: 12px 16px; }
  .thumb { position: relative; aspect-ratio: 4 / 3; min-height: 0; padding: 0; overflow: hidden; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); }
  .thumb img { width: 100%; height: 100%; display: block; object-fit: cover; }
  .thumb.on { outline: 2px solid var(--settings-accent); outline-offset: -2px; }
  .thumb.fav { opacity: .45; cursor: default; }
  .thumb b { position: absolute; right: 4px; top: 4px; width: 15px; height: 15px; display: grid; place-items: center; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-card); color: var(--settings-accent); font-size: 10px; }
  .thumb .label { position: absolute; left: 0; right: 0; bottom: 0; padding: 2px 5px; background: color-mix(in srgb, var(--settings-card) 82%, transparent); color: var(--settings-text); font: 9px var(--font-mono); text-align: left; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .broken { position: absolute; inset: 0; display: grid; place-items: center; color: var(--settings-muted); font-size: 10px; }
  button { min-height: 28px; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; }
  button.primary { border-color: var(--settings-accent); color: var(--settings-accent); font-weight: 600; }
  button:disabled { opacity: .6; }
  .thumb:disabled { opacity: 1; }
  .thumb.fav:disabled { opacity: .45; }
</style>
