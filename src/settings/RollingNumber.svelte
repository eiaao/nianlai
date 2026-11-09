<script lang="ts">
  // 数字变化时滚动过去（清理后“4 条 → 0 条”、回收站“4 → 8”），每一位像里程表一样上下滑动。
  // 减少动画时直接换成新值。bump() 让数字弹一下，用在“飞过来的标签落下”的那一刻。
  import { onDestroy } from "svelte";

  let { value }: { value: number } = $props();

  let shown = $state(0);
  let direction = $state<"up" | "down">("up");
  let initialized = false;
  let frame = 0;
  let root: HTMLElement;
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");

  $effect(() => {
    const target = value;
    if (!initialized || reduced.matches) {
      initialized = true;
      shown = target;
      return;
    }
    cancelAnimationFrame(frame);
    const from = shown;
    if (from === target) return;
    direction = target > from ? "up" : "down";
    // 差得少时一格一格滚，差得多时按缓动直接滚到终点，总时长不超过约 0.7 秒。
    const started = performance.now();
    const duration = Math.min(700, 160 + Math.abs(target - from) * 90);
    const step = (now: number) => {
      const t = Math.min(1, (now - started) / duration);
      const eased = 1 - (1 - t) ** 3;
      shown = Math.round(from + (target - from) * eased);
      if (t < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
  });

  export function bump() {
    if (reduced.matches || !root) return;
    root.animate([
      { transform: "scale(1)" },
      { transform: "scale(1.35)", offset: .35 },
      { transform: "scale(.94)", offset: .7 },
      { transform: "scale(1)" },
    ], { duration: 420, easing: "cubic-bezier(.3,1.4,.5,1)" });
  }

  onDestroy(() => cancelAnimationFrame(frame));
  const digits = $derived(shown.toLocaleString("zh-CN").split(""));
</script>

<span class="rolling" bind:this={root} aria-label={shown.toLocaleString("zh-CN")}>
  {#each digits as digit, index (`${digits.length}-${index}`)}
    {#key digit}<span class="digit {direction}" aria-hidden="true">{digit}</span>{/key}
  {/each}
</span>

<style>
  .rolling { display: inline-flex; overflow: hidden; vertical-align: bottom; transform-origin: 50% 60%; }
  .digit { display: inline-block; animation: 120ms cubic-bezier(.2,.7,.3,1) both; }
  .digit.down { animation-name: from-above; }
  .digit.up { animation-name: from-below; }
  @keyframes from-above { from { transform: translateY(-70%); opacity: 0; } to { transform: none; opacity: 1; } }
  @keyframes from-below { from { transform: translateY(70%); opacity: 0; } to { transform: none; opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .digit { animation: none; } }
</style>
