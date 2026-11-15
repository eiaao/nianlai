<script lang="ts">
  import type { PanelState } from "../state.svelte";

  let { panel }: { panel: PanelState } = $props();
  const loading = $derived(panel.history.store_status === "正在打开");
  const paused = $derived(!!panel.history.store_status?.includes("暂停"));
</script>

{#if panel.history.store_status || !panel.history.recording}
  <section class:error={!loading && !paused} class:paused={paused || loading} class="state-banner" data-hit role="alert">
    <span aria-hidden="true">{paused ? "Ⅱ" : loading ? "···" : "△"}</span>
    <span class="message">
      {#if paused}
        已暂停，现在复制的不会记下
      {:else if loading}
        正在打开，稍等
      {:else if panel.history.store_status}
        存不进去了：{panel.history.store_status}。现在复制的，关掉拈来就没了
      {:else}
        暂时没在记录，正在自己恢复
      {/if}
    </span>
    <button type="button" tabindex="-1" onpointerdown={(event) => event.preventDefault()} onclick={() => { void panel.refreshRecordingStatus(); }}>再看看</button>
  </section>
{/if}

<style>
  .state-banner { min-height: 34px; display: flex; align-items: center; gap: 8px; padding: 7px 12px; border-bottom: 1px solid currentColor; font-size: 12px; line-height: 1.5; animation: appear var(--fade-duration) linear; }
  .state-banner.paused { background: var(--state-caution-bg); color: var(--state-caution); }
  .state-banner.error { background: var(--state-critical-bg); color: var(--state-critical); }
  .message { min-width: 0; flex: 1; }
  button { flex: none; margin-left: auto; padding: 2px 0 2px 8px; border: 0; background: transparent; color: inherit; text-decoration: underline; font-size: 12px; }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  /* 只在设置里选了“跟随系统”且系统开了“减少动画效果”时生效（data-reduce-motion）。 */
  :global(html[data-reduce-motion="true"]) .state-banner { animation-duration: var(--duration-reduce); }
</style>
