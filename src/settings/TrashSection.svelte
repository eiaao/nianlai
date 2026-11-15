<script lang="ts">
  // 完整回收站：拿回和单项彻底删除保持原 IPC；彻底清空复用四秒原地确认按钮。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { TrashItem } from "../lib/ipc";
  import ConfirmButton from "./ConfirmButton.svelte";
  import { message } from "./format";
  import { trashTimeLeft } from "./trashFormat";

  let { disabled }: { disabled: boolean } = $props();

  let items = $state<TrashItem[] | null>(null);
  let error = $state("");
  let busy = $state(false);
  let notice = $state("");

  function when(ms: number) {
    const d = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  function left(ms: number) {
    return trashTimeLeft(ms);
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
    if (busy || disabled) return;
    busy = true; error = ""; notice = "";
    try {
      await action();
      notice = done;
      await load();
    } catch (value) {
      error = message(value);
    } finally {
      busy = false;
    }
  }
</script>

<div class="group"><div class="card">
  <p class="note">扔掉的放在这儿，7 天后自动清掉。可以拿回，也可以在完整列表里彻底删除。面板里按 Ctrl+Z 仍能撤销最近的删除。</p>
  {#if error}<p class="note error" role="alert">{error}</p>{/if}
  {#if items === null}
    {#if !error}<p class="note">正在读取…</p>{/if}
  {:else}
    <ul class="list" aria-label="回收站" data-settings-scroll>
      {#each items as item (item.id)}
        <li>
          <span class="text" title={label(item)}>{#if item.favorite}<span class="star" aria-label="收藏">藏</span>{/if}{label(item)}</span>
          <span class="meta">{item.kind} · {when(item.deleted_ms)} 删掉 · {left(item.deleted_ms)}</span>
          <span class="actions">
            <button type="button" disabled={busy || disabled} onclick={() => run(() => ipc.restoreItem(item.id), "拿回来了")}>拿回来</button>
            <button type="button" class="danger" disabled={busy || disabled} onclick={() => run(() => ipc.purgeItems([item.id]), "已彻底删除")}>彻底删除</button>
          </span>
        </li>
      {:else}
        <li class="empty">回收站是空的</li>
      {/each}
    </ul>
    {#if items.length}
      <div class="foot">
        <small>共 {items.length} 张{notice ? ` · ${notice}` : ""}；彻底清空后不能拿回来。</small>
        <span data-empty-trash>
          <ConfirmButton label="彻底清空" confirmLabel="再点一次，彻底清空" doneLabel="已彻底清空" danger
            disabled={busy || disabled || !items.length}
            action={() => ipc.purgeItems(null)}
            onpending={(pending) => { busy = pending; if (pending) { error = ""; notice = ""; } }}
            onlanded={async () => { notice = "回收站已彻底清空"; await load(); }} />
        </span>
      </div>
    {:else if notice}
      <p class="note">{notice}</p>
    {/if}
  {/if}
</div></div>

<style>
  .note { margin: 0; padding: 12px 16px; border-bottom: 1px solid var(--hand-line); color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .note.error { color: var(--state-critical); }
  .list { max-height: 320px; margin: 0; padding: 0; overflow: auto; list-style: none; }
  .list li { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-rows: auto auto; column-gap: 12px; row-gap: 4px; align-items: center; padding: 8px 16px; border-bottom: 1px solid var(--hand-line); }
  .list li.empty { display: block; color: var(--hand-ink-2); font-size: 12px; }
  .text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 13px; }
  .star { margin-right: 4px; color: var(--hand-seal); }
  .meta { grid-row: 2; color: var(--hand-ink-2); font-size: 12px; }
  .actions { grid-row: 1 / span 2; grid-column: 2; display: flex; gap: 8px; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px; }
  .foot small { color: var(--hand-ink-2); font-size: 12px; }
  button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); font-size: 12px; }
  button.danger { color: var(--state-critical); }
  button:disabled { opacity: .6; }
  [data-empty-trash] :global(.confirm-button) { color: var(--state-critical); }
</style>
