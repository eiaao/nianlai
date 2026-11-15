<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { HistoryItem } from "../lib/ipc";
  import { KIND_NAME, cardKind, fileExt, imageSize, linkParts } from "../lib/cards";
  import { message } from "./format";
  import SaveMark, { type SaveMarkState } from "./SaveMark.svelte";

  const TITLE_MAX = 200;
  const NOTE_MAX = 2000;
  const IDLE: SaveMarkState = { phase: "idle", message: "", error: "" };

  let { item, index, disabled = false, dragging = false, busy = false, structureDisabled = false, onsaved = () => {}, onremove = () => {}, onflip = () => {}, onfrontkey = () => {}, onstartdrag = () => {} }: {
    item: HistoryItem;
    index: number;
    disabled?: boolean;
    dragging?: boolean;
    busy?: boolean;
    structureDisabled?: boolean;
    onsaved?: (item: HistoryItem) => void;
    onremove?: (id: number) => void;
    onflip?: (id: number, flipped: boolean) => void;
    onfrontkey?: (event: KeyboardEvent, id: number) => void;
    onstartdrag?: (event: PointerEvent, id: number) => void;
  } = $props();

  let flipped = $state(false);
  let draft = $state(untrack(() => ({ title: item.title ?? "", note: item.note ?? "" })));
  let saveState = $state<SaveMarkState>({ ...IDLE });
  let frontButton = $state<HTMLButtonElement>();
  let titleInput = $state<HTMLInputElement>();
  let noteInput = $state<HTMLTextAreaElement>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let savedTimer: ReturnType<typeof setTimeout> | undefined;
  let revision = 0;
  let writeRevision = 0;
  let chain: Promise<void> = Promise.resolve();
  let destroyed = false;

  const kind = $derived(cardKind(item));
  const original = $derived.by(() => {
    if (kind === "image") return `图片${imageSize(item) ? ` · ${imageSize(item)}` : ""}`;
    if (kind === "file") return item.file_names.join("、") || item.preview || "文件";
    return item.preview || "（空文本）";
  });
  const link = $derived(kind === "link" ? linkParts(item.preview) : null);

  function clearTimer() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }
  function clearSavedTimer() {
    if (savedTimer !== undefined) clearTimeout(savedTimer);
    savedTimer = undefined;
  }
  function showSaved() {
    clearSavedTimer();
    saveState = { phase: "saved", message: "标题和备注已保存", error: "" };
    savedTimer = setTimeout(() => {
      if (saveState.phase === "saved") saveState = { ...IDLE };
      savedTimer = undefined;
    }, 1500);
  }

  // 同一张牌只有一个 IPC 队列。真正出队时才捕获 revision 与完整快照：A 在途、B/C 排队时，
  // B 发送最新的 C 并记 C 为已写版本，后续同 revision 调用直接合并，不重复写也不制造假失败。
  // destroyed 只禁止 UI 更新；导航销毁前未写的快照仍必须进入真实 IPC。
  function queueSave(): Promise<boolean> {
    clearTimer();
    const id = item.id;
    const attempt = chain.then(async () => {
      if (revision === writeRevision) return true;
      const revisionAtStart = revision;
      const snapshot = { title: draft.title, note: draft.note };
      if (!destroyed) saveState = { phase: "saving", message: "标题和备注已保存", error: "" };
      try {
        const updated = await ipc.setFavoriteLabel(id, snapshot.title, snapshot.note);
        writeRevision = revisionAtStart;
        if (destroyed) return true;
        onsaved(updated);
        if (revision === revisionAtStart) {
          // 后端会清理首尾空白；没有更新输入时才把清理后的值映回控件。
          draft = { title: updated.title ?? "", note: updated.note ?? "" };
          showSaved();
        } else {
          saveState = { ...IDLE };
        }
        return true;
      } catch (value) {
        if (!destroyed) saveState = {
          phase: "failed", message: "",
          error: `没保存上：标题没保存上，再试一次 · ${message(value)}`,
        };
        return false;
      }
    });
    chain = attempt.then(() => undefined);
    return attempt;
  }

  function schedule() {
    clearSavedTimer();
    saveState = { ...IDLE };
    clearTimer();
    timer = setTimeout(() => { timer = undefined; void queueSave(); }, 500);
  }

  function edit(field: "title" | "note", value: string) {
    if (disabled || busy) return;
    // 程序化赋值也遵守实际上限；正常键入同时受原生 maxlength 约束。
    draft[field] = Array.from(value).slice(0, field === "title" ? TITLE_MAX : NOTE_MAX).join("");
    revision += 1;
    schedule();
  }

  export function flush(): Promise<boolean> {
    return queueSave().then(async (saved) => saved && (await chain, true));
  }

  export async function open() {
    if (flipped || disabled || busy) return;
    flipped = true;
    onflip(item.id, true);
    await tick();
    titleInput?.focus({ preventScroll: true });
  }

  export async function close(): Promise<boolean> {
    if (!flipped) return true;
    const saved = await flush();
    if (!saved) {
      titleInput?.focus({ preventScroll: true });
      return false;
    }
    flipped = false;
    onflip(item.id, false);
    await tick();
    frontButton?.focus({ preventScroll: true });
    return true;
  }

  export function isOpen() { return flipped; }

  function frontKey(event: KeyboardEvent) {
    onfrontkey(event, item.id);
    if (event.defaultPrevented) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      void open();
    }
  }

  onDestroy(() => {
    clearTimer();
    clearSavedTimer();
    // DOM 移除不会补发 focusout。同步把最新 revision 挂到既有队列，销毁后仍完成真实写入。
    if (revision !== writeRevision) void queueSave();
    destroyed = true;
  });
</script>

<div class="favorite-card" data-flipped={flipped ? "true" : "false"}>
  <div class="favorite-flipper">
    <button
      type="button"
      class="favorite-front"
      data-favorite-front
      data-kind={kind}
      bind:this={frontButton}
      disabled={disabled || busy}
      aria-label={`${item.title || original}，第 ${index + 1} 个收藏，${KIND_NAME[kind]}，点一下编辑标题和备注`}
      aria-grabbed={dragging ? "true" : "false"}
      inert={flipped}
      onpointerdown={(event) => onstartdrag(event, item.id)}
      onkeydown={frontKey}
      onclick={(event) => { if (event.detail === 0) void open(); }}
    >
      <span class="favorite-position" aria-hidden="true">{index + 1}</span>
      <span class="favorite-type">{KIND_NAME[kind]}</span>
      <span class="favorite-seal" aria-label="收藏">藏</span>
      <span class="favorite-title" class:untitled={!item.title}>{item.title || "没起标题"}</span>
      <span class="favorite-content" data-kind={kind}>
        {#if kind === "image"}
          <span class="image-mark" aria-hidden="true"></span><span>{original}</span>
        {:else if kind === "file"}
          <strong>{item.file_names[0] ? fileExt(item.file_names[0]) : "文件"}</strong>
          <span>{item.file_names[0] || item.preview}</span>
          {#if item.file_names.length > 1}<small>等 {item.file_names.length} 项</small>{/if}
        {:else if kind === "link" && link}
          <strong>{link.host}</strong><span>{link.prefix}{link.tail}</span>
        {:else}
          {item.preview || "（空文本）"}
        {/if}
      </span>
      <span class="favorite-foot">{item.note || "点一下写标题和备注"}</span>
    </button>

    <section class="favorite-back" inert={!flipped} aria-busy={busy} aria-label={`${item.title || original} 的标题和备注`}>
      <header><span class="favorite-position">{index + 1}</span><strong>写标题和备注</strong>
        <button type="button" class="close" data-action="close-card" aria-label="返回正面" disabled={disabled || busy} onclick={() => void close()}>返回</button>
      </header>
      <label><span>标题</span><input bind:this={titleInput} data-favorite-field="title" type="text" value={draft.title} maxlength={TITLE_MAX}
        placeholder="比如：家里地址" disabled={disabled || busy} oninput={(event) => edit("title", event.currentTarget.value)}
        onfocusout={() => { void queueSave(); }} /></label>
      <label><span>备注</span><textarea bind:this={noteInput} data-favorite-field="note" rows="2" value={draft.note} maxlength={NOTE_MAX}
        placeholder="什么时候用、给谁用" disabled={disabled || busy} oninput={(event) => edit("note", event.currentTarget.value)}
        onfocusout={() => { void queueSave(); }}></textarea></label>
      <p class="original" title={original}><span>原内容不能改</span><b>{original}</b></p>
      <div class="back-actions">
        <button type="button" class="remove" data-action="remove-favorite" disabled={disabled || busy || structureDisabled} onclick={() => onremove(item.id)}>移出收藏</button>
        {#if saveState.phase === "failed"}
          <button type="button" class="retry" data-action="retry-label" disabled={disabled || busy} onclick={() => void queueSave()}>再试一次</button>
        {/if}
      </div>
      <div class="save"><SaveMark state={saveState} /></div>
    </section>
  </div>
</div>

<style>
  .favorite-card { width: 100%; height: 224px; perspective: 900px; }
  .favorite-flipper { position: relative; width: 100%; height: 100%; transform-style: preserve-3d; transition: transform 360ms cubic-bezier(.2,.8,.2,1); }
  .favorite-card[data-flipped="true"] .favorite-flipper { transform: rotateY(180deg); }
  .favorite-front, .favorite-back { position: absolute; inset: 0; width: 100%; height: 100%; border: 1px solid var(--hand-line-strong); border-radius: 8px; background: var(--hand-card); box-shadow: var(--hand-card-shadow); backface-visibility: hidden; -webkit-backface-visibility: hidden; }
  .favorite-front { display: block; padding: 0; color: var(--hand-ink); text-align: left; cursor: grab; touch-action: none; overflow: hidden; }
  .favorite-front:active { cursor: grabbing; }
  .favorite-front:focus-visible { outline: 2px solid var(--hand-seal); outline-offset: 3px; }
  .favorite-position { position: absolute; left: 8px; top: 8px; min-width: 24px; height: 24px; padding: 0 4px; border-radius: 4px; background: var(--hand-key); color: var(--hand-ink); box-shadow: inset 0 0 0 1px var(--hand-key-line); font: 600 15px/24px var(--font-ui); text-align: center; font-variant-numeric: tabular-nums; }
  .favorite-type { position: absolute; left: 40px; top: 12px; color: var(--hand-ink-3); font-size: 12px; line-height: 16px; }
  .favorite-seal { position: absolute; right: 8px; top: 8px; width: 18px; height: 18px; border-radius: 4px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 12px/18px var(--font-display); text-align: center; }
  .favorite-title { position: absolute; left: 12px; right: 12px; top: 44px; overflow: hidden; color: var(--hand-ink-2); font-size: 13px; font-weight: 600; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; }
  .favorite-title.untitled { color: var(--hand-ink-3); font-weight: 400; }
  .favorite-content { position: absolute; left: 12px; right: 12px; top: 72px; bottom: 36px; display: block; overflow: hidden; color: var(--hand-ink); font-size: 13px; line-height: 20px; overflow-wrap: anywhere; white-space: pre-wrap; }
  .favorite-content strong, .favorite-content span, .favorite-content small { display: block; }
  .favorite-content strong { font-size: 15px; line-height: 20px; }
  .favorite-content small { color: var(--hand-ink-2); font-size: 12px; }
  .favorite-content[data-kind="link"] span, .favorite-content[data-kind="file"] span { color: var(--hand-ink-2); font-size: 12px; }
  .image-mark { height: 52px; margin-bottom: 8px; border-radius: 4px; background: linear-gradient(160deg, var(--hand-card-back), var(--hand-line-strong)); }
  .favorite-foot { position: absolute; left: 12px; right: 12px; bottom: 8px; overflow: hidden; color: var(--hand-ink-3); font-size: 12px; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; }
  .favorite-back { display: flex; flex-direction: column; gap: 4px; padding: 8px; transform: rotateY(180deg); color: var(--hand-ink); }
  .favorite-back header { position: relative; display: flex; align-items: center; min-height: 24px; padding-left: 32px; }
  .favorite-back header .favorite-position { left: 0; top: 0; background: var(--hand-seal); color: var(--hand-on-seal); box-shadow: none; }
  .favorite-back header strong { font-size: 12px; font-weight: 600; }
  .close { margin-left: auto; padding: 0 0 0 4px; border: 0; background: transparent; color: var(--hand-ink-2); font-size: 12px; }
  .favorite-back label { display: grid; grid-template-columns: 28px minmax(0,1fr); align-items: center; gap: 4px; color: var(--hand-ink-2); font-size: 12px; }
  .favorite-back input, .favorite-back textarea { width: 100%; min-width: 0; margin: 0; padding: 4px 8px; border: 1px solid var(--hand-line); border-radius: 4px; background: var(--hand-paper); color: var(--hand-ink); font: 13px/20px var(--font-ui); resize: none; }
  .favorite-back textarea { height: 48px; font-size: 12px; line-height: 16px; }
  .original { min-width: 0; margin: 0; color: var(--hand-ink-3); font-size: 12px; line-height: 16px; }
  .original b { display: block; overflow: hidden; color: var(--hand-ink-2); font-weight: 400; white-space: nowrap; text-overflow: ellipsis; }
  .back-actions { display: flex; align-items: center; gap: 8px; margin-top: auto; }
  .back-actions button { min-height: 24px; padding: 0; border: 0; background: transparent; font-size: 12px; }
  .remove { color: var(--state-critical); }
  .retry { color: var(--hand-seal); }
  .save { min-height: 20px; display: flex; align-items: center; }
  .save :global(.save-mark) { min-width: 0; justify-content: flex-start; font-size: 12px; }

  /* 减少动画不是禁用编辑：直接交换正反面，不保留 3D 变换和过渡。 */
  :global(html[data-motion="none"]) .favorite-flipper,
  :global(html[data-reduce-motion="true"]) .favorite-flipper { transform: none !important; transition: none; }
  :global(html[data-motion="none"]) .favorite-front,
  :global(html[data-motion="none"]) .favorite-back,
  :global(html[data-reduce-motion="true"]) .favorite-front,
  :global(html[data-reduce-motion="true"]) .favorite-back { transform: none; backface-visibility: visible; -webkit-backface-visibility: visible; }
  :global(html[data-motion="none"]) .favorite-card[data-flipped="false"] .favorite-back,
  :global(html[data-reduce-motion="true"]) .favorite-card[data-flipped="false"] .favorite-back { display: none; }
  :global(html[data-motion="none"]) .favorite-card[data-flipped="true"] .favorite-front,
  :global(html[data-reduce-motion="true"]) .favorite-card[data-flipped="true"] .favorite-front { display: none; }
  @media (forced-colors: active) {
    .favorite-front, .favorite-back, .favorite-position, .favorite-back input, .favorite-back textarea { border: 1px solid CanvasText; box-shadow: none; }
    .favorite-seal, .favorite-back header .favorite-position { background: Highlight; color: HighlightText; }
    .favorite-front:focus-visible { outline-color: Highlight; }
  }
</style>
