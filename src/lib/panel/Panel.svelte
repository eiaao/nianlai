<script lang="ts">
  import { onMount } from "svelte";
  import { PanelState } from "../state.svelte";
  import HandPanel from "./HandPanel.svelte";
  import SearchBox from "./SearchBox.svelte";

  const panel = new PanelState();
  panel.watch();
  onMount(panel.connect);
  let stage: HTMLDivElement;

  // 原生命中区域（SetWindowRgn）也是可见区域：外扩要盖住阴影，否则阴影被裁掉。
  const SHADOW_PAD = 16;
  let lastReport = "";
  // 避让外形只在变化时发（同一姿态不变）；换会话后重发一次。
  let lastAvoid: { session: number; key: string } | null = null;
  type Rect = { left: number; top: number; right: number; bottom: number };
  const union = (a: Rect, b: Rect): Rect => ({ left: Math.min(a.left, b.left), top: Math.min(a.top, b.top), right: Math.max(a.right, b.right), bottom: Math.max(a.bottom, b.bottom) });

  /**
   * 读所有 [data-hit] 的当前矩形；带 data-hit-target 的（牌）再并上目标矩形。DOM 刚更新、过渡还没开始时读，
   * 得到的正好是“新旧位置的并集”；过渡结束（transitionend）后再读一次就收紧到新位置。
   */
  function syncHitRegions() {
    if (!stage) return;
    const armed = panel.visible || !!panel.queue;
    let regions: Rect[] = [];
    if (armed && panel.dragging) {
      // 拖动一张牌时整个窗口都可见、可点，松手后恢复。
      regions = [{ left: 0, top: 0, right: window.innerWidth, bottom: window.innerHeight }];
    } else if (armed) {
      for (const element of stage.querySelectorAll<HTMLElement>("[data-hit]")) {
        const rect = element.getBoundingClientRect();
        let box: Rect | null = rect.width > 0 && rect.height > 0 ? { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom } : null;
        const target = element.dataset.hitTarget?.split(",").map(Number);
        if (target?.length === 4 && target.every(Number.isFinite)) {
          const next = { left: target[0], top: target[1], right: target[2], bottom: target[3] };
          box = box ? union(box, next) : next;
        }
        if (box) regions.push({ left: box.left - SHADOW_PAD, top: box.top - SHADOW_PAD, right: box.right + SHADOW_PAD, bottom: box.bottom + SHADOW_PAD + 8 });
      }
    }
    const scale = window.devicePixelRatio || 1;
    const outline = panel.avoidOutline;
    const avoidKey = outline === undefined ? "" : JSON.stringify(outline);
    const avoid = outline === undefined || (lastAvoid?.session === panel.session && lastAvoid.key === avoidKey) ? undefined : outline;
    const key = `${panel.session}:${armed}|${avoidKey || "keep"}|${scale}|${regions.map((r) => `${Math.round(r.left)},${Math.round(r.top)},${Math.round(r.right)},${Math.round(r.bottom)}`).join(";")}`;
    if (key === lastReport) return;
    lastReport = key;
    if (avoid !== undefined) lastAvoid = { session: panel.session, key: avoidKey };
    panel.reportHitRegions(regions, scale, armed, avoid);
  }

  // DOM 一变就同步重报（微任务里、绘制之前）；过渡 / 动画结束、窗口缩放、DPI 变化也重报。
  let queued = false;
  const schedule = () => { if (queued) return; queued = true; queueMicrotask(() => { queued = false; syncHitRegions(); }); };
  let frame = 0;
  const settle = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(() => syncHitRegions()); };
  onMount(() => {
    const mutations = new MutationObserver(schedule);
    mutations.observe(stage, { subtree: true, childList: true, attributes: true, characterData: true });
    const resize = new ResizeObserver(schedule);
    resize.observe(stage);
    let media: MediaQueryList | null = null;
    const watchResolution = () => {
      media?.removeEventListener("change", onResolution);
      media = matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      media.addEventListener("change", onResolution);
    };
    function onResolution() { watchResolution(); lastReport = ""; schedule(); }
    watchResolution();
    stage.addEventListener("transitionend", settle);
    stage.addEventListener("animationend", settle);
    window.addEventListener("resize", schedule);
    return () => {
      mutations.disconnect(); resize.disconnect(); media?.removeEventListener("change", onResolution);
      stage.removeEventListener("transitionend", settle); stage.removeEventListener("animationend", settle);
      window.removeEventListener("resize", schedule); cancelAnimationFrame(frame);
    };
  });

  // 状态变化本身（显示 / 隐藏、拖动开始结束、新会话）也重报；会话变了强制重报。
  $effect(() => {
    panel.visible; panel.queue; panel.dragging; panel.interaction; panel.pose; panel.avoidOutline;
    panel.session;
    schedule();
  });
</script>

<svelte:window onkeydown={panel.keydown} onkeyup={panel.keyup} onblur={panel.windowBlur} oncontextmenu={(event) => event.preventDefault()} />

<div bind:this={stage} class="panel-shell" class:warming={panel.warming && !panel.visible} data-mode={panel.mode} data-list={panel.handList} data-pose={panel.pose} role="dialog" tabindex="-1" aria-label="拈来手牌">
  {#if panel.queue && !panel.visible}
    <div class="queue-pill" data-hit role="status" aria-live="polite">
      <span class="queue-count">{panel.queue.error ? "!" : panel.queue.remaining}</span>
      {#if panel.queue.error}
        <span class="queue-error">{panel.queue.error}</span>
      {:else if panel.queue.automatic}
        <span>正在一起粘贴 · 还剩 <b>{panel.queue.remaining}</b> 张</span>
      {:else}
        <span>逐张放 · 还剩 <b>{panel.queue.remaining}</b> 张 · 按 <kbd>Ctrl+V</kbd> 粘贴下一张</span>
      {/if}
      {#if panel.queue.skipped && !panel.queue.error}<span>已跳过 {panel.queue.skipped} 张</span>{/if}
      {#if !panel.queue.error}<small>Esc 取消</small>{/if}<button type="button" tabindex="-1" aria-label="取消逐张放" onclick={panel.cancelQueue}>×</button>
    </div>
  {:else if !panel.visible && !panel.warming}
    <!-- 隐藏期不保留旧会话手牌。 -->
  {:else if panel.interaction === "close_only"}
    <section class="terminal-feedback" data-hit>
      <span class="terminal-mark" aria-hidden="true">△</span>
      <p role="status">{panel.message || "本次未提交，请关闭面板后重新呼出"}</p>
      <button type="button" onclick={panel.close}>关闭面板</button>
    </section>
  {:else if !panel.interactive && !panel.warming && panel.interaction !== "passive"}
    {#if panel.inputSurface}<div class="focus-search" data-hit><SearchBox {panel} /></div>{/if}
    <section class="activation" data-hit aria-label="面板激活状态">
      {#if panel.interaction === "activated"}
        <p role="status">{panel.searchStartFailed ? "未能进入搜索，请关闭面板后重新呼出。" : panel.searchStarting ? "正在进入搜索…" : panel.domFocus === "failed" ? "未取得输入焦点，请点击重试" : "正在取得输入焦点，请稍候…"}</p>
        {#if panel.domFocus === "failed" && !panel.searchStarting && !panel.searchStartFailed}
          <button type="button" onclick={(event) => panel.retryDomFocus(event)}>点击重试输入焦点</button>
        {/if}
      {:else}
        <p role="status">{panel.interaction === "activation_pending" || panel.activationRetrying ? "正在取得键盘焦点，请稍候…" : "未取得键盘焦点，请点击面板重试"}</p>
      {/if}
      {#if panel.interaction === "activation_failed"}
        <button type="button" disabled={panel.activationRetrying} onclick={(event) => panel.retryActivation(event)}>点击重试</button>
      {/if}
      <button type="button" onclick={panel.close}>关闭面板</button>
    </section>
  {:else}
    <HandPanel {panel} />
  {/if}
</div>

<style>
  .panel-shell { position: relative; width: 100%; height: 100%; overflow: hidden; background: transparent; }
  .panel-shell.warming { visibility: hidden; }
  .focus-search { position: absolute; z-index: 160; left: 50%; top: 30px; width: 420px; height: 38px; transform: translateX(-50%); }
  .activation, .terminal-feedback { position: absolute; left: 50%; top: 50%; width: 460px; min-height: 180px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 24px; transform: translate(-50%,-50%); border: 1px solid var(--hand-line); border-radius: 12px; background: var(--hand-card); color: var(--hand-muted); box-shadow: var(--hand-pop); text-align: center; }
  .activation p, .terminal-feedback p { margin: 0; font-size: 13px; line-height: 1.8; }
  .activation button, .terminal-feedback button { min-height: 32px; padding: 5px 12px; border: 1px solid var(--hand-line); border-radius: 6px; background: var(--hand-paper); color: var(--hand-ink); font-size: 12px; }
  .terminal-mark { color: var(--state-critical); font-size: 20px; }
  .queue-pill { width: 100%; height: 100%; display: flex; align-items: center; gap: 9px; padding: 0 8px 0 10px; border: 1px solid var(--hand-line); border-radius: 22px; background: var(--hand-paper); color: var(--hand-muted); box-shadow: var(--hand-pop); font-size: 12px; white-space: nowrap; }
  .queue-count { width: 24px; height: 24px; display: grid; place-items: center; flex: none; border-radius: 50%; background: var(--hand-seal); color: var(--hand-on-seal); font-weight: 700; }
  .queue-pill b { color: var(--hand-ink); }
  .queue-pill small { margin-left: auto; flex: none; color: var(--hand-muted); }
  .queue-pill > span:not(.queue-count) { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .queue-pill button { width: 24px; height: 24px; flex: none; margin-left: auto; padding: 0; border: 0; background: transparent; color: var(--hand-muted); font-size: 20px; }
  /* 错误原因单行省略，× 永远可见可点。 */
  .queue-pill > .queue-error { min-width: 0; flex: 1; overflow: hidden; color: var(--state-critical); text-overflow: ellipsis; white-space: nowrap; }
  button:focus-visible { outline: 2px solid var(--hand-seal); outline-offset: 3px; }
</style>
