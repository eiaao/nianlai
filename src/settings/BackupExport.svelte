<script lang="ts">
  // 导出备份（M2-4 / B07）：先说清楚包含什么、多大、不加密；保存位置用系统的“另存为”，默认文件名带日期。
  // 导出完成后按钮变 ✓，并给“在资源管理器里显示”。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { BackupExported, BackupSummary } from "../lib/ipc";
  import { count, message, size } from "./format";

  let { disabled }: { disabled: boolean } = $props();

  let summary = $state<BackupSummary | null>(null);
  let path = $state<string | null>(null);
  let phase = $state<"idle" | "choosing" | "working" | "done">("idle");
  let result = $state<BackupExported | null>(null);
  let error = $state("");

  onMount(async () => {
    try {
      summary = await ipc.backupSummary();
    } catch (value) {
      error = message(value);
    }
  });

  async function choose() {
    if (!summary) return null;
    phase = "choosing"; error = "";
    try {
      const chosen = await ipc.chooseBackupSave(summary.default_name);
      if (chosen) { path = chosen; result = null; }
      return chosen;
    } catch (value) {
      error = message(value);
      return null;
    } finally {
      if (phase === "choosing") phase = "idle";
    }
  }

  async function run() {
    const target = path ?? await choose();
    if (!target) return;
    phase = "working"; error = "";
    try {
      result = await ipc.exportBackup(target);
      phase = "done";
    } catch (value) {
      error = message(value);
      phase = "idle";
    }
  }
</script>

<p class="page-intro">把历史库、来源图标和一份清单打成一个文件（.nianlai-backup）。<strong>不加密</strong>：文件里有你复制过的文字、图片和文件路径，请放在自己信任的地方。</p>

<div class="card">
  <div class="setting">
    <span class="setting-copy">
      {#if summary}
        <strong>包含 {count(summary.items)} 条历史 · {count(summary.favorites)} 条收藏 · {count(summary.icons)} 个来源图标</strong>
        <small>回收站里的不导出。文件类条目只有路径，不包含文件本身</small>
      {:else if !error}
        <strong>正在统计…</strong>
      {/if}
    </span>
    {#if summary}<span class="num" title="导出时会压缩，实际文件通常更小">压缩前 {size(summary.bytes)}</span>{/if}
  </div>
  <div class="setting">
    <span class="setting-copy"><strong>保存到</strong><small class="path" title={path ?? ""}>{path ?? "导出时选择位置，默认放在“文档”里"}</small></span>
    <button type="button" class="ghost" disabled={disabled || !summary || phase === "working" || phase === "choosing"} onclick={choose}>{path ? "更改…" : "选择…"}</button>
    <button type="button" class="primary" class:done={phase === "done"} disabled={disabled || !summary || phase === "working" || phase === "choosing"} onclick={run}>
      {#if phase === "working"}<span class="spinner" aria-hidden="true"></span>正在导出…{:else if phase === "done"}<svg viewBox="0 0 16 16" aria-hidden="true"><path class="tick" d="M3.5 8.5l3 3 6-7" /></svg>已导出{:else}导出{/if}
    </button>
  </div>
  {#if result}
    <div class="setting result" role="status">
      <span class="setting-copy"><strong>已导出 {size(result.bytes)}</strong><small>{count(result.items)} 条历史 · {count(result.favorites)} 条收藏 · {count(result.icons)} 个图标</small></span>
      <button type="button" class="link" onclick={() => result && ipc.revealInExplorer(result.path).catch((value) => { error = message(value); })}>在资源管理器里显示</button>
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</div>

<style>
  .page-intro strong { color: var(--settings-text); font-weight: 600; }
  .num { color: var(--settings-text); font: 600 13px var(--font-mono); white-space: nowrap; }
  .path { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-family: var(--font-mono); }
  button { min-height: 28px; flex: none; display: inline-flex; align-items: center; gap: 6px; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; white-space: nowrap; }
  button.ghost { background: transparent; }
  button.primary { border-color: var(--settings-accent); color: var(--settings-accent); font-weight: 600; }
  button.primary.done { background: var(--settings-accent); color: var(--settings-card); }
  button.link { border: 0; background: transparent; color: var(--settings-accent); text-decoration: underline; text-underline-offset: 2px; }
  button:disabled { opacity: .6; }
  button.primary.done:disabled { opacity: 1; }
  svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
  .tick { stroke-dasharray: 14; stroke-dashoffset: 14; animation: draw 360ms 60ms ease-out forwards; }
  @keyframes draw { to { stroke-dashoffset: 0; } }
  .spinner { width: 10px; height: 10px; border: 1.5px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: spin .7s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .result { animation: rise 260ms ease-out; }
  @keyframes rise { from { opacity: 0; transform: translateY(-4px); } }
  .error { margin: 0; padding: 10px 16px; color: var(--state-critical); font-size: 11px; }
  :global(html[data-reduce-motion="true"]) .tick { animation: none; stroke-dashoffset: 0; }
  :global(html[data-reduce-motion="true"]) .result { animation: none; }
</style>
