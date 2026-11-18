<script lang="ts">
  // 数据主页的回收站摊开区：只展示后端顺序中的最近八张；彻底删除仍只属于完整列表。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { HistoryItem, TrashItem } from "../lib/ipc";
  import { message } from "./format";
  import { trashTimeLeft } from "./trashFormat";

  let { disabled, onchange, onviewall }: {
    disabled: boolean;
    onchange: () => Promise<void>;
    onviewall: () => void;
  } = $props();

  let items = $state<TrashItem[] | null>(null);
  let loaded = $state(false);
  let expanded = $state(true);
  let busyId = $state<number | null>(null);
  let error = $state("");
  let notice = $state("");
  let thumbnails = $state<Record<number, string | null>>({});
  let generation = 0;
  let disposed = false;

  const recent = $derived((items ?? []).slice(0, 8));

  function kindLabel(item: TrashItem) {
    if (item.kind === "文本") return "文字";
    if (item.kind === "富文本") return "带格式的文字";
    return item.kind;
  }
  function label(item: TrashItem) {
    const text = (item.preview || item.file_names.join("、")).replace(/\s+/g, " ").trim();
    return text || kindLabel(item);
  }

  // 所有入口共用代次：只有最新请求能提交列表、错误或 loaded，旧成功/旧失败都只返回结果给调用方判断。
  export async function reload(): Promise<{ current: boolean; error: string }> {
    const request = ++generation;
    try {
      const value = await ipc.getTrash();
      if (disposed || request !== generation) return { current: false, error: "" };
      items = value;
      error = "";
      loaded = true;
      return { current: true, error: "" };
    } catch (value) {
      const text = message(value);
      if (disposed || request !== generation) return { current: false, error: "" };
      error = text;
      loaded = true;
      return { current: true, error: text };
    }
  }
  onMount(() => {
    disposed = false;
    void reload();
    return () => { disposed = true; generation += 1; };
  });

  $effect(() => {
    const visible = new Set(recent.map((item) => item.id));
    for (const key of Object.keys(thumbnails)) {
      const id = Number(key);
      if (!visible.has(id)) delete thumbnails[id];
    }
    for (const item of recent) {
      const id = item.id;
      if (!item.kind.includes("图片") || id in thumbnails) continue;
      thumbnails[id] = null;
      const commit = (url: string | null) => {
        if (!disposed && recent.some((entry) => entry.id === id)) thumbnails[id] = url;
      };
      void ipc.getThumbnail(id).then(commit).catch(() => commit(null));
    }
  });

  async function refresh() {
    const parent = onchange().then(() => "").catch((value) => message(value));
    const list = await reload();
    return { list, parentError: await parent };
  }

  function refreshWarning(listError: string, parentError: string) {
    const parts = [];
    if (listError) parts.push("列表没刷新");
    if (parentError) parts.push("占用没刷新");
    return { parts, detail: listError || parentError };
  }

  async function restore(item: TrashItem) {
    if (busyId !== null || disabled) return;
    busyId = item.id;
    error = "";
    notice = "";
    let restored: HistoryItem | null;
    try {
      restored = await ipc.restoreItem(item.id);
    } catch (value) {
      if (!disposed) error = `${label(item)} 没拿回来：${message(value)}`;
      if (!disposed) busyId = null;
      return;
    }
    if (disposed) return;

    if (restored === null) {
      const { list, parentError } = await refresh();
      if (disposed) return;
      const warning = list.current ? refreshWarning(list.error, parentError) : { parts: [], detail: "" };
      error = `这张已经不在回收站${warning.parts.length ? `；${warning.parts.join("、")}：${warning.detail}` : ""}`;
      notice = "";
      if (!disposed) busyId = null;
      return;
    }

    // IPC 已确认恢复：先把该 ID 从可见列表移除，再分别核对列表与父页占用刷新结果。
    items = (items ?? []).filter((entry) => entry.id !== item.id);
    loaded = true;
    const { list, parentError } = await refresh();
    if (disposed) return;
    if (list.current) {
      const warning = refreshWarning(list.error, parentError);
      if (warning.parts.length) {
        error = `已拿回，但${warning.parts.join("、")}：${warning.detail}`;
        notice = "";
      } else {
        error = "";
        notice = "拿回来了";
      }
    }
    if (!disposed) busyId = null;
  }
</script>

<div class="trash-fan" data-trash-fan data-loaded={loaded ? "true" : "false"}>
  <div class="fan-head">
    <div>
      <strong>回收站 <span data-trash-fan-count>{items?.length ?? "…"}</span> 张</strong>
      <small>扔掉的先放这儿，7 天后清掉。这里摊开最近 8 张。</small>
    </div>
    <span class="fan-actions">
      <button type="button" onclick={onviewall}>查看全部</button>
      <button type="button" aria-expanded={expanded} onclick={() => { expanded = !expanded; }}>{expanded ? "收起" : "摊开"}</button>
    </span>
  </div>
  {#if error}<p class="fan-message error" role="alert">{error}{#if items === null}<button type="button" onclick={() => { loaded = false; void reload(); }}>重新读取</button>{/if}</p>{/if}
  {#if notice}<p class="fan-message ok" role="status">{notice}</p>{/if}
  {#if expanded}
    {#if !loaded}
      <p class="fan-empty">正在读取…</p>
    {:else if items === null}
      <p class="fan-empty">没读到回收站，再试一次。</p>
    {:else if items.length === 0}
      <p class="fan-empty">回收站是空的</p>
    {:else}
      <div class="trash-cards">
        {#each recent as item, index (item.id)}
          <article class="trash-card" data-trash-card data-id={item.id} style={`--turn:${(index % 4 - 1.5) * 1.2}deg`}>
            <span class="kind">{kindLabel(item)}{#if item.favorite}<span aria-label="收藏"> · 藏</span>{/if}</span>
            {#if item.kind.includes("图片") && thumbnails[item.id]}
              <img src={thumbnails[item.id]} alt={label(item)} />
            {:else}
              <span class="summary" title={label(item)}>{label(item)}</span>
            {/if}
            <span class="left">{trashTimeLeft(item.deleted_ms)}</span>
            <button type="button" class="restore" disabled={disabled || busyId !== null} onclick={() => restore(item)}>{busyId === item.id ? "正在拿回来…" : "拿回来"}</button>
          </article>
        {/each}
      </div>
      {#if items.length > 8}<p class="more">还有 {items.length - 8} 张，点“查看全部”慢慢看。</p>{/if}
    {/if}
  {/if}
</div>

<style>
  .trash-fan { display: grid; gap: 12px; padding: 16px; border-top: 1px solid var(--hand-line); background: var(--hand-card-back); }
  .fan-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
  .fan-head > div { min-width: 0; }
  .fan-head strong { display: block; font-size: 13px; }
  .fan-head small { display: block; margin-top: 4px; color: var(--hand-ink-2); font-size: 12px; line-height: 1.5; }
  .fan-actions { display: flex; flex: none; gap: 8px; }
  button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); color: var(--hand-ink); font-size: 12px; }
  button:hover:not(:disabled) { border-color: var(--hand-seal); }
  button:disabled { opacity: .6; }
  .trash-cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; padding: 8px 4px 4px; }
  .trash-card { min-width: 0; min-height: 144px; display: flex; flex-direction: column; gap: 8px; padding: 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); box-shadow: var(--hand-row-shadow); transform: rotate(var(--turn)); animation: spread 220ms ease-out both; }
  .kind { color: var(--hand-ink-2); font-size: 12px; }
  .summary { min-height: 40px; display: -webkit-box; overflow: hidden; color: var(--hand-ink); font-size: 13px; line-height: 1.55; overflow-wrap: anywhere; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
  img { width: 100%; height: 48px; object-fit: cover; border-radius: 2px; background: var(--hand-paper); }
  .left { margin-top: auto; color: var(--hand-ink-2); font-size: 12px; }
  .restore { align-self: flex-start; }
  .more, .fan-empty, .fan-message { margin: 0; color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .fan-empty { padding: 16px 4px; text-align: center; }
  .fan-message { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .fan-message.error { color: var(--state-critical); }
  .fan-message.ok { color: var(--state-success); }
  @keyframes spread { from { opacity: 0; transform: translateY(12px) rotate(0); } to { opacity: 1; transform: translateY(0) rotate(var(--turn)); } }
  :global(html[data-motion="none"]) .trash-card, :global(html[data-reduce-motion="true"]) .trash-card { animation: none; transition: none; }
  @media (max-width: 900px) { .trash-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (forced-colors: active) { .trash-card { border-color: CanvasText; box-shadow: none; } }
</style>
