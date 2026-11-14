<script module lang="ts">
  export type SavePhase = "idle" | "saving" | "saved" | "failed";
  export type SaveMarkState = { phase: SavePhase; message: string; error: string };
</script>

<script lang="ts">
  let { state = { phase: "idle", message: "", error: "" } }: { state?: SaveMarkState } = $props();
  const failureText = $derived(state.error.startsWith("没保存上") ? state.error : `没保存上：${state.error}`);
</script>

<span
  class="save-mark"
  class:saving={state.phase === "saving"}
  class:saved={state.phase === "saved"}
  class:failed={state.phase === "failed"}
  data-save-mark
  data-phase={state.phase}
  role={state.phase === "failed" ? "alert" : "status"}
  aria-live="polite"
  aria-atomic="true"
>
  {#if state.phase === "saving"}
    <span>正在保存…</span>
  {:else if state.phase === "saved"}
    <span title={state.message}>✓ 已保存</span>
  {:else if state.phase === "failed"}
    <span>{failureText}</span>
  {/if}
</span>

<style>
  .save-mark { min-width: 72px; display: inline-flex; align-items: center; justify-content: flex-end; flex: none; color: var(--hand-ink-2); font-size: 12px; line-height: 1.5; overflow-wrap: anywhere; }
  .saved { color: var(--state-success); }
  .failed { color: var(--state-critical); text-align: left; }
  @media (forced-colors: active) { .save-mark { color: CanvasText; } }
</style>
