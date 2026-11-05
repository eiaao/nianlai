<script lang="ts">
  import { formatTime, type PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
  let remaining = $state(0);

  $effect(() => {
    const feedback = panel.feedback;
    if (!feedback) { remaining = 0; return; }
    const update = () => { remaining = Math.max(0, feedback.duration - (Date.now() - feedback.startedAt)); };
    update();
    const timer = setInterval(update, 250);
    return () => clearInterval(timer);
  });

  const recordText = $derived(panel.history.store_status && panel.history.store_status !== "正在加载历史"
    ? panel.history.recording ? "记录中（仅内存）" : "未在记录 · 仅内存"
    : panel.history.recording
      ? `记录中${panel.history.last_recorded_ms ? ` · 上次 ${formatTime(panel.history.last_recorded_ms)}` : ""}`
      : "未在记录");
  const lastLabel = $derived(panel.lastResult === "confirmed" ? "已粘贴" : panel.lastResult === "sent" ? "已发出粘贴" : "已复制");
</script>

<footer class:has-result={!!panel.feedback} class="statusbar" title={panel.timing ? `呼出 ${panel.timing.hotkeyToShowMs?.toFixed(1) ?? "—"} ms / 首帧 ${panel.timing.showToPaintMs?.toFixed(1) ?? "—"} ms` : undefined}>
  {#if panel.feedback}
    {#key panel.feedback}
      <div class="result-footer" class:copied={panel.feedback.kind === "copied"} class:failed={panel.feedback.kind === "failed"} role="status" aria-live="polite">
        <i class="result-line" class:animated={panel.motion !== "none"} style={`animation-duration: ${panel.feedback.duration}ms`}></i>
        <div class="result-copy"><strong>{panel.feedback.title}</strong><small>{panel.feedback.detail} · {Math.ceil(remaining / 1000)} 秒</small></div>
        {#if panel.interaction === "passive"}<button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>{/if}
      </div>
    {/key}
  {:else}
    <div class="normal-footer">
      <span class="record-status"><span class="record-square" class:inactive={!panel.history.recording}></span><span>{panel.lastResult ? `上次：${lastLabel} · 刚才` : recordText}</span></span>
      <div class="key-help">
        {#if panel.interaction === "passive"}
          <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>
        {:else if !panel.interactive}
          <span>INPUT / WAITING</span>
        {:else if panel.previewOpen}
          <span><kbd>Space</kbd> 返回</span><span><kbd>Esc</kbd> 逐层退出</span>
        {:else if panel.mode === "search"}
          <span><kbd>↵</kbd> 粘贴</span><span><kbd>Ctrl+1–9</kbd> 取用</span><span><kbd>Esc</kbd> 返回</span>
        {:else}
          <span><kbd>↵</kbd> 粘贴</span><span><kbd>1–9</kbd> 取用</span><span><kbd>Space</kbd> 预览</span>
        {/if}
      </div>
    </div>
    {#if panel.message && panel.interaction !== "passive"}<p class="message" role="status">{panel.message}</p>{/if}
  {/if}
</footer>

<style>
  .statusbar { min-height: 36px; flex: none; padding: 0 12px; border-top: 1px solid var(--line); color: var(--muted); font-size: 10px; }
  .statusbar.has-result { min-height: 54px; padding-top: 8px; padding-bottom: 8px; }
  .normal-footer { min-height: 35px; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .record-status { min-width: 0; display: flex; align-items: center; gap: 7px; white-space: nowrap; }
  .record-status > span:last-child { overflow: hidden; text-overflow: ellipsis; }
  .record-square { width: 6px; height: 6px; flex: none; background: var(--accent); }
  .record-square.inactive { background: var(--state-critical); }
  .key-help { display: flex; align-items: center; gap: 12px; white-space: nowrap; }
  .key-help span { display: flex; align-items: center; gap: 4px; }
  kbd { color: var(--text); font: 10px/1.4 var(--font-mono); }
  .result-footer { position: relative; width: 100%; display: flex; align-items: center; gap: 8px; line-height: 1.45; animation: appear var(--fade-duration) linear; }
  .result-footer.copied { color: var(--state-caution); }
  .result-footer.failed { color: var(--state-critical); }
  .result-copy { min-width: 0; display: grid; gap: 3px; }
  .result-footer strong { font-size: 11px; }
  .result-footer small { font-size: 10px; overflow-wrap: anywhere; }
  .result-line { position: absolute; left: 0; top: -8px; width: 100%; height: 2px; background: currentColor; transform-origin: left; }
  .result-line.animated { animation-name: countdown; animation-timing-function: linear; animation-fill-mode: forwards; }
  button { flex: none; margin-left: auto; padding: 3px 0 3px 8px; border: 0; background: transparent; color: inherit; text-decoration: underline; font-size: 10px; }
  .message { max-height: 32px; margin: 0 0 6px; overflow: auto; color: var(--state-critical); font-size: 10px; line-height: 1.5; }
  @keyframes countdown { from { transform: scaleX(1); } to { transform: scaleX(0); } }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  @media (max-width: 599px) {
    .statusbar { padding-left: 8px; padding-right: 8px; }
    .normal-footer { gap: 7px; }
    .record-status { max-width: 45%; font-size: 9px; }
    .key-help { gap: 7px; font-size: 9px; }
    .key-help span:nth-child(2) { display: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    .result-footer { animation-duration: var(--duration-reduce); }
    .result-line.animated { display: none; }
  }
</style>
