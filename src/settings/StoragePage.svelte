<script lang="ts">
  // “历史与存储”分区：自动快照（B18）。恢复会登记后让拈来重启，启动时在打开库之前替换。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, SettingsUpdate, SnapshotInfo, SnapshotReason, SnapshotState } from "../lib/ipc";

  let { maxMb, disabled, onsave }: {
    maxMb: number;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  let state_ = $state<SnapshotState | null>(null);
  let loadError = $state("");
  let actionError = $state("");
  let busy = $state<"" | "create" | "restore">("");
  let confirming = $state<string | null>(null);

  const REASONS: Record<SnapshotReason, string> = {
    daily: "每日",
    upgrade: "版本更新",
    manual: "手动",
    before_restore: "恢复前",
  };
  const LIMITS = [256, 512, 1024, 2048, 5120];

  const total = $derived((state_?.items ?? []).reduce((sum, item) => sum + item.bytes, 0));
  const restoredItem = $derived(state_?.restored ? state_.items.find((item) => item.reason === "before_restore") : undefined);

  function size(bytes: number) {
    if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
    if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
    return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  }

  function time(ms: number) {
    const d = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function limitLabel(mb: number) {
    return mb >= 1024 ? `${mb / 1024} GB` : `${mb} MB`;
  }

  function message(error: unknown) {
    return typeof error === "string" ? error : error instanceof Error ? error.message : "操作没有完成，请重试";
  }

  async function load() {
    try {
      state_ = await ipc.getSnapshots();
      loadError = "";
    } catch (error) {
      loadError = message(error);
    }
  }

  onMount(() => { void load(); });

  async function create() {
    busy = "create"; actionError = "";
    try {
      await ipc.createSnapshot();
      await load();
    } catch (error) {
      actionError = message(error);
    } finally {
      busy = "";
    }
  }

  async function restore(item: SnapshotInfo) {
    busy = "restore"; actionError = "";
    try {
      // 成功时拈来会退出并重启，这个窗口随之关闭。
      await ipc.restoreSnapshot(item.name);
    } catch (error) {
      actionError = message(error);
      busy = "";
      confirming = null;
    }
  }
</script>

<section class="page">
  <div><h2>自动快照</h2><p class="page-intro">每天自动给历史库拍一份快照，版本更新后也会额外拍一份，保留最近 {state_?.keep ?? 7} 份。快照包含文字、图片和文件路径，不包含文件本身。出问题时可以把历史恢复到其中任何一份。</p></div>

  {#if state_?.restored}
    {#if state_.restored.error}
      <p class="banner error">上次恢复没有成功，历史保持原样。</p>
    {:else}
      <p class="banner">已恢复到所选快照。恢复前的历史另存成了“恢复前”快照{restoredItem ? `（${time(restoredItem.created_ms)}）` : ""}，需要时可以从下面恢复回去。</p>
    {/if}
  {/if}

  <div class="group"><span class="group-title">快照</span><div class="card">
    {#if loadError}
      <p class="note error">{loadError}</p>
    {:else if !state_}
      <p class="note">正在读取…</p>
    {:else}
      <ul class="list" aria-label="快照列表">
        {#each state_.items as item (item.name)}
          <li>
            <div class="row">
              <span class="when">{time(item.created_ms)}</span>
              <span class="tag">{item.reason ? REASONS[item.reason] : "自动"}</span>
              <span class="bytes">{size(item.bytes)}</span>
              <button type="button" disabled={!!busy || disabled} onclick={() => { confirming = confirming === item.name ? null : item.name; actionError = ""; }}>恢复</button>
            </div>
            {#if confirming === item.name}
              <div class="confirm" role="alert">
                <p>把历史恢复到 {time(item.created_ms)}？之后新增的记录会消失。当前历史会先另存成一份“恢复前”快照，{state_.relaunch ? "然后拈来自动重启。" : "然后拈来退出，需要手动重新打开。"}</p>
                <div>
                  <button type="button" class="danger" disabled={!!busy} onclick={() => restore(item)}>{busy === "restore" ? "正在校验快照…" : "确认恢复"}</button>
                  <button type="button" disabled={!!busy} onclick={() => { confirming = null; }}>取消</button>
                </div>
              </div>
            {/if}
          </li>
        {:else}
          <li class="empty">还没有快照。拈来启动后会自动拍第一份。</li>
        {/each}
      </ul>
      <div class="foot">
        <small>共 {state_.items.length} 份，占用 {size(total)}</small>
        <button type="button" disabled={!!busy || disabled} onclick={create}>{busy === "create" ? "正在拍快照…" : "立即拍一份"}</button>
      </div>
    {/if}
    {#if actionError}<p class="note error">{actionError}</p>{/if}
  </div></div>

  <div class="group"><span class="group-title">空间</span><div class="card">
    <label class="setting">
      <span class="setting-copy"><strong>快照占用上限</strong><small>超过上限时从最旧的删起，至少保留最新的一份。</small></span>
      <select value={String(maxMb)} {disabled} onchange={(event) => onsave({ snapshots: { max_mb: Number(event.currentTarget.value) } }, "快照上限已保存，下次拍快照时生效")}>
        {#each LIMITS as mb}<option value={String(mb)}>{limitLabel(mb)}{mb === 1024 ? "（默认）" : ""}</option>{/each}
        {#if !LIMITS.includes(maxMb)}<option value={String(maxMb)}>{limitLabel(maxMb)}</option>{/if}
      </select>
    </label>
  </div></div>
</section>

<style>
  .banner { margin: 0 0 16px; padding: 10px 14px; border: 1px solid var(--settings-accent); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; line-height: 1.6; }
  .banner.error { border-color: var(--state-critical); color: var(--state-critical); }
  .note { margin: 0; padding: 11px 16px; color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .note.error { color: var(--state-critical); }
  .list { margin: 0; padding: 0; list-style: none; }
  .list li { border-bottom: 1px solid var(--settings-line); }
  .list li.empty { padding: 14px 16px; color: var(--settings-muted); font-size: 11px; }
  .row { display: grid; grid-template-columns: minmax(0, 1fr) auto 72px auto; align-items: center; gap: 12px; min-height: 44px; padding: 6px 16px; }
  .when { font: 12px var(--font-mono); }
  .tag { padding: 1px 7px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); color: var(--settings-muted); font-size: 10px; }
  .bytes { color: var(--settings-muted); font: 11px var(--font-mono); text-align: right; }
  button { min-height: 28px; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; }
  button.danger { border-color: var(--state-critical); color: var(--state-critical); }
  button:disabled { opacity: .6; }
  .confirm { display: grid; gap: 8px; padding: 0 16px 12px; }
  .confirm p { margin: 0; color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .confirm div { display: flex; gap: 8px; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 16px; }
  .foot small { color: var(--settings-muted); font-size: 10px; }
</style>
