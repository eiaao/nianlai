<script lang="ts">
  // 全部存档：列表、立即存一份、回到这天、占用上限。恢复 IPC 和重启行为不变。
  import * as ipc from "../lib/ipc";
  import type { AppSettings, SettingsUpdate, SnapshotInfo, SnapshotReason, SnapshotState } from "../lib/ipc";
  import { limitLabel, message, size, time } from "./format";

  let { snapshots, maxMb, disabled, onsave, onchange }: {
    snapshots: SnapshotState | null;
    maxMb: number;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
    onchange: () => Promise<void>;
  } = $props();

  let actionError = $state("");
  let busy = $state<"" | "create" | "restore">("");
  let confirming = $state<string | null>(null);
  const REASONS: Record<SnapshotReason, string> = {
    daily: "每天",
    upgrade: "更新时",
    manual: "手动",
    before_restore: "回去之前",
  };
  const LIMITS = [256, 512, 1024, 2048, 5120];
  const total = $derived((snapshots?.items ?? []).reduce((sum, item) => sum + item.bytes, 0));

  async function create() {
    busy = "create"; actionError = "";
    try {
      await ipc.createSnapshot();
      await onchange();
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

<div><p class="page-intro">每天自动存一份，版本更新后也会额外存一份，留最近 {snapshots?.keep ?? 7} 份。存档包含文字、图片和文件在哪，不包含文件本身。出了问题，可以回到其中任何一天。</p></div>

<div class="group"><span class="group-title">存档</span><div class="card">
  {#if !snapshots}
    <p class="note">正在读取…</p>
  {:else}
    <ul class="list" aria-label="存档列表">
      {#each snapshots.items as item (item.name)}
        <li>
          <div class="row">
            <span class="when">{time(item.created_ms)}</span>
            <span class="tags"><span class="tag">{item.reason ? REASONS[item.reason] : "自动"}</span>
            {#if item.version !== null && item.version < snapshots.current_version}<span class="tag" title="这份存档是旧版结构，当前是 v{snapshots.current_version}">旧版本，回去后自动升级</span>{/if}</span>
            <span class="bytes">{size(item.bytes)}</span>
            <button type="button" disabled={!!busy || disabled} onclick={() => { confirming = confirming === item.name ? null : item.name; actionError = ""; }}>回到这天</button>
          </div>
          {#if confirming === item.name}
            <div class="confirm" role="alert">
              <p>回到 {time(item.created_ms)} 的样子？之后复制的会不见。现在的会先另存一份，{snapshots.relaunch ? "拈来会重新打开。" : "然后拈来退出，需要手动重新打开。"}</p>
              <div>
                <button type="button" class="danger" disabled={!!busy} onclick={() => restore(item)}>{busy === "restore" ? "正在检查这份存档…" : "再点一次，回到这天"}</button>
                <button type="button" disabled={!!busy} onclick={() => { confirming = null; }}>取消</button>
              </div>
            </div>
          {/if}
        </li>
      {:else}
        <li class="empty">还没有存档。拈来启动后会自动存第一份。</li>
      {/each}
    </ul>
    <div class="foot">
      <small>共 {snapshots.items.length} 份，占用 {size(total)}</small>
      <button type="button" disabled={!!busy || disabled} onclick={create}>{busy === "create" ? "正在存档…" : "立即存一份"}</button>
    </div>
  {/if}
  {#if actionError}<p class="note error" role="alert">{actionError}</p>{/if}
</div></div>

<div class="group"><span class="group-title">空间</span><div class="card">
  <label class="setting">
    <span class="setting-copy"><strong>存档最多占多大</strong><small>超了从最早的删，最新一份总会留着。</small></span>
    <select value={String(maxMb)} {disabled} onchange={(event) => onsave({ snapshots: { max_mb: Number(event.currentTarget.value) } }, "存档上限已保存，下次存档时生效")}>
      {#each LIMITS as mb}<option value={String(mb)}>{limitLabel(mb)}{mb === 1024 ? "（默认）" : ""}</option>{/each}
      {#if !LIMITS.includes(maxMb)}<option value={String(maxMb)}>{limitLabel(maxMb)}</option>{/if}
    </select>
  </label>
</div></div>

<style>
  .note { margin: 0; padding: 12px 16px; color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .note.error { color: var(--state-critical); }
  .list { margin: 0; padding: 0; list-style: none; }
  .list li { border-bottom: 1px solid var(--hand-line); }
  .list li.empty { padding: 16px; color: var(--hand-ink-2); font-size: 12px; }
  .row { display: grid; grid-template-columns: minmax(0, 1fr) auto 72px auto; align-items: center; gap: 12px; min-height: 48px; padding: 8px 16px; }
  .when { font: 12px var(--font-mono); }
  .tags { display: flex; align-items: center; gap: 8px; }
  .tag { padding: 4px 8px; border: 1px solid var(--hand-line-strong); border-radius: 4px; color: var(--hand-ink-2); font-size: 12px; }
  .bytes { color: var(--hand-ink-2); font: 12px var(--font-mono); text-align: right; }
  button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); font-size: 12px; }
  button.danger { border-color: var(--state-critical); color: var(--state-critical); }
  button:disabled { opacity: .6; }
  .confirm { display: grid; gap: 8px; padding: 0 16px 12px; }
  .confirm p { margin: 0; color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .confirm div { display: flex; gap: 8px; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px; }
  .foot small { color: var(--hand-ink-2); font-size: 12px; }
</style>
