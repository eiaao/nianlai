<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import type { PanelState } from "../state.svelte";
  import { SpringValue } from "./motion";

  let { panel, layout = "a" }: { panel: PanelState; layout?: "a" | "b" } = $props();
  let rail: HTMLLIElement;
  const spring = new SpringValue(-50, (value) => {
    if (rail) rail.style.transform = `translate3d(0, ${value}px, 0)`;
  }, () => panel.motion);

  $effect(() => {
    const selectedId = panel.visibleItems[panel.selected]?.id;
    panel.motion;
    void tick().then(() => {
      if (!panel.list || !rail) return;
      const row = selectedId === undefined ? null : panel.list.querySelector<HTMLElement>(`[data-history-index="${panel.selected}"]`);
      spring.to(row ? row.offsetTop + (layout === "a" ? 13 : 0) : -60);
    });
  });

  onDestroy(() => spring.destroy());
</script>

<li bind:this={rail} class="selection-rail" class:command={layout === "b"} aria-hidden="true"></li>

<style>
  .selection-rail { position: absolute; z-index: 3; left: 0; top: 0; width: 3px; height: 38px; margin: 0; padding: 0; background: var(--accent); pointer-events: none; will-change: transform; }
  .selection-rail.command { width: 4px; height: var(--size-row-b); }
</style>
