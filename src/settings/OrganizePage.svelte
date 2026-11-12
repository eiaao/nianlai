<script lang="ts">
  // “整理”分区（M2-6）：常用的顺序（E03）、标题和备注（E04），以及粘贴旧条目后是否移到最前（B10）。
  // 常用的整理只在这里做，面板里不放（11-08 定）。顺序改动立即保存，面板下次呼出按新顺序。
  import { onMount, tick } from "svelte";
  import { flip } from "svelte/animate";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, HistoryItem, SettingsUpdate } from "../lib/ipc";
  import { reducedMotion } from "../lib/preferences";
  import { cardKind, imageSize, sourceName } from "../lib/cards";
  import { message } from "./format";

  let { reuseToTop, disabled, onsave }: {
    reuseToTop: boolean;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  const TITLE_MAX = 200;
  const NOTE_MAX = 2000;

  let items = $state<HistoryItem[] | null>(null);
  let error = $state("");
  let editing = $state<number | null>(null);
  let selected = $state<number | null>(null);
  let saving = $state<Record<number, "saving" | "saved" | "failed">>({});
  let drafts = $state<Record<number, { title: string; note: string }>>({});
  let list = $state<HTMLOListElement>();

  // 拖动：被拖的一行在原位变成虚线框，一张浮起、略微倾斜的“替身”跟着指针走；松手后替身落回虚线框。
  let drag = $state<{ id: number; pointer: number; grab: number; top: number; width: number; height: number; before: number[] } | null>(null);
  let settled = $state<number | null>(null);

  async function load() {
    try {
      items = await ipc.getFavorites();
      error = "";
    } catch (value) {
      error = message(value);
    }
  }
  onMount(() => {
    void load();
    // 面板里常用或取消常用后，回到设置窗口时刷新。
    const refresh = () => { if (!drag && editing === null) void load(); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  });

  /** 列表第一行：有标题时显示标题，否则显示原内容。 */
  function original(item: HistoryItem) {
    const kind = cardKind(item);
    if (kind === "image") return `图片${imageSize(item) ? ` · ${imageSize(item)}` : ""}`;
    if (kind === "file") return `文件 · ${item.file_names.length} 项`;
    return (item.preview || "（空文本）").replace(/\s+/g, " ").trim();
  }

  async function commitOrder(next: HistoryItem[], before: number[]) {
    const ids = next.map((item) => item.id);
    if (ids.join() === before.join()) return;
    try {
      items = await ipc.reorderFavorites(ids);
      error = "";
    } catch (value) {
      error = `顺序没有保存：${message(value)}`;
      await load();
    }
  }

  function move(id: number, delta: number) {
    if (!items || disabled) return;
    const from = items.findIndex((item) => item.id === id);
    const to = from + delta;
    if (from < 0 || to < 0 || to >= items.length) return;
    const before = items.map((item) => item.id);
    const next = [...items];
    next.splice(to, 0, ...next.splice(from, 1));
    items = next;
    void commitOrder(next, before);
  }

  function onRowKey(event: KeyboardEvent, item: HistoryItem) {
    if (event.altKey && !event.ctrlKey && !event.shiftKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      move(item.id, event.key === "ArrowUp" ? -1 : 1);
      // 移动后焦点跟着这一行走，可以连续按。
      void tick().then(() => list?.querySelector<HTMLElement>(`[data-id="${item.id}"] .row-main`)?.focus());
    } else if (!event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown") && items) {
      event.preventDefault();
      const at = items.findIndex((entry) => entry.id === item.id) + (event.key === "ArrowUp" ? -1 : 1);
      const target = items[at];
      if (target) { selected = target.id; list?.querySelector<HTMLElement>(`[data-id="${target.id}"] .row-main`)?.focus(); }
    }
  }

  function startDrag(event: PointerEvent, item: HistoryItem) {
    if (event.button !== 0 || disabled || !items || !list) return;
    const row = (event.currentTarget as HTMLElement).closest<HTMLElement>("li");
    if (!row) return;
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    const rect = row.getBoundingClientRect();
    const host = list.getBoundingClientRect();
    selected = item.id;
    drag = { id: item.id, pointer: event.clientY, grab: event.clientY - rect.top, top: rect.top - host.top, width: rect.width, height: rect.height, before: items.map((entry) => entry.id) };
  }

  function moveDrag(event: PointerEvent) {
    if (!drag || !items || !list) return;
    drag.pointer = event.clientY;
    // 按其他行的中线找落点：指针越过哪一行的中线，就排到它前面或后面。
    const rows = [...list.querySelectorAll<HTMLElement>("li[data-id]")];
    const center = event.clientY - drag.grab + drag.height / 2;
    let target = 0;
    for (const row of rows) {
      if (Number(row.dataset.id) === drag.id) continue;
      const rect = row.getBoundingClientRect();
      if (center > rect.top + rect.height / 2) target += 1;
    }
    const from = items.findIndex((entry) => entry.id === drag!.id);
    if (from !== target && from >= 0) {
      const next = [...items];
      next.splice(target, 0, ...next.splice(from, 1));
      items = next;
    }
  }

  async function endDrag() {
    if (!drag || !items) return;
    const { id, before } = drag;
    drag = null;
    settled = id;
    setTimeout(() => { if (settled === id) settled = null; }, 360);
    await commitOrder(items, before);
  }

  const ghostTop = $derived.by(() => {
    if (!drag || !list) return 0;
    const host = list.getBoundingClientRect();
    return drag.pointer - drag.grab - host.top;
  });

  function toggleEdit(item: HistoryItem) {
    editing = editing === item.id ? null : item.id;
    selected = item.id;
    if (editing !== null) drafts[item.id] = { title: item.title ?? "", note: item.note ?? "" };
  }

  // 标题、备注输入即保存：停顿 500 毫秒或离开输入框时写入。
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  function schedule(id: number) {
    clearTimeout(timers.get(id));
    timers.set(id, setTimeout(() => void saveLabel(id), 500));
  }
  async function saveLabel(id: number) {
    clearTimeout(timers.get(id));
    timers.delete(id);
    const draft = drafts[id];
    const item = items?.find((entry) => entry.id === id);
    if (!draft || !item || (draft.title === (item.title ?? "") && draft.note === (item.note ?? ""))) return;
    saving[id] = "saving";
    try {
      const updated = await ipc.setFavoriteLabel(id, draft.title, draft.note);
      if (items) items = items.map((entry) => (entry.id === id ? updated : entry));
      saving[id] = "saved";
    } catch (value) {
      saving[id] = "failed";
      error = `标题和备注没有保存：${message(value)}`;
    }
  }
  onMount(() => () => { for (const id of timers.keys()) void saveLabel(id); });

  const status = (id: number) => saving[id] === "saving" ? "正在保存…" : saving[id] === "saved" ? "已保存" : saving[id] === "failed" ? "没有保存" : "改动自动保存";
</script>

<section class="page">
  <div><h2>整理</h2><p class="page-intro">常用的顺序就是面板里常用区的顺序，不会因为复制或粘贴而变动。可以给常用起个标题、写一句备注，搜索时也能搜到。</p></div>

  <div class="group">
    <span class="group-title">常用<em class="title-note">拖动排序，或选中后 Alt+↑ / Alt+↓</em></span>
    <div class="card">
      {#if error}<p class="note error" role="alert">{error}</p>{/if}
      {#if items === null}
        {#if !error}<p class="note">正在读取…</p>{/if}
      {:else if !items.length}
        <p class="note">还没有常用。在面板里选中一条，按 Ctrl+D 或点星标常用。</p>
      {:else}
        <ol class="favorites" bind:this={list} aria-label="常用" class:dragging={!!drag}>
          {#each items as item, index (item.id)}
            <li data-id={item.id} class:placeholder={drag?.id === item.id} class:open={editing === item.id} class:selected={selected === item.id} class:settled={settled === item.id && !reducedMotion()}
              animate:flip={{ duration: reducedMotion() ? 0 : 200 }}>
              <div class="row">
                <button type="button" class="handle" aria-label="拖动调整顺序" tabindex="-1" disabled={disabled || items.length < 2}
                  onpointerdown={(event) => startDrag(event, item)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}><svg viewBox="0 0 10 16" width="10" height="16" aria-hidden="true"><g fill="currentColor"><circle cx="2.5" cy="3" r="1.3"/><circle cx="7.5" cy="3" r="1.3"/><circle cx="2.5" cy="8" r="1.3"/><circle cx="7.5" cy="8" r="1.3"/><circle cx="2.5" cy="13" r="1.3"/><circle cx="7.5" cy="13" r="1.3"/></g></svg></button>
                <span class="index">{String(index + 1).padStart(2, "0")}</span>
                <button type="button" class="row-main" aria-label={`${item.title || original(item)}，第 ${index + 1} 个常用`}
                  onfocus={() => { selected = item.id; }} onclick={() => { selected = item.id; }} onkeydown={(event) => onRowKey(event, item)}>
                  <strong class:untitled={!item.title}>{item.title || original(item)}</strong>
                  <small>
                    {#if item.title}{original(item)}{:else}{sourceName(item)} · 未加标题{/if}{#if item.note}<span class="note-inline">{` · 备注：${item.note.replace(/\s+/g, " ")}`}</span>{/if}
                  </small>
                </button>
                <button type="button" class="edit" aria-expanded={editing === item.id} disabled={disabled} onclick={() => toggleEdit(item)}>{editing === item.id ? "收起" : "编辑"}</button>
              </div>
              {#if editing === item.id && drafts[item.id]}
                <div class="editor">
                  <label><span>标题</span><input type="text" maxlength={TITLE_MAX} placeholder="例如：家里收货地址" bind:value={drafts[item.id].title} oninput={() => schedule(item.id)} onblur={() => saveLabel(item.id)} /></label>
                  <label><span>备注</span><textarea rows="2" maxlength={NOTE_MAX} placeholder="什么时候用、给谁用…" bind:value={drafts[item.id].note} oninput={() => schedule(item.id)} onblur={() => saveLabel(item.id)}></textarea></label>
                  <p class="hint"><span class="origin">原内容：{original(item)}（不可修改）</span><span class:failed={saving[item.id] === "failed"}>· {status(item.id)}</span></p>
                </div>
              {/if}
            </li>
          {/each}
          {#if drag}
            {@const item = items.find((entry) => entry.id === drag?.id)}
            {#if item}
              <li class="ghost" aria-hidden="true" style={`top:${ghostTop}px;width:${drag.width}px;height:${drag.height}px`}>
                <div class="row">
                  <span class="handle"><svg viewBox="0 0 10 16" width="10" height="16" aria-hidden="true"><g fill="currentColor"><circle cx="2.5" cy="3" r="1.3"/><circle cx="7.5" cy="3" r="1.3"/><circle cx="2.5" cy="8" r="1.3"/><circle cx="7.5" cy="8" r="1.3"/><circle cx="2.5" cy="13" r="1.3"/><circle cx="7.5" cy="13" r="1.3"/></g></svg></span>
                  <span class="index">{String(items.indexOf(item) + 1).padStart(2, "0")}</span>
                  <span class="row-main"><strong class:untitled={!item.title}>{item.title || original(item)}</strong><small>{item.title ? original(item) : `${sourceName(item)} · 未加标题`}</small></span>
                </div>
              </li>
            {/if}
          {/if}
        </ol>
      {/if}
    </div>
  </div>

  <div class="group">
    <span class="group-title">历史</span>
    <div class="card">
      <label class="setting"><span class="setting-copy"><strong>粘贴旧条目后移到最前</strong><small>从历史里粘贴一条之前的内容后，把它移到列表最前面。常用的顺序不受影响。</small></span>
        <input class="switch" type="checkbox" role="switch" checked={reuseToTop} {disabled}
          onchange={(event) => onsave({ history: { reuse_to_top: event.currentTarget.checked } }, event.currentTarget.checked ? "粘贴旧条目后会移到最前" : "粘贴旧条目后保持原位置")} /></label>
    </div>
  </div>
</section>

<style>
  .title-note { margin-left: 8px; font-style: normal; font-weight: 400; letter-spacing: 0; }
  .note { margin: 0; padding: 14px 16px; color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .note.error { color: var(--state-critical); border-bottom: 1px solid var(--settings-line); }
  .favorites { position: relative; margin: 0; padding: 0; list-style: none; }
  .favorites.dragging { user-select: none; cursor: grabbing; }
  .favorites > li { border-bottom: 1px solid var(--settings-line); background: var(--settings-card); }
  .favorites > li:last-of-type { border-bottom: 0; }
  .favorites > li.selected:not(.placeholder) { background: var(--settings-selected); }
  .row { display: grid; grid-template-columns: 22px 22px minmax(0, 1fr) auto; align-items: center; gap: 8px; min-height: 52px; padding: 6px 12px 6px 8px; }
  .handle { width: 22px; height: 30px; display: grid; place-items: center; padding: 0; border: 0; background: transparent; color: var(--settings-muted); cursor: grab; touch-action: none; }
  .handle:disabled { cursor: default; opacity: .4; }
  .index { color: var(--settings-muted); font: 11px var(--font-mono); }
  .row-main { min-width: 0; display: grid; gap: 3px; padding: 4px 6px; border: 0; border-radius: var(--radius-sm); background: transparent; color: inherit; text-align: left; }
  .row-main:focus-visible { outline: 2px solid var(--settings-accent); outline-offset: -2px; }
  .row-main strong, .row-main small { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .row-main strong { font-size: 13px; font-weight: 600; }
  .row-main strong.untitled { font-weight: 500; }
  .row-main small { color: var(--settings-muted); font-size: 11px; }
  .edit { min-height: 26px; padding: 2px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; }
  .open .edit { border-color: var(--settings-accent); color: var(--settings-accent); }
  .editor { display: grid; gap: 8px; padding: 2px 16px 14px 60px; }
  .editor label { display: grid; grid-template-columns: 36px minmax(0, 1fr); align-items: start; gap: 8px; color: var(--settings-muted); font-size: 11px; }
  .editor label span { padding-top: 6px; }
  .editor input, .editor textarea { width: 100%; box-sizing: border-box; padding: 5px 8px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-card); color: var(--settings-text); font: inherit; font-size: 12px; line-height: 1.6; resize: vertical; }
  .editor input:focus, .editor textarea:focus { outline: 2px solid color-mix(in srgb, var(--settings-accent) 45%, transparent); outline-offset: -1px; border-color: var(--settings-accent); }
  .hint { display: flex; gap: 4px; min-width: 0; margin: 0 0 0 44px; color: var(--settings-muted); font-size: 10px; }
  .hint .origin { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .hint span:last-child { flex: none; }
  .hint .failed { color: var(--state-critical); }
  /* 被拖动的行在原位留一个虚线框；替身浮在上面，略微倾斜、带阴影。 */
  .favorites > li.placeholder { background: transparent; }
  .favorites > li.placeholder .row { outline: 1.5px dashed color-mix(in srgb, var(--settings-accent) 60%, transparent); outline-offset: -5px; border-radius: var(--radius-md); }
  .favorites > li.placeholder .row > * { visibility: hidden; }
  .favorites > li.placeholder .editor { display: none; }
  .ghost { position: absolute; left: 0; z-index: 2; box-sizing: border-box; border: 1px solid var(--settings-accent) !important; border-radius: var(--radius-md); background: var(--settings-card); box-shadow: 0 10px 28px rgb(0 0 0 / .16), 0 2px 6px rgb(0 0 0 / .08); transform: rotate(-1.2deg) scale(1.01); pointer-events: none; }
  .ghost .handle { cursor: grabbing; }
  .settled { animation: settle 320ms cubic-bezier(.3, 1.4, .5, 1); }
  @keyframes settle { from { transform: scale(1.015); } to { transform: none; } }
</style>
