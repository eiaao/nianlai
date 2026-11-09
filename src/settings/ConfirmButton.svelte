<script lang="ts">
  import { reducedMotion } from "../lib/preferences";
  // 原地确认的清理按钮（M2-3）：
  //   点一下 → 按钮变宽、换成琥珀色“确认清理 N 条”，底部一条 4 秒的细线在缩短，走完自动恢复；
  //   再点 → 转圈 → 画出 ✓，同时一个“+N”小标签沿弧线飞向回收站那一行（flyTo），落下时调用 onlanded。
  // 减少动画时不飞、不变宽，只换文字和 ✓。
  let { label, confirmLabel, doneLabel = "已移到回收站", disabled = false, action, flyTo, onlanded }: {
    label: string;
    confirmLabel: string;
    doneLabel?: string;
    disabled?: boolean;
    action: () => Promise<number>;
    flyTo?: () => HTMLElement | null | undefined;
    onlanded?: (count: number) => void;
  } = $props();

  const ARM_MS = 4000;
  let phase = $state<"idle" | "armed" | "working" | "done">("idle");
  let error = $state("");
  let button: HTMLButtonElement;
  let sizes: Record<string, HTMLSpanElement> = {};
  let width = $state<number | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;
  const reduced = reducedMotion;

  // 按钮宽度跟着文字平滑变化：每种状态的文字先在隐藏层里量好宽度。
  $effect(() => {
    void label; void confirmLabel;
    const key = phase === "working" ? "done" : phase;
    const node = sizes[key];
    width = node ? node.offsetWidth : null;
  });

  function reset() {
    if (timer) clearTimeout(timer);
    timer = null;
    phase = "idle";
  }

  async function press() {
    if (phase === "idle") {
      error = "";
      phase = "armed";
      timer = setTimeout(reset, ARM_MS);
      return;
    }
    if (phase !== "armed") return;
    if (timer) clearTimeout(timer);
    phase = "working";
    try {
      const count = await action();
      phase = "done";
      const target = flyTo?.();
      if (count > 0 && target && !reduced()) await fly(count, target);
      onlanded?.(count);
      timer = setTimeout(reset, 1600);
    } catch (value) {
      error = typeof value === "string" ? value : value instanceof Error ? value.message : "清理没有完成，请重试";
      reset();
    }
  }

  // “+N”小标签从按钮飞到目标：先往上弹起一点，再沿弧线落下、缩小。
  function fly(count: number, target: HTMLElement) {
    const from = button.getBoundingClientRect();
    const to = target.getBoundingClientRect();
    const chip = document.createElement("span");
    chip.className = "fly-chip";
    chip.textContent = `+${count}`;
    Object.assign(chip.style, { left: `${from.left + from.width / 2}px`, top: `${from.top + from.height / 2}px` });
    document.body.append(chip);
    const dx = to.left + to.width / 2 - (from.left + from.width / 2);
    const dy = to.top + to.height / 2 - (from.top + from.height / 2);
    const lift = Math.max(36, Math.abs(dy) * .35);
    const frames: Keyframe[] = [];
    for (let i = 0; i <= 20; i++) {
      const t = i / 20;
      const x = dx * t;
      const y = dy * t - lift * 4 * t * (1 - t);
      const scale = t < .15 ? .6 + t / .15 * .55 : 1.15 - (t - .15) / .85 * .45;
      frames.push({ transform: `translate(-50%, -50%) translate(${x}px, ${y}px) scale(${scale})`, opacity: t > .9 ? (1 - t) / .1 : 1 });
    }
    return chip.animate(frames, { duration: 620, easing: "cubic-bezier(.4,0,.6,1)", fill: "forwards" })
      .finished.catch(() => {}).finally(() => chip.remove());
  }
</script>

<span class="wrap">
  <button bind:this={button} type="button" class="confirm-button {phase}" disabled={disabled && phase === "idle" || phase === "working"}
    style={width && !reduced() ? `width:${width + 26}px` : undefined} onclick={press} onblur={() => { if (phase === "armed") reset(); }}>
    {#key phase === "working" ? "done" : phase}
      <span class="text">
        {#if phase === "idle"}{label}
        {:else if phase === "armed"}{confirmLabel}
        {:else if phase === "working"}<i class="spinner" aria-hidden="true"></i>
        {:else}<svg class="check" viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8.5 6.5 12 13 4.5" /></svg>{doneLabel}{/if}
      </span>
    {/key}
    {#if phase === "armed"}<i class="drain" style={`animation-duration:${ARM_MS}ms`} aria-hidden="true"></i>{/if}
  </button>
  <span class="measure" aria-hidden="true">
    <span bind:this={sizes.idle}>{label}</span><span bind:this={sizes.armed}>{confirmLabel}</span><span bind:this={sizes.done}><svg class="check" viewBox="0 0 16 16"></svg>{doneLabel}</span>
  </span>
  {#if error}<small class="error">{error}</small>{/if}
</span>

<style>
  .wrap { position: relative; display: inline-flex; flex: none; flex-direction: column; align-items: flex-end; }
  .confirm-button { position: relative; min-height: 28px; display: inline-flex; align-items: center; justify-content: center; overflow: hidden; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; white-space: nowrap; transition: width 220ms cubic-bezier(.3,.8,.3,1), border-color 160ms, color 160ms, background-color 160ms; }
  .confirm-button:disabled { opacity: .6; }
  .confirm-button.working:disabled { opacity: 1; }
  .confirm-button.armed { border-color: var(--state-caution); background: color-mix(in srgb, var(--state-caution) 10%, var(--settings-card)); color: var(--state-caution); font-weight: 600; }
  .confirm-button.done { border-color: var(--settings-accent); background: color-mix(in srgb, var(--settings-accent) 12%, var(--settings-card)); color: var(--settings-accent); font-weight: 600; }
  .text { display: inline-flex; align-items: center; gap: 5px; animation: swap 180ms cubic-bezier(.2,.8,.3,1) both; }
  .drain { position: absolute; left: 0; right: 0; bottom: 0; height: 2px; background: var(--state-caution); transform-origin: left; animation: drain linear forwards; }
  .spinner { width: 12px; height: 12px; border: 1.5px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: spin 600ms linear infinite; }
  .check { width: 13px; height: 13px; flex: none; }
  .check path { fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; stroke-dasharray: 16; stroke-dashoffset: 16; animation: draw 320ms 60ms cubic-bezier(.4,0,.2,1) forwards; }
  /* 量宽度用的隐藏层：自身不占地方，也不撑出横向滚动条。 */
  .measure { position: absolute; right: 0; top: 0; width: 0; height: 0; overflow: hidden; visibility: hidden; pointer-events: none; white-space: nowrap; font-size: 11px; font-weight: 600; }
  .measure span { position: absolute; display: inline-flex; gap: 5px; }
  .error { margin-top: 3px; color: var(--state-critical); font-size: 10px; }
  :global(.fly-chip) { position: fixed; z-index: 1000; padding: 2px 8px; border-radius: 999px; background: var(--settings-accent); color: var(--settings-card); font: 600 11px var(--font-mono); pointer-events: none; box-shadow: 0 4px 12px color-mix(in srgb, var(--settings-accent) 35%, transparent); }
  @keyframes swap { from { opacity: 0; transform: translateY(5px) scale(.96); } to { opacity: 1; transform: none; } }
  @keyframes drain { from { transform: scaleX(1); } to { transform: scaleX(0); } }
  @keyframes spin { to { transform: rotate(360deg); } }
  @keyframes draw { to { stroke-dashoffset: 0; } }
  /* 只在设置里选了“跟随系统”且系统开了“减少动画效果”时生效（data-reduce-motion）。 */
  :global(html[data-reduce-motion="true"]) .text, :global(html[data-reduce-motion="true"]) .drain, :global(html[data-reduce-motion="true"]) .spinner { animation: none; }
  :global(html[data-reduce-motion="true"]) .check path { animation: none; stroke-dashoffset: 0; }
  :global(html[data-reduce-motion="true"]) .confirm-button { transition: none; }
</style>
