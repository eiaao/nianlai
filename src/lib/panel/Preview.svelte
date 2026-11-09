<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import * as ipc from "../ipc";
  import type { HistoryItem, PathCheck } from "../ipc";
  import { formatTime, type PanelState } from "../state.svelte";
  import { colorValue, linkParts, relativeTime, sourceName, visualKind } from "./content";
  import { SpringValue } from "./motion";

  let { panel, item, expanded = false, compact = false }: { panel: PanelState; item: HistoryItem | null; expanded?: boolean; compact?: boolean } = $props();
  let root: HTMLDivElement;
  let previousId: number | null | undefined;
  const kind = $derived(item ? visualKind(item) : "short");
  const link = $derived(item ? linkParts(item) : null);
  const color = $derived(item ? colorValue(item) : null);
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
    const files = kind === "files";
    if (id === pathsFor) return;
    pathsFor = id;
    paths = null;
    if (!files) return;
    void ipc.getFilePaths?.(id)?.then((result) => { if (pathsFor === id) paths = result; }).catch(() => {});
  });
  const mark = $derived(item?.file_state ?? null);
  const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

  onDestroy(() => { y.destroy(); opacity.destroy(); });
</script>

<div bind:this={root} class="preview-swap" class:expanded class:compact>
  {#if !item}
    <div class="empty-preview">
      <strong>{panel.query ? "没有可预览的匹配项" : "还没有记录"}</strong>
      <p>{panel.query ? "试试更短的关键词；Esc 返回完整历史。" : "复制文字、图片或文件，它们会按时间出现在这里。"}</p>
    </div>
  {:else}
    <div class="preview-eyebrow">
      <span>{sourceName(item)} / {relativeTime(item.copied_ms)}</span>
      <span>SNAPSHOT {String(item.id).padStart(3, "0")} / LOCAL</span>
    </div>

    {#if kind === "image"}
      <div class="image-placeholder" role="img" aria-label={item.preview || "图片记录"}>
        <svg viewBox="0 0 48 48" fill="none" aria-hidden="true"><rect x="5" y="7" width="38" height="34" stroke="currentColor"/><circle cx="17" cy="18" r="4" stroke="currentColor"/><path d="m8 36 11-11 8 8 6-6 8 9" stroke="currentColor"/></svg>
        <strong>{item.preview || "图片"}</strong>
        <span>当前数据只提供图片元信息，图像资源预览待后续接入。</span>
      </div>
      <div class="image-meta"><span>{sourceName(item)}</span><span>{formatTime(item.copied_ms)}</span></div>
    {:else if kind === "color" && color}
      <div class="color-block" role="img" style={`background-color: ${color.value}`} aria-label={`颜色 ${color.value}`}></div>
      <div class="color-value">{color.value}</div>
      {#if color.rgb}<div class="color-channels">RGB　{color.rgb.join(" · ")}</div>{/if}
      <p class="preview-note">取用时粘贴原始颜色值，不会自动转换格式。</p>
    {:else if kind === "link" && link}
      <div class="link-host">{link.host}</div>
      <div class="link-url">{link.url.href}</div>
      <div class="link-rule"></div>
      <p class="preview-note">不会自动访问链接，也不会抓取网页标题。取用时粘贴原始网址。</p>
    {:else if kind === "code"}
      <pre class="code-sheet">{item.preview || "（空文本）"}</pre>
    {:else if kind === "files"}
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
      <p class="preview-text" class:muted={kind === "long" || kind === "rich"}>{item.preview || "（空文本）"}</p>
      {#if kind === "rich"}<p class="preview-note">当前预览为已保存的纯文本摘要，不还原富文本样式。</p>{/if}
    {/if}
    {#if item.partial}<p class="partial">部分格式未保存；此处只显示可用内容。</p>{/if}
  {/if}
</div>

<style>
  .preview-swap { min-height: 100%; will-change: transform, opacity; }
  .preview-eyebrow { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-3); margin-bottom: 13px; color: var(--faint); font: 9px/1.5 var(--font-mono); letter-spacing: .05em; }
  .preview-eyebrow span:last-child { text-align: right; white-space: nowrap; }
  .preview-title { margin: 0 0 15px; color: var(--text); font: 600 22px/1.35 var(--font-display); letter-spacing: -.02em; overflow-wrap: anywhere; white-space: pre-wrap; }
  .preview-text { margin: 0; color: var(--text); font-size: 14px; line-height: 1.85; overflow-wrap: anywhere; white-space: pre-wrap; }
  .preview-text.muted, .preview-note { color: var(--muted); }
  .preview-note { margin: var(--space-4) 0 0; font-size: 12px; line-height: 1.75; }
  .code-sheet { margin: 0; padding: 17px; overflow: auto; border: 1px solid var(--line); border-left: 3px solid var(--accent); background: var(--panel); color: var(--text); font: 12px/1.75 var(--font-mono); white-space: pre; }
  .link-host { margin: 8px 0 12px; color: var(--accent); font: 600 27px/1.2 var(--font-display); overflow-wrap: anywhere; }
  .link-url { color: var(--muted); font: 12px/1.7 var(--font-mono); overflow-wrap: anywhere; }
  .link-rule { width: 46px; height: 3px; margin: 23px 0; background: var(--accent); }
  .image-placeholder { min-height: 210px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-3); padding: var(--space-5); border: 1px solid var(--line); background: var(--panel); color: var(--muted); text-align: center; }
  .image-placeholder svg { width: 48px; height: 48px; color: var(--faint); }
  .image-placeholder strong { color: var(--text); font-size: 14px; }
  .image-placeholder span { max-width: 290px; font-size: 11px; line-height: 1.7; }
  .image-meta { display: flex; justify-content: space-between; gap: var(--space-2); margin-top: 10px; color: var(--muted); font: 10px var(--font-mono); }
  .color-block { height: 154px; border: 1px solid var(--line); }
  .color-value { margin-top: 13px; font: 600 26px var(--font-mono); overflow-wrap: anywhere; }
  .color-channels { margin-top: 7px; color: var(--muted); font: 12px/1.8 var(--font-mono); }
  .file-list { margin: 0; padding: 0; list-style: none; border-top: 1px solid var(--line); }
  .file-list li { display: flex; align-items: flex-start; gap: 8px; padding: 12px 0; border-bottom: 1px solid var(--line-soft); font-size: 12px; overflow-wrap: anywhere; }
  .file-list svg { width: 18px; height: 18px; flex: none; }
  .file-state { width: 18px; flex: none; text-align: center; font: 600 13px/18px var(--font-mono); }
  .file-list li.missing .file-state, .file-list li.missing small { color: var(--state-caution); }
  .file-list li.unknown .file-state, .file-list li.unknown small { color: var(--faint); }
  .file-list li.missing .file-copy > span { color: var(--muted); }
  .file-copy { min-width: 0; flex: 1; display: grid; gap: 3px; }
  .file-copy code { color: var(--faint); font: 10px/1.5 var(--font-mono); overflow-wrap: anywhere; }
  .file-list small { flex: none; font-size: 10px; line-height: 18px; }
  .file-note { margin: var(--space-4) 0 0; padding: 8px 10px; border-left: 2px solid var(--state-caution); background: color-mix(in srgb, var(--state-caution) 8%, transparent); color: var(--muted); font-size: 11px; line-height: 1.7; }
  .file-note.neutral { border-left-color: var(--line); background: color-mix(in srgb, var(--line) 12%, transparent); }
  .partial { margin: var(--space-4) 0 0; color: var(--state-caution); font-size: 11px; line-height: 1.6; }
  .empty-preview { padding: var(--space-6) 0; color: var(--muted); line-height: 1.8; }
  .empty-preview strong { color: var(--text); font-size: 14px; }
  .empty-preview p { margin: var(--space-2) 0 0; font-size: 12px; }
  .expanded .preview-title { font-size: 25px; }
  .expanded .preview-text { font-size: 15px; }
  .compact .preview-eyebrow { display: none; }
  .compact .preview-title { margin-bottom: 8px; font-size: 17px; }
  .compact .preview-text { font-size: 13px; line-height: 1.72; }
  .compact .preview-note { margin-top: 8px; font-size: 11px; }
  .compact .code-sheet { padding: 9px 11px; font-size: 11px; line-height: 1.65; }
  .compact .link-host { margin: 0 0 6px; font-size: 20px; }
  .compact .link-url { font-size: 11px; }
  .compact .link-rule { margin: 10px 0; }
  .compact .image-placeholder { min-height: 82px; padding: 10px; gap: 6px; }
  .compact .image-placeholder svg { width: 30px; height: 30px; }
  .compact .image-placeholder span { max-width: none; font-size: 10px; }
  .compact .image-meta { margin-top: 6px; font-size: 9px; }
  .compact .color-block { height: 52px; }
  .compact .color-value { margin-top: 8px; font-size: 19px; }
  .compact .color-channels { margin-top: 3px; font-size: 10px; }
  .compact .file-list li { padding: 6px 0; }
  .compact .file-note { margin-top: 8px; font-size: 10px; }
  .compact .empty-preview { padding: 8px 0; }
  .compact .partial { margin-top: 8px; }
  @media (max-width: 599px) {
    .preview-eyebrow span:last-child { display: none; }
    .preview-title { font-size: 18px; }
    .preview-text { font-size: 13px; }
    .code-sheet { padding: var(--space-3); font-size: 11px; }
    .link-host { font-size: 22px; }
    .image-placeholder { min-height: 180px; padding: var(--space-3); }
  }
</style>
