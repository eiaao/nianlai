<script lang="ts">
  // 页脚：粘贴结果和错误提示（含 M0-6 的管理员提示）、模式、呼出耗时、退出快捷键、版本号（M0-5）。
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<footer>
  {#if panel.message}<p class="result" role="status">{panel.message}</p>{/if}
  <div class="status">
    <span class="mode"><i class:search={panel.mode === "search"}></i>{panel.interaction === "activation_pending" ? "等待激活" : panel.interaction === "activation_failed" ? "未取得焦点" : panel.interaction === "activated" && !panel.interactive ? "输入准备中" : panel.mode === "search" ? "搜索模式" : panel.interaction === "activated" ? "复制取用" : "快速取用"}</span>
    <span class="timing" title="快捷键到显示 / 显示到前端首帧">{panel.timing ? panel.timing.hotkeyToShowMs.toFixed(1) : "—"} / {panel.timing?.showToPaintMs?.toFixed(1) ?? "—"} ms</span>
    <!-- 面板常压在输入框上方，按钮容易被误点，退出只留快捷键。 -->
    {#if panel.interactive}<span class="quit">退出：Ctrl+Alt+Shift+Q</span>{/if}
  </div>
  {#if panel.version}<p class="version">v{panel.version}</p>{/if}
</footer>

<style>
  footer { border-top: 1px solid var(--color-border-divider); padding: var(--space-2) var(--space-3); }
  .status { display: flex; align-items: center; gap: var(--space-status-gap); min-height: 24px; }
  .mode { display: flex; align-items: center; gap: var(--space-mode-gap); white-space: nowrap; font-size: var(--font-size-caption); }
  .mode i { width: 6px; height: 6px; background: var(--color-neutral); border-radius: 50%; }
  .mode i.search { background: var(--color-accent); }
  .timing { color: var(--color-text-secondary); font-size: var(--font-size-caption); margin-left: auto; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .quit { flex-shrink: 0; color: var(--color-text-secondary); font-size: var(--font-size-caption); white-space: nowrap; }
  .version { margin: var(--space-version-before) 0 0; color: var(--color-text-secondary); font-size: var(--font-size-caption); text-align: right; }
  .result { margin: 0 0 var(--space-result-after); color: var(--color-accent); font-size: var(--font-size-caption); line-height: 1.5; overflow-wrap: anywhere; }
</style>
