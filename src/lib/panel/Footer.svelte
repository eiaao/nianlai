<script lang="ts">
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
</script>

<footer class:has-result={!!panel.feedback} class="statusbar" title={panel.timing ? `呼出 ${panel.timing.hotkeyToShowMs?.toFixed(1) ?? "—"} ms / 首帧 ${panel.timing.showToPaintMs?.toFixed(1) ?? "—"} ms` : undefined}>

  {#if panel.feedback}
    {#key panel.feedback}
      <div class="result-footer" class:copied={panel.feedback.kind === "copied"} class:failed={panel.feedback.kind === "failed"} role="status" aria-live="polite">
        <i class="result-line" class:animated={panel.motion !== "none"} style={`animation-duration: ${panel.feedback.duration}ms`}></i>
        <div class="result-copy"><strong>{panel.feedback.title}</strong><small>{panel.feedback.detail}</small></div>
        {#if panel.interaction === "passive"}<button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>{/if}
      </div>
    {/key}
  {:else}
    <div class="normal-footer">
      <!-- 平时左侧留空：记录正常、粘贴成功都不需要说；记录出问题时顶部横幅会出现。 -->
      <span class="record-status">
        {#if panel.undo}
          {#key panel.undo.startedAt}
            <!-- 删除后的撤销入口：页脚左侧的浅色小标签，倒计时条走完自动消失（与后端撤销窗口对齐）。 -->
            <span class="undo-chip" role="status" aria-live="polite">
              <span class="undo-text">已移到回收站<span class="undo-label">「{panel.undo.label}」</span></span>
              <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={() => { void panel.undoDelete(); }}>撤销</button><kbd>Ctrl+Z</kbd>
              <i class="undo-progress" style={`animation-duration: ${panel.undo.duration}ms`}></i>
            </span>
          {/key}
        {:else if panel.interactive && panel.interaction !== "passive"}
          <!-- 回收站入口：列表和管理都在设置里，面板只给一个入口（M2-D）。 -->
          <button class="trash-link" type="button" tabindex="-1" title="在设置里查看回收站" onpointerdown={(event) => event.preventDefault()} onclick={panel.openTrash}>
            <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M4 5.5h12M8 5.5V3.8h4v1.7M5.6 5.5l.8 10.7h7.2l.8-10.7M8.5 8.5v5M11.5 8.5v5" stroke="currentColor" stroke-width="1.2"/></svg>回收站
          </button>
        {/if}
      </span>
      <div class="key-help">
        {#if panel.interaction === "passive"}
          <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={panel.close}>关闭提示</button>
        {:else if !panel.interactive}
          <span>INPUT / WAITING</span>
        {:else if panel.previewOpen}
          <span><kbd>Space</kbd> 返回</span><span><kbd>Esc</kbd> 逐层退出</span>
        {:else if panel.heldModifier === "ctrl"}
          <span><kbd>↵</kbd> 纯文本粘贴</span>{#if panel.mode === "search"}<span><kbd>1–9</kbd> 取用</span>{/if}<span><kbd>D</kbd> 收藏</span>{#if panel.undo}<span><kbd>Z</kbd> 撤销删除</span>{/if}
        {:else if panel.heldModifier === "shift"}
          <span><kbd>↵</kbd> 只复制，不粘贴</span>
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
  .statusbar { position: relative; min-height: 36px; flex: none; padding: 0 12px; border-top: 1px solid var(--line); color: var(--muted); font-size: 10px; }
  .statusbar.has-result { min-height: 54px; padding-top: 8px; padding-bottom: 8px; }
  .normal-footer { min-height: 35px; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .record-status { min-width: 0; display: flex; align-items: center; gap: 6px; white-space: nowrap; }
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
  .trash-link { display: inline-flex; align-items: center; gap: 4px; margin: 0; padding: 2px 4px; color: var(--faint); text-decoration: none; }
  .trash-link:hover { color: var(--text); }
  .trash-link svg { width: 13px; height: 13px; }
  .undo-chip { position: relative; min-width: 0; display: inline-flex; align-items: center; gap: 6px; padding: 3px 8px 4px; overflow: hidden; border-radius: var(--radius-sm); background: color-mix(in srgb, var(--accent) 13%, transparent); color: var(--accent); animation: appear var(--fade-duration) linear; }
  .undo-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .undo-label { color: var(--muted); }
  .undo-chip button { margin-left: 2px; padding: 0; color: inherit; font-weight: 600; }
  .undo-chip kbd { color: inherit; opacity: .75; }
  .undo-progress { position: absolute; left: 0; bottom: 0; width: 100%; height: 2px; background: currentColor; opacity: .6; transform-origin: left; animation-name: countdown; animation-timing-function: linear; animation-fill-mode: forwards; }
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
    /* 倒计时条是信息（还剩多久能撤销），减少动画时保留；只缩短出现动画。 */
    .undo-chip { animation-duration: var(--duration-reduce); }
  }
</style>
