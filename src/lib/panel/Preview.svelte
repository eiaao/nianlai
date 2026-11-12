<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import * as ipc from "../ipc";
  import type { HistoryItem, ImageView, PathCheck, TextChunk } from "../ipc";
  import { count } from "../format";
  import { formatTime, type PanelState } from "../state.svelte";
  import { cardKind, colorValue, linkParts, relativeTime, sourceName } from "../cards";
  import { SpringValue } from "./motion";

  let { panel, item, expanded = false, compact = false }: { panel: PanelState; item: HistoryItem | null; expanded?: boolean; compact?: boolean } = $props();
  let root: HTMLDivElement;
  let previousId: number | null | undefined;
  const kind = $derived(item ? cardKind(item) : "short");
  const rich = $derived(item?.kind === "富文本");
  const link = $derived(item ? linkParts(item.preview ?? "") : null);
  const color = $derived(item ? colorValue(item.preview ?? "") : null);
  const y = new SpringValue(0, (value) => { if (root) root.style.transform = `translate3d(0, ${value}px, 0)`; }, () => panel.motion);
  const opacity = new SpringValue(1, (value) => { if (root) root.style.opacity = String(Math.max(0, Math.min(1, value))); }, () => panel.motion, 0.22);

  $effect(() => {
    const id = item?.id ?? null;
    panel.motion;
    void tick().then(() => {
      if (!root) return;
      if (previousId === undefined || previousId === id || panel.motion === "none") {
        y.reset(0); opacity.reset(1);
      } else {
        y.nudge(previousId !== null && id !== null && id > previousId ? 7 : -7);
        opacity.nudge(0.68);
        y.to(0); opacity.to(1);
      }
      previousId = id;
    });
  });

  // 文件条目：现查每个路径还在不在（M2-3）。切换条目时旧的结果作废。
  let paths = $state<PathCheck[] | null>(null);
  let pathsFor = -1;
  $effect(() => {
    const id = item?.id ?? -1;
    const files = kind === "file";
    if (id === pathsFor) return;
    pathsFor = id;
    paths = null;
    if (!files) return;
    void ipc.getFilePaths(id).then((result) => { if (pathsFor === id) paths = result; }).catch(() => {});
  });
  const mark = $derived(item?.file_state ?? null);

  // 图片（M2-7）：常驻预览用长边 480 的缩略图；完整预览（expanded）用原图。切换条目时旧的结果作废。
  let image = $state<ImageView | null>(null);
  let imageFailed = $state(false);
  let imageFor = "";
  $effect(() => {
    const key = item && kind === "image" ? `${item.id}:${expanded ? "full" : "thumb"}` : "";
    if (key === imageFor) return;
    imageFor = key;
    image = null; imageFailed = false;
    if (!key || !item) return;
    void ipc.getImage(item.id, expanded ? null : 480)
      .then((result) => { if (imageFor === key) { image = result; imageFailed = !result; } })
      .catch(() => { if (imageFor === key) imageFailed = true; });
  });
  const fit = $derived(panel.previewZoom === "fit");
  let stage = $state<HTMLDivElement>();
  let stageSize = $state({ width: 0, height: 0 });
  const scale = $derived(image && stageSize.width ? Math.min(1, stageSize.width / image.width, stageSize.height / image.height) : 1);
  function measure(node: HTMLElement) {
    const observer = new ResizeObserver(() => { stageSize = { width: node.clientWidth - 2, height: node.clientHeight - 2 }; });
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
  }
  // 原尺寸时按住拖动平移。
  function pan(node: HTMLElement) {
    let start: { x: number; y: number; left: number; top: number } | null = null;
    const down = (event: PointerEvent) => {
      if (fit || event.button !== 0) return;
      start = { x: event.clientX, y: event.clientY, left: node.scrollLeft, top: node.scrollTop };
      node.setPointerCapture(event.pointerId);
      event.preventDefault();
    };
    const move = (event: PointerEvent) => {
      if (!start) return;
      node.scrollLeft = start.left - (event.clientX - start.x);
      node.scrollTop = start.top - (event.clientY - start.y);
    };
    const up = () => { start = null; };
    node.addEventListener("pointerdown", down);
    node.addEventListener("pointermove", move);
    node.addEventListener("pointerup", up);
    node.addEventListener("pointercancel", up);
    return { destroy: () => { node.removeEventListener("pointerdown", down); node.removeEventListener("pointermove", move); node.removeEventListener("pointerup", up); node.removeEventListener("pointercancel", up); } };
  }

  // 全文（M2-7）：完整预览里先取前 2,000 字，滚到底再取其余。列表里的预览只有前 300 字。
  const FIRST_CHUNK = 2000;
  let full = $state<TextChunk | null>(null);
  let fullFor = -1;
  let loadingRest = false;
  const textual = $derived(kind !== "image" && kind !== "file");
  $effect(() => {
    const id = expanded && item && textual ? item.id : -1;
    if (id === fullFor) return;
    fullFor = id; full = null; loadingRest = false;
    if (id < 0) return;
    void ipc.getFullText(id, FIRST_CHUNK).then((chunk) => { if (fullFor === id) full = chunk; }).catch(() => {});
  });
  // 完整预览的头部按全文的字数显示（列表里的字数只算了摘要）。
  $effect(() => {
    if (!expanded) return;
    panel.fullChars = full?.total ?? null;
  });
  function loadRest(node: HTMLElement) {
    const observer = new IntersectionObserver((entries) => {
      const id = fullFor;
      if (!entries.some((entry) => entry.isIntersecting) || !full || full.shown >= full.total || loadingRest) return;
      loadingRest = true;
      void ipc.getFullText(id, null).then((chunk) => { if (fullFor === id) full = chunk; }).catch(() => {}).finally(() => { loadingRest = false; });
    });
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
  }
  function size(bytes: number) {
    if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
    return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  }
  const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

  onDestroy(() => { y.destroy(); opacity.destroy(); });
</script>

<div bind:this={root} class="preview-swap" class:expanded class:compact>
  {#if !item}
    <div class="empty-preview">
      <strong>{panel.query ? "没有可看的匹配项" : "还没有复制过东西"}</strong>
      <p>{panel.query ? "试试更短的词；Esc 清空搜索。" : "复制文字、图片或文件，它们会按时间出现在这里。"}</p>
    </div>
  {:else}
    <div class="preview-eyebrow">
      <span>{sourceName(item)} · {relativeTime(item.copied_ms)}</span>
    </div>
    {#if item.favorite && (item.title || item.note)}
      <!-- M2-6：常用的标题和备注在设置“整理”页里编辑，这里只显示。 -->
      <div class="label-block">
        {#if item.title}<strong>{item.title}</strong>{/if}
        {#if item.note}<p>{item.note}</p>{/if}
      </div>
    {/if}

    {#if kind === "image"}
      {#if expanded}
        <div class="image-stage" class:original={!fit} bind:this={stage} use:measure use:pan role="img" aria-label={item.preview || "图片记录"}>
          {#if image}
            <img src={image.url} alt="" draggable="false" style={fit ? `width:${image.width * scale}px;height:${image.height * scale}px` : `width:${image.width}px;height:${image.height}px`} />
          {:else if imageFailed}
            <span class="image-missing">这张图片没法显示</span>
          {/if}
        </div>
        {#if image}<div class="image-meta"><span>{image.width} × {image.height} · {size(image.bytes)}</span><span>{fit ? `适应 · ${Math.round(scale * 100)}%` : "原图 · 100%"}</span></div>{/if}
      {:else}
        <div class="image-frame" role="img" aria-label={item.preview || "图片记录"}>
          {#if image}<img src={image.url} alt="" draggable="false" />{:else if imageFailed}<span class="image-missing">这张图片没法显示</span>{/if}
        </div>
        <div class="image-meta"><span>{image ? `${image.width} × ${image.height} · ${size(image.bytes)}` : item.preview}</span><span>{formatTime(item.copied_ms)}</span></div>
      {/if}
    {:else if kind === "color" && color}
      <div class="color-block" role="img" style={`background-color: ${color.hex}`} aria-label={`颜色 ${color.value}`}></div>
      <div class="color-value">{color.value}</div>
      <div class="color-channels">HEX　{color.hex}<br>RGB　{color.rgb.join(", ")}<br>HSL　{color.hsl[0]}°, {color.hsl[1]}%, {color.hsl[2]}%</div>
      <p class="preview-note">取用时粘贴原始颜色值，不会自动转换格式。</p>
    {:else if kind === "link" && link}
      <div class="link-host">{link.host}</div>
      <div class="link-url">{link.href}</div>
      <div class="link-rule"></div>
      <p class="preview-note">不会自动访问链接，也不会抓取网页标题。取用时粘贴原始网址。</p>
    {:else if expanded && full && kind !== "link"}
      {#if kind === "code"}<pre class="code-sheet selectable">{full.text}</pre>{:else}<p class="preview-text selectable">{full.text}</p>{/if}
      {#if full.shown < full.total}
        <div class="cut" use:loadRest>已显示前 {count(full.shown)} 字 · 滚到底自动加载其余 {count(full.total - full.shown)} 字</div>
      {/if}
      {#if rich}<p class="preview-note">显示的是保存的纯文本，不还原富文本样式。</p>{/if}
    {:else if kind === "code"}
      <pre class="code-sheet">{item.preview || "（空文本）"}</pre>
    {:else if kind === "file"}
      <h2 class="preview-title">{item.file_names.length === 1 ? item.file_names[0] : `${item.file_names.length} 个文件`}</h2>
      <ul class="file-list">
        {#if paths}
          {#each paths as entry}
            <li class={entry.state}>
              {#if entry.state === "missing"}<b class="file-state" aria-label="不在了">✕</b>{:else if entry.state === "unknown"}<b class="file-state" aria-label="未确认">?</b>{:else}<svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M5 2.5h6l4 4v11H5v-15Z M11 2.5v4h4" stroke="currentColor"/></svg>{/if}
              <span class="file-copy"><span>{fileName(entry.path)}</span><code>{entry.path}</code></span>
              {#if entry.state === "missing"}<small>不在了</small>{:else if entry.state === "unknown"}<small>未确认</small>{/if}
            </li>
          {/each}
        {:else}
          {#each item.file_names as name}
            <li><svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M5 2.5h6l4 4v11H5v-15Z M11 2.5v4h4" stroke="currentColor"/></svg><span>{name}</span></li>
          {:else}
            <li><span>文件名称不可用</span></li>
          {/each}
        {/if}
      </ul>
      {#if mark?.state === "missing"}
        <p class="file-note">原文件已被移走或删除，粘贴不了。</p>
      {:else if mark?.state === "partial"}
        <p class="file-note">有 {mark.missing} 个已不在，粘贴时只粘还在的 {mark.total - mark.missing} 个。</p>
      {:else if mark?.state === "unknown"}
        <p class="file-note neutral">网络位置连不上，暂时无法确认。</p>
      {:else}
        <p class="preview-note">这里记录文件引用，不复制文件内容。</p>
      {/if}
    {:else if kind === "short"}
      <h2 class="preview-title">{item.preview || "（空文本）"}</h2>
    {:else}
      <p class="preview-text" class:muted={kind === "long" || rich}>{item.preview || "（空文本）"}</p>
      {#if rich}<p class="preview-note">当前预览为已保存的纯文本摘要，不还原富文本样式。</p>{/if}
    {/if}
    {#if item.partial}<p class="partial">部分格式未保存；此处只显示可用内容。</p>{/if}
  {/if}
</div>

<style>
  .label-block { display: grid; gap: 4px; margin: 0 0 12px; padding: 0 0 10px 10px; border-left: 2px solid var(--accent); border-bottom: 1px solid var(--line-soft); }
  .label-block strong { color: var(--text); font-size: 13px; font-weight: 650; }
  .label-block p { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.6; white-space: pre-wrap; }
  .preview-swap { min-height: 100%; will-change: transform, opacity; }
  .preview-eyebrow { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-3); margin-bottom: 13px; color: var(--faint); font: 12px/1.5 var(--font-mono); }
  .preview-eyebrow span:last-child { text-align: right; white-space: nowrap; }
  .preview-title { margin: 0 0 15px; color: var(--text); font: 600 20px/1.4 var(--font-ui); overflow-wrap: anywhere; white-space: pre-wrap; }
  .preview-text { margin: 0; color: var(--text); font-size: 13px; line-height: 1.85; overflow-wrap: anywhere; white-space: pre-wrap; }
  .preview-text.muted, .preview-note { color: var(--muted); }
  .preview-note { margin: var(--space-4) 0 0; font-size: 12px; line-height: 1.75; }
  .code-sheet { margin: 0; padding: 17px; overflow: auto; border: 1px solid var(--line); border-left: 3px solid var(--accent); background: var(--panel); color: var(--text); font: 12px/1.75 var(--font-mono); white-space: pre; }
  .link-host { margin: 8px 0 12px; color: var(--accent); font: 600 20px/1.3 var(--font-ui); overflow-wrap: anywhere; }
  .link-url { color: var(--muted); font: 12px/1.7 var(--font-mono); overflow-wrap: anywhere; }
  .link-rule { width: 46px; height: 3px; margin: 23px 0; background: var(--accent); }
  .image-frame { height: 210px; display: grid; place-items: center; overflow: hidden; border: 1px solid var(--line); background: repeating-conic-gradient(color-mix(in srgb, var(--line) 22%, transparent) 0 25%, transparent 0 50%) 0 0 / 14px 14px; }
  .image-frame img { max-width: 100%; max-height: 100%; display: block; object-fit: contain; }
  /* 留出头部、底栏和尺寸那一行，外层不再出现第二条滚动条。 */
  .image-stage { height: max(160px, calc(100vh - 264px)); display: grid; place-items: center; overflow: hidden; border: 1px solid var(--line-soft); background: repeating-conic-gradient(color-mix(in srgb, var(--line) 22%, transparent) 0 25%, transparent 0 50%) 0 0 / 16px 16px; }
  .image-stage img { display: block; max-width: none; user-select: none; }
  /* 原尺寸：可以拖动或用方向键平移，不显示滚动条。 */
  .image-stage.original { display: block; overflow: auto; cursor: grab; scrollbar-width: none; }
  .image-stage.original::-webkit-scrollbar { display: none; }
  .image-stage.original:active { cursor: grabbing; }
  .image-missing { color: var(--faint); font-size: 12px; }
  .selectable { user-select: text; cursor: text; }
  .selectable::selection, .selectable *::selection { background: color-mix(in srgb, var(--accent) 30%, transparent); }
  .cut { display: flex; align-items: center; gap: 10px; margin: 14px 0 0; color: var(--faint); font: 12px var(--font-mono); white-space: nowrap; }
  .cut::before, .cut::after { content: ""; flex: 1; height: 1px; background: repeating-linear-gradient(90deg, var(--line) 0 4px, transparent 4px 8px); }
  .image-meta { display: flex; justify-content: space-between; gap: var(--space-2); margin-top: 10px; color: var(--muted); font: 12px var(--font-mono); }
  .color-block { height: 154px; border: 1px solid var(--line); }
  .color-value { margin-top: 13px; font: 600 20px var(--font-mono); overflow-wrap: anywhere; }
  .color-channels { margin-top: 7px; color: var(--muted); font: 12px/1.8 var(--font-mono); }
  .file-list { margin: 0; padding: 0; list-style: none; border-top: 1px solid var(--line); }
  .file-list li { display: flex; align-items: flex-start; gap: 8px; padding: 12px 0; border-bottom: 1px solid var(--line-soft); font-size: 12px; overflow-wrap: anywhere; }
  .file-list svg { width: 18px; height: 18px; flex: none; }
  .file-state { width: 18px; flex: none; text-align: center; font: 600 12px/18px var(--font-mono); }
  .file-list li.missing .file-state, .file-list li.missing small { color: var(--state-caution); }
  .file-list li.unknown .file-state, .file-list li.unknown small { color: var(--faint); }
  .file-list li.missing .file-copy > span { color: var(--muted); }
  .file-copy { min-width: 0; flex: 1; display: grid; gap: 3px; }
  .file-copy code { color: var(--faint); font: 12px/1.5 var(--font-mono); overflow-wrap: anywhere; }
  .file-list small { flex: none; font-size: 12px; line-height: 18px; }
  .file-note { margin: var(--space-4) 0 0; padding: 8px 10px; border-left: 2px solid var(--state-caution); background: color-mix(in srgb, var(--state-caution) 8%, transparent); color: var(--muted); font-size: 12px; line-height: 1.7; }
  .file-note.neutral { border-left-color: var(--line); background: color-mix(in srgb, var(--line) 12%, transparent); }
  .partial { margin: var(--space-4) 0 0; color: var(--state-caution); font-size: 12px; line-height: 1.6; }
  .empty-preview { padding: var(--space-6) 0; color: var(--muted); line-height: 1.8; }
  .empty-preview strong { color: var(--text); font-size: 15px; }
  .empty-preview p { margin: var(--space-2) 0 0; font-size: 12px; }
  .expanded .preview-title { font-size: 20px; }
  .expanded .preview-text { font-size: 15px; }
  .compact .preview-eyebrow { display: none; }
  .compact .preview-title { margin-bottom: 8px; font-size: 15px; }
  .compact .preview-text { font-size: 13px; line-height: 1.72; }
  .compact .preview-note { margin-top: 8px; font-size: 12px; }
  .compact .code-sheet { padding: 9px 11px; font-size: 12px; line-height: 1.65; }
  .compact .link-host { margin: 0 0 6px; font-size: 15px; }
  .compact .link-url { font-size: 12px; }
  .compact .link-rule { margin: 10px 0; }
  .compact .image-frame { height: 82px; }
  .compact .image-meta { margin-top: 6px; font-size: 12px; }
  .compact .color-block { height: 52px; }
  .compact .color-value { margin-top: 8px; font-size: 15px; }
  .compact .color-channels { margin-top: 3px; font-size: 12px; }
  .compact .file-list li { padding: 6px 0; }
  .compact .file-note { margin-top: 8px; font-size: 12px; }
  .compact .empty-preview { padding: 8px 0; }
  .compact .partial { margin-top: 8px; }
  @media (max-width: 599px) {
    .preview-eyebrow span:last-child { display: none; }
    .preview-title { font-size: 15px; }
    .preview-text { font-size: 13px; }
    .code-sheet { padding: var(--space-3); font-size: 12px; }
    .link-host { font-size: 15px; }
  }
</style>
