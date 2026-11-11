<script lang="ts">
  // 导入备份（M2-4 / B07）：选文件 → 三步校验逐行打勾（转圈 → ✓）→ 选合并或覆盖（默认合并）→ 确认。
  // 任一步失败就停在那一步、写明原因，现有数据不动。合并完成后用滚动数字显示新增了多少条。
  import { onDestroy } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { BackupImported, BackupInspection, BackupStep } from "../lib/ipc";
  import { reducedMotion } from "../lib/preferences";
  import RollingNumber from "./RollingNumber.svelte";
  import { count, message, time } from "./format";

  let { disabled, onchange }: { disabled: boolean; onchange: () => void } = $props();

  const STEPS: { id: BackupStep["id"]; label: string }[] = [
    { id: "manifest", label: "读取清单" },
    { id: "hashes", label: "核对文件" },
    { id: "database", label: "检查历史库" },
  ];

  let phase = $state<"pick" | "checking" | "ready" | "failed" | "working" | "merged" | "restarting">("pick");
  let inspection = $state<BackupInspection | null>(null);
  // 校验一次返回全部结果；界面逐行揭晓，每行先转一会儿圈再打勾，让人看清做了哪几件事。
  let revealed = $state(0);
  let mode = $state<"merge" | "overwrite">("merge");
  let armed = $state(false);
  let result = $state<BackupImported | null>(null);
  let shown = $state(0);
  let error = $state("");
  let armTimer: ReturnType<typeof setTimeout> | null = null;

  const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, reducedMotion() ? 0 : ms));

  async function pick() {
    error = "";
    let path: string | null;
    try {
      path = await ipc.chooseBackupOpen();
    } catch (value) {
      error = message(value);
      return;
    }
    if (!path) return;
    await discard();
    phase = "checking"; revealed = 0; mode = "merge"; armed = false; result = null;
    const started = performance.now();
    try {
      const found = await ipc.inspectBackup(path);
      inspection = found;
      // 第一行至少转 400 毫秒；之后每行间隔 320 毫秒揭晓。
      await wait(Math.max(0, 400 - (performance.now() - started)));
      for (let index = 0; index < found.steps.length; index++) {
        revealed = index + 1;
        if (!found.steps[index].ok) break;
        if (index < found.steps.length - 1) await wait(320);
      }
      phase = found.ok ? "ready" : "failed";
    } catch (value) {
      error = message(value);
      phase = "pick";
    }
  }

  async function discard() {
    const token = inspection?.token;
    inspection = null;
    if (token && phase !== "merged" && phase !== "restarting") await ipc.discardImport(token).catch(() => {});
  }
  onDestroy(() => { void discard(); if (armTimer) clearTimeout(armTimer); });

  function stepState(index: number) {
    if (!inspection || index >= revealed) return phase === "checking" && index === revealed ? "running" : "waiting";
    return inspection.steps[index]?.ok ? "ok" : "failed";
  }

  async function confirm() {
    if (!inspection || phase !== "ready") return;
    if (mode === "overwrite" && !armed) {
      // 覆盖要按两次：第一次按钮变成“确认覆盖并重启”，4 秒内再按才执行。
      armed = true;
      armTimer = setTimeout(() => { armed = false; }, 4000);
      return;
    }
    if (armTimer) clearTimeout(armTimer);
    phase = "working"; error = "";
    try {
      result = await ipc.importBackup(inspection.token, mode);
      if (result.mode === "overwrite" || result.relaunch) {
        phase = "restarting";
        return;
      }
      phase = "merged";
      shown = 0;
      await wait(120);
      shown = result.added_items;
      onchange();
    } catch (value) {
      error = message(value);
      phase = "ready";
      armed = false;
    }
  }

  // 备份里的内容本地都有：合并什么也不会加，直接说清楚，并让“合并”按不下去。
  const nothingNew = $derived(!!inspection?.merge && inspection.merge.new_items === 0 && inspection.merge.new_favorites === 0);

  async function reset() {
    await discard();
    phase = "pick"; revealed = 0; result = null; error = ""; armed = false;
  }
</script>

<p class="page-intro">{#if inspection}{inspection.file_name} · 导出于 {time(inspection.exported_ms)} · 来自 {inspection.app_version}{:else}选一个 .nianlai-backup 文件。导入前会先校验，校验没通过不会改动现在的任何数据。{/if}</p>

{#if phase === "pick"}
  <div class="card">
    <div class="setting">
      <span class="setting-copy"><strong>选择备份文件</strong><small>可以是这台电脑或别的电脑上导出的；旧版本的备份会先在临时区升级</small></span>
      <button type="button" class="primary" {disabled} onclick={pick}>选择…</button>
    </div>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
{:else}
  <div class="card steps" aria-live="polite">
    {#each STEPS as step, index (step.id)}
      {@const state = stepState(index)}
      <div class="step {state}">
        <span class="mark" aria-hidden="true">
          {#if state === "running"}<span class="spinner"></span>{:else if state === "ok"}<svg viewBox="0 0 16 16"><path class="tick" d="M3.5 8.5l3 3 6-7" /></svg>{:else if state === "failed"}<svg viewBox="0 0 16 16"><path class="cross" d="M4.5 4.5l7 7M11.5 4.5l-7 7" /></svg>{/if}
        </span>
        <strong>{step.label}</strong>
        <small>{state === "ok" || state === "failed" ? inspection?.steps[index]?.detail : state === "running" ? "正在检查…" : ""}</small>
      </div>
    {/each}
  </div>

  {#if phase === "failed"}
    <div class="card">
      <div class="setting">
        <span class="setting-copy"><strong class="bad">校验没通过，没有改动任何数据</strong><small>换一个备份文件再试；如果文件是从别处拷来的，确认拷贝完整</small></span>
        <button type="button" onclick={reset}>换一个文件</button>
      </div>
    </div>
  {:else if inspection?.ok && (phase === "ready" || phase === "working")}
    <div class="choices" role="radiogroup" aria-label="导入方式">
      <button type="button" class="choice" role="radio" aria-checked={mode === "merge"} disabled={phase === "working"} onclick={() => { mode = "merge"; armed = false; }}>
        <span class="radio" aria-hidden="true"></span>
        <span class="copy"><strong>合并</strong><small>把备份里有、现在没有的内容加进来；同样的内容只留一份。收藏追加在现有收藏后面。</small></span>
        <span class="figure">{#if nothingNew}没有新内容{:else}新增 {count(inspection.merge?.new_items ?? 0)} 条{#if inspection.merge?.new_favorites} · 收藏 +{count(inspection.merge.new_favorites)}{/if}{/if}</span>
      </button>
      <button type="button" class="choice" role="radio" aria-checked={mode === "overwrite"} disabled={phase === "working"} onclick={() => { mode = "overwrite"; }}>
        <span class="radio" aria-hidden="true"></span>
        <span class="copy"><strong>覆盖</strong><small>用备份替换现在的全部历史。替换前先把当前历史另存成一份“恢复前”快照，然后拈来重启。</small></span>
        <span class="figure">替换为 {count(inspection.overwrite?.items ?? 0)} 条</span>
      </button>
    </div>
    <div class="actions">
      {#if error}<span class="error inline" role="alert">{error}</span>{/if}
      <button type="button" disabled={phase === "working"} onclick={reset}>取消</button>
      <button type="button" class="primary" class:armed disabled={disabled || phase === "working" || (mode === "merge" && nothingNew)} title={mode === "merge" && nothingNew ? "备份里的内容这里都有了" : undefined} onclick={confirm}>
        {#if phase === "working"}<span class="spinner" aria-hidden="true"></span>{mode === "merge" ? "正在合并…" : "正在准备…"}{:else if mode === "merge"}合并{:else if armed}确认覆盖并重启{:else}覆盖并重启{/if}
        {#if armed}<i class="countdown" aria-hidden="true"></i>{/if}
      </button>
    </div>
  {:else if phase === "merged" && result}
    <div class="card done" role="status">
      <div class="setting">
        <span class="setting-copy"><strong>合并完成：新增 <span class="big"><RollingNumber value={shown} /></span> 条历史{result.added_favorites ? ` · 收藏 +${result.added_favorites}` : ""}</strong><small>{result.added_items ? "新内容按原来的复制时间排进历史里" : "备份里的内容这里都有了，没有新增"}</small></span>
        <button type="button" onclick={reset}>再导入一个</button>
      </div>
    </div>
  {:else if phase === "restarting"}
    <div class="card"><p class="note">已登记覆盖。{result?.auto_restart ? "拈来正在重启" : "拈来即将退出，请手动重新打开"}，之后历史就是备份里的内容；原来的历史另存成了“恢复前”快照，在“全部快照”里。</p></div>
  {/if}
{/if}

<style>
  .card.steps { display: grid; padding: 4px 0; }
  .step { display: grid; grid-template-columns: 18px 88px minmax(0, 1fr); align-items: center; gap: 10px; min-height: 36px; padding: 0 16px; font-size: 12px; }
  .step + .step { border-top: 1px solid var(--settings-line); }
  .step.waiting { opacity: .45; }
  .step small { color: var(--settings-muted); font-size: 11px; }
  .step.failed small, .step.failed strong { color: var(--state-critical); }
  .mark { width: 16px; height: 16px; display: grid; place-items: center; color: var(--settings-accent); }
  .step.failed .mark { color: var(--state-critical); }
  svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
  .cross { stroke-dasharray: 10; stroke-dashoffset: 10; animation: draw 260ms ease-out forwards; }
  /* 对勾、转圈和 rise/draw/spin 关键帧在 settings.css；这里只改尺寸。 */
  .spinner { --spinner-size: 11px; display: inline-block; }
  .choices { display: grid; gap: 8px; animation: rise 240ms ease-out; }
  .choice { display: grid; grid-template-columns: 16px minmax(0, 1fr) auto; align-items: start; gap: 12px; padding: 13px 16px; border: 1px solid var(--settings-line); border-radius: var(--radius-md); background: var(--settings-card); text-align: left; }
  .choice[aria-checked="true"] { border-color: var(--settings-accent); background: var(--settings-selected); }
  .radio { width: 12px; height: 12px; margin-top: 2px; border: 1.5px solid var(--settings-muted); border-radius: 50%; }
  .choice[aria-checked="true"] .radio { border-color: var(--settings-accent); box-shadow: inset 0 0 0 2.5px var(--settings-card), inset 0 0 0 6px var(--settings-accent); }
  .copy { display: grid; gap: 4px; }
  .copy strong { font-size: 13px; }
  .copy small { color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .figure { color: var(--settings-text); font: 600 12px var(--font-mono); white-space: nowrap; }
  .actions { display: flex; align-items: center; justify-content: flex-end; gap: 8px; }
  button { position: relative; min-height: 28px; flex: none; display: inline-flex; align-items: center; gap: 6px; overflow: hidden; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; white-space: nowrap; }
  button.primary { border-color: var(--settings-accent); color: var(--settings-accent); font-weight: 600; }
  button.primary.armed { border-color: var(--state-caution); color: var(--state-caution); }
  button:disabled { opacity: .6; }
  .countdown { position: absolute; left: 0; bottom: 0; height: 2px; width: 100%; background: currentColor; transform-origin: left; animation: shrink 4s linear forwards; }
  @keyframes shrink { to { transform: scaleX(0); } }
  .bad { color: var(--state-critical); }
  .done { animation: rise 260ms ease-out; }
  .big { display: inline-block; color: var(--settings-accent); font: 700 16px var(--font-mono); }
  .note { margin: 0; padding: 14px 16px; color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .error { margin: 0; padding: 10px 16px; color: var(--state-critical); font-size: 11px; }
  .error.inline { padding: 0; margin-right: auto; }
  :global(html[data-reduce-motion="true"]) .cross { animation: none; stroke-dashoffset: 0; }
  :global(html[data-reduce-motion="true"]) .choices, :global(html[data-reduce-motion="true"]) .done, :global(html[data-reduce-motion="true"]) .countdown { animation: none; }
</style>
