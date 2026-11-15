<script lang="ts">
  // 收藏：真实收藏顺序（E03）、标题和备注（E04）。读取与结构操作共用一个队列，
  // 每张牌的草稿由 FavoriteCard 各自串行保存；移出与撤销只调用既有收藏 IPC。
  import { onMount, tick } from "svelte";
  import { flip } from "svelte/animate";
  import * as ipc from "../lib/ipc";
  import type { HistoryItem } from "../lib/ipc";
  import { reducedMotion } from "../lib/preferences";
  import { cardKind, imageSize } from "../lib/cards";
  import { message } from "./format";
  import FavoriteCard from "./FavoriteCard.svelte";

  let { disabled }: { disabled: boolean } = $props();

  type UndoState = { item: HistoryItem; ids: number[]; error: string; busy: boolean };
  type DragState = {
    id: number; pointerId: number; startX: number; startY: number; x: number; y: number;
    grabX: number; grabY: number; width: number; height: number; moved: boolean; before: HistoryItem[];
  };

  let items = $state<HistoryItem[] | null>(null);
  let loadError = $state("");
  let orderError = $state("");
  let orderPhase = $state<"idle" | "saving" | "saved" | "failed">("idle");
  let orderTimer: ReturnType<typeof setTimeout> | undefined;
  let undo = $state<UndoState | null>(null);
  let cards = $state<Record<number, FavoriteCard | null | undefined>>({});
  let openCards = $state<Record<number, boolean>>({});
  let busyCards = $state<Record<number, boolean>>({});
  let grid = $state<HTMLOListElement>();
  let undoButton = $state<HTMLButtonElement>();
  let emptyState = $state<HTMLParagraphElement>();
  let drag = $state<DragState | null>(null);
  let memberBusy = $state(false);
  let disposed = false;
  let structural: Promise<void> = Promise.resolve();

  const liveCards = () => Object.values(cards).filter((card): card is FavoriteCard => !!card);

  function serialize(operation: () => Promise<void>) {
    const run = async () => {
      try { await operation(); }
      catch (value) {
        if (!disposed) {
          orderPhase = "failed";
          orderError = `操作没有完成，请重试 · ${message(value)}`;
        }
      }
    };
    const result = structural.then(run);
    structural = result.then(() => undefined);
    return result;
  }

  async function load() {
    try {
      items = await ipc.getFavorites();
      loadError = "";
    } catch (value) {
      loadError = `没读到收藏，关掉窗口再开试试 · ${message(value)}`;
    }
  }

  onMount(() => {
    disposed = false;
    void serialize(load);
    const refresh = () => {
      if (!drag && !liveCards().some((card) => card.isOpen())) void serialize(load);
    };
    window.addEventListener("focus", refresh);
    return () => {
      disposed = true;
      window.removeEventListener("focus", refresh);
      if (orderTimer !== undefined) clearTimeout(orderTimer);
    };
  });

  function original(item: HistoryItem) {
    const kind = cardKind(item);
    if (kind === "image") return `图片${imageSize(item) ? ` · ${imageSize(item)}` : ""}`;
    if (kind === "file") return item.file_names.join("、") || item.preview || "文件";
    return item.preview || "（空文本）";
  }

  function savedLabel(updated: HistoryItem) {
    if (items) items = items.map((entry) => entry.id === updated.id ? updated : entry);
  }

  function flipped(id: number, value: boolean) {
    openCards[id] = value;
  }

  async function flushDrafts() {
    const results = await Promise.all(liveCards().map((card) => card.flush()));
    return results.every(Boolean);
  }

  function orderSaved() {
    if (orderTimer !== undefined) clearTimeout(orderTimer);
    orderPhase = "saved";
    orderError = "";
    orderTimer = setTimeout(() => {
      if (orderPhase === "saved") orderPhase = "idle";
      orderTimer = undefined;
    }, 1500);
  }

  async function reloadMembers(fallback: HistoryItem[]) {
    try { items = await ipc.getFavorites(); }
    catch { items = fallback; }
  }

  async function commitOrder(next: HistoryItem[], before: HistoryItem[], force = false) {
    const ids = next.map((item) => item.id);
    if (!force && ids.join() === before.map((item) => item.id).join()) return true;
    orderPhase = "saving";
    orderError = "";
    try {
      items = await ipc.reorderFavorites(ids);
      orderSaved();
      return true;
    } catch (value) {
      await reloadMembers(before);
      orderPhase = "failed";
      orderError = `顺序没保存上，再拖一次 · ${message(value)}`;
      return false;
    }
  }

  function move(id: number, delta: number) {
    if (drag || memberBusy) return;
    void serialize(async () => {
      if (!items || disabled || !(await flushDrafts())) return;
      const from = items.findIndex((item) => item.id === id);
      const to = from + delta;
      if (from < 0 || to < 0 || to >= items.length) return;
      const before = [...items];
      const next = [...items];
      next.splice(to, 0, ...next.splice(from, 1));
      items = next;
      await tick();
      grid?.querySelector<HTMLElement>(`[data-id="${id}"] [data-favorite-front]`)?.focus();
      await commitOrder(next, before);
    });
  }

  function frontKey(event: KeyboardEvent, id: number) {
    if (event.altKey && !event.ctrlKey && !event.shiftKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      move(id, event.key === "ArrowUp" ? -1 : 1);
    } else if (!event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown") && items) {
      event.preventDefault();
      const at = items.findIndex((entry) => entry.id === id) + (event.key === "ArrowUp" ? -1 : 1);
      const target = items[at];
      if (target) grid?.querySelector<HTMLElement>(`[data-id="${target.id}"] [data-favorite-front]`)?.focus();
    }
  }

  function startDrag(event: PointerEvent, id: number) {
    if (event.button !== 0 || disabled || memberBusy || !items || !grid || drag) return;
    event.preventDefault();
    const front = event.currentTarget as HTMLElement;
    front.setPointerCapture?.(event.pointerId);
    const rect = front.getBoundingClientRect();
    drag = {
      id, pointerId: event.pointerId, startX: event.clientX, startY: event.clientY,
      x: event.clientX, y: event.clientY, grabX: event.clientX - rect.left, grabY: event.clientY - rect.top,
      width: rect.width, height: rect.height, moved: false, before: [...items],
    };
  }

  function moveDrag(event: PointerEvent) {
    if (!drag || !items || event.pointerId !== drag.pointerId) return;
    drag.x = event.clientX;
    drag.y = event.clientY;
    if (!drag.moved && Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) < 6) return;
    drag.moved = true;
    const target = document.elementsFromPoint(event.clientX, event.clientY)
      .map((node) => node.closest<HTMLElement>("[data-favorite-card][data-id]"))
      .find((node) => node && Number(node.dataset.id) !== drag!.id);
    const targetId = Number(target?.dataset.id);
    if (!targetId) return;
    const from = items.findIndex((entry) => entry.id === drag!.id);
    const to = items.findIndex((entry) => entry.id === targetId);
    if (from < 0 || to < 0 || from === to) return;
    const next = [...items];
    next.splice(to, 0, ...next.splice(from, 1));
    items = next;
  }

  function endDrag(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    const finished = drag;
    drag = null;
    if (!finished.moved) {
      void cards[finished.id]?.open();
      return;
    }
    void serialize(async () => {
      if (!items || !(await flushDrafts())) {
        await reloadMembers(finished.before);
        return;
      }
      await commitOrder(items, finished.before);
    });
  }

  function remove(id: number) {
    if (disabled || memberBusy || busyCards[id] || !items?.some((item) => item.id === id)) return;
    if (drag) {
      orderPhase = "failed";
      orderError = "先结束拖动，再移出收藏或撤销";
      return;
    }
    // 从点击同步锁牌，之后 flush、reorder、setFavorite 的任何在途窗口都不接受晚到编辑。
    busyCards[id] = true;
    memberBusy = true;
    void serialize(async () => {
      let removed = false;
      try {
        if (!items || !(await flushDrafts())) return;
        const before = [...items];
        const ids = before.map((item) => item.id);
        // 即使顺序没变也先写完整列表：这是后续撤销要恢复的快照。
        if (!(await commitOrder(before, before, true))) return;
        await ipc.setFavorite(id, false);
        const item = items.find((entry) => entry.id === id);
        if (!item) throw new Error("没有找到要移出的收藏");
        openCards[id] = false;
        items = items.filter((entry) => entry.id !== id);
        undo = { item, ids, error: "", busy: false };
        orderPhase = "idle";
        removed = true;
        delete busyCards[id];
        memberBusy = false;
        await tick();
        const nextFront = grid?.querySelector<HTMLElement>("[data-favorite-front]");
        if (nextFront) nextFront.focus();
        else if (undoButton) undoButton.focus();
        else emptyState?.focus();
      } catch (value) {
        orderPhase = "failed";
        orderError = `没能移出收藏，再试一次 · ${message(value)}`;
      } finally {
        memberBusy = false;
        if (!removed) busyCards[id] = false;
      }
    });
  }

  function restore() {
    const snapshot = undo;
    if (!snapshot || snapshot.busy || memberBusy) return;
    if (drag) {
      orderPhase = "failed";
      orderError = "先结束拖动，再移出收藏或撤销";
      return;
    }
    memberBusy = true;
    undo = { ...snapshot, busy: true, error: "" };
    void serialize(async () => {
      try {
        try {
          // 顺序不能反：先让 ID 回到收藏集合，再提交包含它的原完整列表。
          await ipc.setFavorite(snapshot.item.id, true);
        } catch (value) {
          undo = { ...snapshot, busy: false, error: `撤销没完成，再试一次 · ${message(value)}` };
          return;
        }
        try {
          items = await ipc.reorderFavorites(snapshot.ids);
          undo = null;
          orderSaved();
          await tick();
          grid?.querySelector<HTMLElement>(`[data-id="${snapshot.item.id}"] [data-favorite-front]`)?.focus();
        } catch (value) {
          // 收藏可能已经恢复、顺序可能仍在末尾；重读真实状态，不能把快照当成事实。
          await reloadMembers(items ?? []);
          undo = { ...snapshot, busy: false, error: `撤销没完成，再试一次 · ${message(value)}` };
        }
      } finally {
        memberBusy = false;
        if (undo?.item.id === snapshot.item.id && undo.busy) undo = { ...undo, busy: false };
      }
    });
  }
</script>

<svelte:window onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag} />

<section class="page favorite-page">
  <div class="favorite-heading">
    <div><h2>收藏</h2><p class="page-intro">呼出后按 ↑ 就是这一手。排好顺序，手就能记住。</p></div>
    <span class="order-status" data-order-status data-phase={orderPhase} role={orderPhase === "failed" ? "alert" : "status"}>
      {#if orderPhase === "saving"}正在保存…{:else if orderPhase === "saved"}✓ 已保存{:else if orderPhase === "failed"}{orderError}{/if}
    </span>
  </div>

  <div class="favorite-help"><span>拖动换位置，数字跟着位置走</span><span>点一张翻到背面，写标题和备注</span><span>选中后 Alt+↑ / Alt+↓ 也能换位置</span></div>

  {#if undo}
    <div class="undo" class:failed={!!undo.error} data-favorite-undo role={undo.error ? "alert" : "status"}>
      <span>{undo.error || "已移出收藏 ·"}</span>
      <button type="button" bind:this={undoButton} data-action="undo-favorite" disabled={undo.busy || memberBusy || !!drag} onclick={restore}>撤销</button>
    </div>
  {/if}

  {#if loadError}<p class="load-error" role="alert">{loadError}</p>{/if}
  {#if orderError && orderPhase !== "failed"}<p class="load-error" role="alert">{orderError}</p>{/if}
  {#if items === null}
    {#if !loadError}<p class="empty">正在读取…</p>{/if}
  {:else if !items.length}
    <p class="empty" data-favorite-empty bind:this={emptyState} tabindex="-1">还没有收藏的。呼出后选一张，按 Ctrl+D</p>
  {:else}
    <ol class="favorite-grid" data-favorite-grid bind:this={grid} aria-label="收藏" aria-busy={memberBusy} class:dragging={!!drag?.moved}>
      {#each items as item, index (item.id)}
        <li
          data-favorite-card
          data-id={item.id}
          data-kind={cardKind(item)}
          data-flipped={openCards[item.id] ? "true" : "false"}
          aria-posinset={index + 1}
          aria-setsize={items.length}
          class:drag-source={drag?.id === item.id && drag.moved}
          animate:flip={{ duration: drag || reducedMotion() || document.documentElement.dataset.motion === "none" ? 0 : 200 }}
        >
          <FavoriteCard
            bind:this={cards[item.id]}
            {item} {index} {disabled}
            dragging={drag?.id === item.id && drag.moved}
            busy={!!busyCards[item.id]}
            structureDisabled={!!drag || memberBusy}
            onsaved={savedLabel}
            onremove={remove}
            onflip={flipped}
            onfrontkey={frontKey}
            onstartdrag={startDrag}
          />
        </li>
      {/each}
    </ol>
  {/if}
  <p class="favorite-rule">数字对应当前位置。复制、粘贴不会改变收藏顺序，原内容也不能在这里改。</p>
</section>

{#if drag?.moved}
  {@const item = items?.find((entry) => entry.id === drag?.id)}
  {#if item}
    <div class="drag-ghost" aria-hidden="true" style={`transform:translate(${drag.x - drag.grabX}px,${drag.y - drag.grabY}px);width:${drag.width}px;height:${drag.height}px`}>
      <span>{(items?.findIndex((entry) => entry.id === item.id) ?? -1) + 1}</span><strong>{item.title || original(item)}</strong><small>松开保存位置</small>
    </div>
  {/if}
{/if}

<style>
  .favorite-page { gap: 16px; }
  .favorite-heading { display: flex; align-items: start; gap: 16px; }
  .favorite-heading > div { min-width: 0; }
  .order-status { max-width: 280px; margin-left: auto; padding-top: 4px; color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; text-align: right; }
  .order-status[data-phase="saved"] { color: var(--state-success); }
  .order-status[data-phase="failed"] { color: var(--state-critical); text-align: left; }
  .favorite-help { display: flex; flex-wrap: wrap; gap: 4px 16px; color: var(--hand-ink-2); font-size: 12px; line-height: 20px; }
  .favorite-help span + span::before { content: "·"; margin-right: 16px; }
  .favorite-grid { display: grid; grid-template-columns: repeat(4, minmax(136px, 1fr)); gap: 16px; margin: 0; padding: 4px; list-style: none; }
  .favorite-grid.dragging { user-select: none; cursor: grabbing; }
  .favorite-grid > li { min-width: 0; }
  .favorite-grid > li.drag-source { outline: 1px dashed var(--hand-seal); outline-offset: 4px; border-radius: 8px; }
  .favorite-grid > li.drag-source :global(.favorite-card) { opacity: .25; }
  .empty, .load-error { margin: 0; padding: 24px; border: 1px solid var(--hand-line); border-radius: 8px; background: var(--hand-card); color: var(--hand-ink-2); font-size: 13px; line-height: 1.7; }
  .load-error { border-color: var(--state-critical); color: var(--state-critical); }
  .favorite-rule { margin: 0; color: var(--hand-ink-2); font-size: 12px; line-height: 1.7; }
  .undo { display: flex; align-items: center; gap: 8px; width: fit-content; min-height: 32px; padding: 4px 4px 4px 12px; border-radius: 4px; background: var(--hand-ink); color: var(--hand-paper); font-size: 12px; }
  .undo.failed { background: var(--state-critical-bg); color: var(--state-critical); outline: 1px solid var(--state-critical); }
  .undo button { min-height: 24px; padding: 0 8px; border: 1px solid currentColor; border-radius: 4px; background: transparent; color: inherit; font-size: 12px; }
  .drag-ghost { position: fixed; left: 0; top: 0; z-index: 100; display: flex; flex-direction: column; gap: 8px; padding: 16px; border: 1px solid var(--hand-seal); border-radius: 8px; background: var(--hand-card); box-shadow: var(--hand-lift); pointer-events: none; transform-origin: center; }
  .drag-ghost > span { width: 24px; height: 24px; border-radius: 4px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 15px/24px var(--font-ui); text-align: center; }
  .drag-ghost strong { overflow: hidden; font-size: 13px; text-overflow: ellipsis; white-space: nowrap; }
  .drag-ghost small { margin-top: auto; color: var(--hand-ink-2); font-size: 12px; }
  @media (max-width: 1100px) { .favorite-grid { grid-template-columns: repeat(3, minmax(136px, 1fr)); } }
  @media (max-width: 900px) { .favorite-grid { grid-template-columns: repeat(2, minmax(136px, 1fr)); } }
  @media (forced-colors: active) {
    .undo, .undo.failed { background: Canvas; color: CanvasText; outline: 1px solid CanvasText; }
    .drag-ghost { border-color: Highlight; }
  }
</style>
