<script lang="ts">
  // 轻提示条：粘贴结果、撤销删除、轻提示（notice）、错误说明。只在有话要说时出现；按键提示不在这里（见 HandPanel 的 .hint）。
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<div class="statusbar" class:has-result={!!panel.feedback} data-hit title={panel.timing ? `呼出 ${panel.timing.hotkeyToShowMs?.toFixed(1) ?? "—"} ms / 首帧 ${panel.timing.showToPaintMs?.toFixed(1) ?? "—"} ms` : undefined}>
  {#if panel.feedback}
    {#key panel.feedback}
      <div class="result" class:copied={panel.feedback.kind === "copied"} class:failed={panel.feedback.kind === "failed"} role="status" aria-live="polite">
        <i class="result-line" class:animated={panel.motion !== "none"} style={`animation-duration: ${panel.feedback.duration}ms`}></i>
        <div class="result-copy"><strong>{panel.feedback.title}</strong><small>{panel.feedback.detail}</small></div>
        {#if panel.interaction === "passive"}<button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>{/if}
      </div>
    {/key}
  {:else if panel.interaction === "passive"}
    <span class="line">{panel.message}</span>
    <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>
  {:else if panel.message}
    <p class="line message" role="status">{panel.message}</p>
  {:else if panel.holdDelete?.phase === "hint"}
    <span class="line" role="status">常用的要按住 {panel.holdDelete.keys} 才会删除</span>
  {:else if panel.notice}
    <span class="line" role="status" aria-live="polite">{panel.notice}</span>
  {:else if panel.undo}
    {#key panel.undo.startedAt}
      <span class="undo" role="status" aria-live="polite">
        <span class="undo-text">已放进回收站<span class="undo-label">「{panel.undo.label}」</span></span>
        <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={() => { void panel.undoDelete(); }}>撤销</button><kbd>Ctrl+Z</kbd>
        <i class="undo-progress" style={`animation-duration: ${panel.undo.duration}ms`}></i>
      </span>
    {/key}
  {/if}
</div>

<style>
  .statusbar { position: relative; min-height: 32px; display: flex; align-items: center; gap: 8px; padding: 0 12px; border-radius: 16px; background: var(--hand-paper); box-shadow: var(--hand-pop); color: var(--hand-ink-2); font-size: 12px; white-space: nowrap; animation: appear var(--fade-duration) linear; }
  .statusbar.has-result { min-height: 48px; padding-top: 6px; padding-bottom: 6px; border-radius: 10px; white-space: normal; }
  .line { min-width: 0; margin: 0; overflow: hidden; text-overflow: ellipsis; }
  .message { color: var(--state-critical); white-space: normal; line-height: 18px; padding: 7px 0; }
  .result { position: relative; width: 100%; display: flex; align-items: center; gap: 8px; line-height: 1.45; }
  .result.copied { color: var(--state-caution); }
  .result.failed { color: var(--state-critical); }
  .result-copy { min-width: 0; display: grid; gap: 2px; }
  .result strong, .result small { font-size: 12px; overflow-wrap: anywhere; }
  .result-line { position: absolute; left: 0; top: -6px; width: 100%; height: 2px; background: currentColor; transform-origin: left; }
  .result-line.animated { animation-name: countdown; animation-timing-function: linear; animation-fill-mode: forwards; }
  button { flex: none; margin-left: auto; padding: 2px 0 2px 8px; border: 0; background: transparent; color: inherit; text-decoration: underline; font-size: 12px; }
  .undo { position: relative; min-width: 0; display: inline-flex; align-items: center; gap: 6px; padding: 4px 0; overflow: hidden; }
  .undo-text { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .undo-label { color: var(--hand-ink-3); }
  .undo button { margin-left: 2px; padding: 0; color: var(--hand-ink); font-weight: 600; }
  kbd { color: var(--hand-ink-3); font: 12px/1 var(--font-ui); }
  .undo-progress { position: absolute; left: 0; bottom: 0; width: 100%; height: 2px; background: currentColor; opacity: .5; transform-origin: left; animation-name: countdown; animation-timing-function: linear; animation-fill-mode: forwards; }
  @keyframes countdown { from { transform: scaleX(1); } to { transform: scaleX(0); } }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  :global(html[data-reduce-motion="true"]) .statusbar { animation-duration: var(--duration-reduce); }
  :global(html[data-reduce-motion="true"]) .result-line.animated { display: none; }
</style>
