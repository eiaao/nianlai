<script lang="ts">
  // 回收站（B12）：删除和清理的条目在这里保留 7 天，可以恢复或彻底删除。“数据与备份”的子页面。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { TrashItem } from "../lib/ipc";

  let { disabled }: { disabled: boolean } = $props();

  let items = $state<TrashItem[] | null>(null);
  let error = $state("");
  let busy = $state(false);
  let confirmEmpty = $state(false);
  let notice = $state("");

  const DAY = 24 * 60 * 60 * 1000;

  function message(value: unknown) {
    return typeof value === "string" ? value : value instanceof Error ? value.message : "操作没有完成，请重试";
  }

  function when(ms: number) {
    const d = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function left(ms: number) {
    const days = Math.ceil((ms + 7 * DAY - Date.now()) / DAY);
    return days <= 1 ? "不到 1 天后清除" : `${days} 天后清除`;
  }

  function label(item: TrashItem) {
    const text = (item.preview || item.file_names[0] || "").replace(/\s+/g, " ").trim();
    return text || item.kind;
  }

  async function load() {
    try {
      items = await ipc.getTrash();
      error = "";
    } catch (value) {
      error = message(value);
    }
  }

  onMount(() => { void load(); });

  async function run(action: () => Promise<unknown>, done: string) {
    busy = true; error = ""; notice = "";
    try {
      await action();
      notice = done;
      await load();
    } catch (value) {
      error = message(value);
    } finally {
      busy = false;
      confirmEmpty = false;
    }
  }
</script>

<div class="group"><div class="card">
  <p class="note">在面板里删除、在“垃圾清理”里清理的记录先放在这里，保留 7 天后自动彻底删除。面板里按 Ctrl+Z 可以撤销最近的删除。</p>
  {#if error}<p class="note error">{error}</p>{/if}
  {#if items === null}
    {#if !error}<p class="note">正在读取…</p>{/if}
  {:else}
    <ul class="list" aria-label="回收站">
      {#each items as item (item.id)}
        <li>
          <span class="text" title={label(item)}>{#if item.favorite}<span class="star" aria-label="收藏">★</span>{/if}{label(item)}</span>
          <span class="meta">{item.kind} · {when(item.deleted_ms)} 删除 · {left(item.deleted_ms)}</span>
          <span class="actions">
            <button type="button" disabled={busy || disabled} onclick={() => run(() => ipc.restoreItem(item.id), "已恢复到历史")}>恢复</button>
            <button type="button" class="danger" disabled={busy || disabled} onclick={() => run(() => ipc.purgeItems([item.id]), "已彻底删除")}>彻底删除</button>
          </span>
        </li>
      {:else}
        <li class="empty">回收站是空的。</li>
      {/each}
    </ul>
    {#if items.length}
      <div class="foot">
        {#if confirmEmpty}
          <small class="warn">彻底删除这 {items.length} 条？删除后不能恢复。</small>
          <span class="actions">
            <button type="button" class="danger" disabled={busy || disabled} onclick={() => run(() => ipc.purgeItems(null), "回收站已清空")}>确认清空</button>
            <button type="button" disabled={busy} onclick={() => { confirmEmpty = false; }}>取消</button>
          </span>
        {:else}
          <small>共 {items.length} 条{notice ? ` · ${notice}` : ""}</small>
          <button type="button" class="danger" disabled={busy || disabled} onclick={() => { confirmEmpty = true; }}>清空回收站</button>
        {/if}
      </div>
    {:else if notice}
      <p class="note">{notice}</p>
    {/if}
  {/if}
</div></div>

<style>
  .note { margin: 0; padding: 11px 16px; border-bottom: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .note.error { color: var(--state-critical); }
  .list { max-height: 320px; margin: 0; padding: 0; overflow: auto; list-style: none; }
  .list li { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-rows: auto auto; column-gap: 12px; row-gap: 2px; align-items: center; padding: 8px 16px; border-bottom: 1px solid var(--settings-line); }
  .list li.empty { display: block; color: var(--settings-muted); font-size: 11px; }
  .text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 12px; }
  .star { margin-right: 4px; color: var(--settings-accent); }
  .meta { grid-row: 2; color: var(--settings-muted); font-size: 10px; }
  .actions { grid-row: 1 / span 2; grid-column: 2; display: flex; gap: 6px; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 16px; }
  .foot .actions { grid-row: auto; }
  .foot small { color: var(--settings-muted); font-size: 10px; }
  .foot small.warn { color: var(--state-critical); }
  button { min-height: 26px; padding: 3px 10px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; }
  button.danger { color: var(--state-critical); }
  button:disabled { opacity: .6; }
</style>
