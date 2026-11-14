<script lang="ts">
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import {
    GuideFlow,
    type GuideEffect,
    type GuideEvent,
    type GuideSettingsPage,
    type GuideState,
  } from "../lib/guide";
  import { normalizePreferences, type Preferences } from "../lib/preferences";
  import type { AppSettings, HistorySnapshot, UnlistenFn } from "../lib/ipc";

  let flow: GuideState = $state(GuideFlow.createState());
  let snapshot = $state<HistorySnapshot | null>(null);
  let preferences = $state<Preferences>(normalizePreferences(undefined));
  let hotkey = $state("Ctrl+`");
  let loaded = $state(false);
  let loadError = $state("");
  let runtimeError = $state("");
  let systemReduced = $state(false);
  let flyVisible = $state(false);
  let animationCount = $state(0);
  let paper = $state<HTMLElement>();
  let destroyed = false;
  let acceptEvents = false;
  let loadGeneration = 0;
  let animationSeen = false;
  let flyTimer: ReturnType<typeof setTimeout> | undefined;
  let persistQueue: Promise<void> = Promise.resolve();
  let historyQueue: Promise<void> = Promise.resolve();
  const unlisteners: UnlistenFn[] = [];

  const visibleGuide = $derived(flow.guide);
  const sample = $derived(snapshot?.items.find((item) => item.id === visibleGuide.sample_id) ?? null);
  const reduced = $derived(preferences.motion === "none" || (preferences.followSystemMotion && systemReduced));
  const currentStep = $derived(visibleGuide.phase === "await_copy" ? 1 : visibleGuide.phase === "await_show" ? 2 : 3);
  const retryOpen = $derived(flow.notice?.kind === "paste_failed" || flow.notice?.message.startsWith("面板已收起") === true);
  const needsOpen = $derived(visibleGuide.phase === "await_paste"
    && (flow.session === null || !flow.targetConfirmed || retryOpen));

  function errorMessage(error: unknown, fallback: string): string {
    return error instanceof Error && error.message ? error.message : typeof error === "string" && error ? error : fallback;
  }

  function applySettings(value: AppSettings) {
    preferences = normalizePreferences(value);
    hotkey = value.hotkey ?? "Ctrl+`";
    document.documentElement.dataset.theme = preferences.theme;
    document.documentElement.dataset.motion = preferences.motion;
    document.documentElement.dataset.reduceMotion = preferences.followSystemMotion && systemReduced ? "true" : "false";
    if (reduced) flyVisible = false;
  }

  function markFirstRecord(previous: GuideState, event: GuideEvent) {
    if (event.type !== "persist_succeeded" || previous.guide.phase !== "await_copy"
      || flow.guide.phase !== "await_show" || animationSeen) return;
    animationSeen = true;
    if (reduced) return;
    animationCount += 1;
    flyVisible = true;
    if (flyTimer !== undefined) clearTimeout(flyTimer);
    flyTimer = setTimeout(() => { flyVisible = false; }, 720);
  }

  function dispatch(event: GuideEvent) {
    if (destroyed) return;
    if (!loaded && (event.type === "skip_guide" || event.type === "close_guide" || event.type === "skip_step")) {
      // 两个跳过语义在首读期间也可用；用户明确选择终态后，迟到读取不得将其覆盖。
      loadGeneration += 1;
      loaded = true;
      acceptEvents = false;
    }
    const previous = flow;
    const transition = GuideFlow.reduce(previous, event);
    flow = transition.state;
    markFirstRecord(previous, event);
    for (const effect of transition.effects) execute(effect);
  }

  async function perform(effect: Exclude<GuideEffect, { type: "persist_guide" }>) {
    if (effect.type === "copy_sample" || effect.type === "open_panel" || effect.type === "open_settings") {
      runtimeError = "";
    }
    try {
      switch (effect.type) {
        case "copy_sample":
          await ipc.guideCopySample();
          break;
        case "open_panel":
          await ipc.guideOpenPanel();
          break;
        case "open_settings":
          await ipc.openSettings(effect.page);
          break;
        case "deal_cards":
          // 持久事实已经保存成功；这里只发定向通知，普通 panel 不新增广播监听。
          await ipc.notifyGuideCardsDealt();
          break;
        case "close_guide":
          await ipc.closeGuide();
          break;
      }
    } catch (error) {
      if (destroyed) return;
      const fallback = effect.type === "copy_sample" ? "示例没复制成，再试一次"
        : effect.type === "open_panel" ? "面板没打开，选好能打字的地方后再试一次"
          : effect.type === "close_guide" ? "纸笺没能关闭，可以再试一次" : "没能完成这一步，再试一次";
      runtimeError = errorMessage(error, fallback);
    }
  }

  function execute(effect: GuideEffect) {
    if (effect.type !== "persist_guide") {
      void perform(effect);
      return;
    }
    // 所有保存严格按 reducer 发出顺序执行；事件仍可继续产生新 token，但不能并发写。
    persistQueue = persistQueue.then(async () => {
      if (destroyed) return;
      try {
        await ipc.setSettings(effect.update);
        if (!destroyed) dispatch({ type: "persist_succeeded", token: effect.token });
      } catch (error) {
        if (!destroyed) {
          dispatch({
            type: "persist_failed", token: effect.token,
            message: errorMessage(error, "没保存上，还是原来的设置"),
          });
        }
      }
    });
  }

  async function readHistory(advance: boolean) {
    try {
      // 历史增长和健康变化共用同一定向通知；两者都读真实接口，顺序仍由 historyQueue 保证。
      const [next, health] = await Promise.all([ipc.getHistory(), ipc.getGuideHealth()]);
      if (destroyed || !acceptEvents) return;
      snapshot = next;
      dispatch({ type: "health_changed", health, paused: flow.paused });
      if (advance && health.store !== "loading") {
        // StoreHealth::Loading 的临时快照不是可信基线；恢复后首个快照只建立基线。
        dispatch({ type: flow.baselineReady ? "history_changed" : "baseline", snapshot: next });
      }
    } catch (error) {
      if (!destroyed) runtimeError = errorMessage(error, "还没读到复制记录或健康状态，再试一次");
    }
  }

  function onHistoryChanged() {
    if (!acceptEvents) return;
    // 每个定向通知都重新读真实历史，且按事件到达顺序完成读取与 reduce。
    historyQueue = historyQueue.then(() => readHistory(true));
  }

  function bind<T>(register: (handler: (payload: T) => void) => Promise<UnlistenFn>, handler: (payload: T) => void) {
    void register(handler).then((unlisten) => {
      if (destroyed) unlisten();
      else unlisteners.push(unlisten);
    }).catch((error) => {
      if (!destroyed) runtimeError = errorMessage(error, "纸笺没能收到后续状态，请重新打开");
    });
  }

  async function load() {
    const generation = ++loadGeneration;
    loaded = false;
    loadError = "";
    runtimeError = "";
    acceptEvents = false;
    const [settingsResult, historyResult, captureResult, healthResult] = await Promise.allSettled([
      ipc.getSettings(), ipc.getHistory(), ipc.getCaptureState(), ipc.getGuideHealth(),
    ]);
    if (destroyed || generation !== loadGeneration) return;

    if (settingsResult.status === "fulfilled") {
      applySettings(settingsResult.value);
      flow = GuideFlow.createState(settingsResult.value.guide);
      animationSeen = flow.guide.phase !== "await_copy";
    } else {
      loadError = errorMessage(settingsResult.reason, "还没读到设置，再试一次");
    }
    if (captureResult.status === "rejected") {
      runtimeError = errorMessage(captureResult.reason, "还没读到记录开关，再试一次");
    }
    if (historyResult.status === "fulfilled") {
      snapshot = historyResult.value;
      const paused = captureResult.status === "fulfilled" ? captureResult.value.paused : flow.paused;
      if (healthResult.status === "fulfilled") {
        const health = healthResult.value;
        if (settingsResult.status === "fulfilled" && health.store !== "loading") {
          dispatch({ type: "restore", snapshot: historyResult.value });
        } else if (settingsResult.status === "rejected") {
          loadError = `${loadError}；没有可信基线，暂未改动引导进度`;
        }
        // restore 的普通保存会清掉旧提示；最后写入真实类型化健康，暂停/故障不能被基线保存掩盖。
        dispatch({ type: "health_changed", health, paused });
      }
    } else {
      const message = errorMessage(historyResult.reason, "还没读到复制记录，再试一次");
      loadError = `${loadError ? `${loadError}；` : ""}还没读到复制记录：${message}`;
    }
    if (healthResult.status === "rejected") {
      const message = errorMessage(healthResult.reason, "还没读到健康状态，再试一次");
      loadError = `${loadError ? `${loadError}；` : ""}没有可信健康状态，暂未改动引导进度：${message}`;
    }
    loaded = true;
    acceptEvents = settingsResult.status === "fulfilled";
    requestAnimationFrame(() => syncHitRegions());
  }

  function syncHitRegions() {
    if (!paper || destroyed) return;
    const rect = paper.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    void ipc.setGuideHitRegions([{ left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }], devicePixelRatio)
      .catch((error) => {
        if (!destroyed) runtimeError = errorMessage(error, "纸笺的点击范围没能更新，请重新打开");
      });
  }

  function openSettings(page: GuideSettingsPage) {
    dispatch({ type: "open_settings", page });
  }

  onMount(() => {
    const media = matchMedia("(prefers-reduced-motion: reduce)");
    systemReduced = media.matches;
    const onMedia = () => {
      systemReduced = media.matches;
      document.documentElement.dataset.reduceMotion = preferences.followSystemMotion && systemReduced ? "true" : "false";
      if (reduced) flyVisible = false;
    };
    media.addEventListener("change", onMedia);

    bind(ipc.onGuideHistoryChanged, onHistoryChanged);
    bind(ipc.onGuidePanelShown, ({ session }) => {
      if (acceptEvents) dispatch({ type: "panel_shown", session });
    });
    bind(ipc.onPanelHidden, ({ session }) => {
      if (acceptEvents) dispatch({ type: "panel_hidden", session });
    });
    bind(ipc.onGuidePasteResult, ({ session, id, result }) => {
      if (acceptEvents) dispatch({ type: "paste_result", session, id, result });
    });
    bind(ipc.onCaptureChanged, ({ paused }) => {
      if (!acceptEvents) return;
      dispatch({ type: "health_changed", health: flow.health, paused });
      // 暂停变化不是复制通知：只更新健康，不把这里读到的增长当成新复制。
      historyQueue = historyQueue.then(() => readHistory(false));
    });
    bind(ipc.onSettingsChanged, (settings) => applySettings(settings));
    void load();

    return () => {
      destroyed = true;
      acceptEvents = false;
      loadGeneration += 1;
      media.removeEventListener("change", onMedia);
      for (const unlisten of unlisteners.splice(0)) unlisten();
      if (flyTimer !== undefined) clearTimeout(flyTimer);
      if (paper) {
        void ipc.setGuideHitRegions([], devicePixelRatio, false).catch(() => {});
      }
    };
  });

  $effect(() => {
    if (!paper || !loaded) return;
    const observer = new ResizeObserver(() => syncHitRegions());
    observer.observe(paper);
    syncHitRegions();
    return () => observer.disconnect();
  });
</script>

<div class="guide-stage">
  <div class="fly-card" data-fly-card hidden={!flyVisible} aria-hidden="true">
    <span></span><span></span><span></span>
  </div>

  <div
    class="guide-paper"
    data-guide-paper
    data-phase={visibleGuide.phase}
    data-pending-phase={flow.pending?.guide.phase ?? ""}
    data-session={flow.session ?? ""}
    data-target-confirmed={flow.targetConfirmed ? "true" : "false"}
    data-animation-count={animationCount}
    bind:this={paper}
    role="dialog"
    aria-label="拈来首次启动"
  >
    <header class="topline">
      <span class="brand">拈来 · 首次启动</span>
      <div class="window-actions">
        <button type="button" class="quiet" data-action="skip-guide" aria-label="跳过整份引导" onclick={() => dispatch({ type: "skip_guide" })}>跳过</button>
        <button type="button" class="close" data-action="close-guide" aria-label="关闭纸笺并跳过整份引导" onclick={() => dispatch({ type: "close_guide" })}>×</button>
      </div>
    </header>

    <ol class="steps" aria-label="首次启动进度">
      {#each ["复制一句", "看它记下", "呼出贴出"] as label, index}
        <li class:active={currentStep === index + 1} class:done={currentStep > index + 1} aria-current={currentStep === index + 1 ? "step" : undefined}>
          <span>{index + 1}</span>{label}
        </li>
      {/each}
    </ol>

    <div class="content">
      {#if visibleGuide.phase === "await_copy"}
        <p class="eyebrow">第一拍 · 复制</p>
        <h1>先复制一句话</h1>
        <p>选中能打字的地方的一句话，按 <kbd>Ctrl</kbd> + <kbd>C</kbd>。拈来真实记下后，才会进入下一拍。</p>
        <div class="actions">
          <button type="button" class="primary" data-action="copy-sample" disabled={!loaded} onclick={() => dispatch({ type: "copy_sample" })}>帮我复制一句</button>
        </div>
      {:else if visibleGuide.phase === "await_show"}
        <p class="eyebrow">第二拍 · 记下了</p>
        <h1>这句已经记下</h1>
        {#if sample}<blockquote data-sample>{sample.preview || "这条没有文字摘要"}</blockquote>{/if}
        <p><strong>先点一下能打字的地方，再呼出拈来。</strong>比如点一下记事本里要写文字的位置。</p>
        <p class="hint">确认后打开面板；你的呼出键是 <kbd>{hotkey}</kbd>。</p>
        <div class="actions">
          <button type="button" class="primary" data-action="confirm-target" aria-label="我已选好位置，呼出拈来" disabled={!loaded} onclick={() => dispatch({ type: "input_target_confirmed" })}>我已选好位置</button>
        </div>
      {:else if visibleGuide.phase === "await_paste"}
        <p class="eyebrow">第三拍 · 贴出</p>
        <h1>这是你的第一手牌</h1>
        {#if sample}<blockquote data-sample>{sample.preview || "这条没有文字摘要"}</blockquote>{/if}
        {#if needsOpen}
          <p><strong>先点一下能打字的地方，再呼出拈来。</strong>确认位置后再试，旧面板会话不会沿用。</p>
          <div class="actions">
            <button
              type="button"
              class="primary"
              data-action={retryOpen ? "retry-paste" : "confirm-target"}
              aria-label="我已选好位置，再试一次"
              disabled={!loaded}
              onclick={() => dispatch({ type: "input_target_confirmed" })}
            >{retryOpen ? "再试一次" : "我已选好位置"}</button>
          </div>
        {:else}
          <p>第 1 张就是上面这条。按 <kbd>1</kbd>，把它贴进你刚选好的位置。</p>
          <p class="hint">面板由你操作；这里只接受这次面板真实返回的结果。</p>
        {/if}
      {:else if visibleGuide.phase === "completed"}
        <p class="eyebrow">完成</p>
        <h1>三拍都完成了</h1>
        <p>进度已经保存。新手牌由面板接着处理。</p>
      {:else}
        <p class="eyebrow">已跳过</p>
        <h1>以后可以再看</h1>
        <p>这次选择已经保存，不会发新手牌。</p>
      {/if}

      {#if flow.pending}
        <p class="saving" role="status">正在保存这一步…</p>
      {/if}
      {#if flow.notice}
        <div class:problem={flow.notice.kind !== "info"} class="notice" role={flow.notice.kind === "info" ? "status" : "alert"}>
          <p>{flow.notice.message}</p>
          {#if flow.notice.settingsPage}
            <button type="button" class="text-button" data-action={`settings-${flow.notice.settingsPage}`} onclick={() => openSettings(flow.notice!.settingsPage!)}>
              打开{flow.notice.settingsPage === "data" ? "数据与备份" : flow.notice.settingsPage === "privacy" ? "记录与隐私" : "通用"}设置
            </button>
          {/if}
        </div>
      {/if}
      {#if loadError}
        <div class="notice problem" role="alert">
          <p>{loadError}</p>
          <button type="button" class="text-button" data-action="retry-load" onclick={() => void load()}>再试一次</button>
        </div>
      {/if}
      {#if runtimeError}<p class="notice problem" role="alert">{runtimeError}</p>{/if}
    </div>
    {#if visibleGuide.phase === "await_paste" || (flow.pending && flow.notice?.kind === "save_failed")}
      <div class="paper-actions">
        {#if visibleGuide.phase === "await_paste"}
          <button type="button" class="quiet" data-action="skip-step" onclick={() => dispatch({ type: "skip_step" })}>跳过这一步</button>
        {/if}
        {#if flow.pending && flow.notice?.kind === "save_failed"}
          <button type="button" class="primary" data-action="retry-persist" onclick={() => dispatch({ type: "retry_persist" })}>重试保存</button>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  :global(html), :global(body), :global(#app) { width: 100%; height: 100%; margin: 0; overflow: hidden; background: transparent; }
  :global(body) { pointer-events: none; font-family: var(--font-ui); font-size: 13px; color: var(--hand-ink); }
  :global(*) { box-sizing: border-box; }
  :global(button) { font: inherit; }
  :global(button:focus-visible) { outline: 2px solid var(--hand-seal); outline-offset: 2px; }
  .guide-stage { position: relative; width: 100%; height: 100%; pointer-events: none; }
  .guide-paper {
    position: absolute; left: 20px; bottom: 0; display: flex; flex-direction: column; width: 320px; max-height: 344px;
    overflow: hidden; pointer-events: auto; padding: 12px 16px 16px;
    background: var(--hand-paper); color: var(--hand-ink); border: 1px solid var(--hand-line-strong);
    border-radius: 4px; box-shadow: var(--hand-lift); scrollbar-width: thin;
  }
  .topline { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-height: 32px; }
  .brand { color: var(--hand-ink-2); font-size: 12px; font-weight: 600; }
  .window-actions { display: flex; align-items: center; gap: 4px; }
  button { min-height: 32px; border: 1px solid var(--hand-line-strong); border-radius: 4px; padding: 4px 12px; background: var(--hand-card); color: var(--hand-ink); cursor: pointer; font-size: 13px; }
  button:hover { border-color: var(--hand-ink-3); }
  button.primary { border-color: var(--hand-seal); background: var(--hand-seal); color: var(--hand-on-seal); font-weight: 600; }
  button.quiet, button.close { background: transparent; border-color: transparent; }
  button.close { width: 32px; padding: 0; font-size: 20px; line-height: 1; }
  .steps { display: flex; gap: 8px; margin: 8px 0 12px; padding: 0; list-style: none; }
  .steps li { display: flex; align-items: center; gap: 4px; color: var(--hand-ink-3); font-size: 12px; white-space: nowrap; }
  .steps li span { display: grid; place-items: center; width: 20px; height: 20px; border: 1px solid var(--hand-line-strong); border-radius: 50%; font-size: 12px; }
  .steps li.active { color: var(--hand-ink); font-weight: 600; }
  .steps li.active span { border-color: var(--hand-ink); }
  .steps li.done { color: var(--hand-ink-2); }
  .content { display: grid; flex: 0 1 auto; gap: 8px; min-height: 0; overflow-y: auto; scrollbar-width: thin; }
  .paper-actions { display: flex; flex: none; flex-wrap: wrap; gap: 8px; padding-top: 8px; background: var(--hand-paper); }
  .eyebrow { margin: 0; color: var(--hand-ink-3); font-size: 12px; }
  h1 { margin: 0; font-family: var(--font-display); font-size: 20px; font-weight: 600; line-height: 1.4; }
  p { margin: 0; font-size: 13px; line-height: 1.65; }
  p.hint { color: var(--hand-ink-2); font-size: 12px; }
  blockquote { margin: 0; padding: 8px 12px; border-left: 2px solid var(--hand-line-strong); background: var(--hand-card); color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; overflow-wrap: anywhere; }
  kbd { padding: 0 4px; border: 1px solid var(--hand-key-line); border-radius: 2px; background: var(--hand-key); font: 12px var(--font-mono); }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 4px; }
  .notice { padding: 8px; border-left: 2px solid var(--hand-line-strong); background: var(--hand-card); }
  .notice.problem { border-left-color: var(--state-caution); }
  .notice p { font-size: 12px; }
  .text-button { min-height: 32px; margin-top: 4px; padding: 4px 0; border-color: transparent; background: transparent; color: var(--hand-ink); text-decoration: underline; text-underline-offset: 3px; font-size: 12px; }
  .saving { color: var(--hand-ink-3); font-size: 12px; }
  .fly-card {
    position: absolute; right: 56px; bottom: 16px; z-index: 2; display: grid; align-content: center; gap: 4px;
    width: 40px; height: 56px; padding: 8px; border: 1px solid var(--hand-line-strong); border-radius: 2px;
    background: var(--hand-card); box-shadow: var(--hand-card-shadow); pointer-events: none;
    animation: fly-to-tray 700ms cubic-bezier(.2,.75,.25,1) both;
  }
  .fly-card[hidden] { display: none; }
  .fly-card span { display: block; height: 4px; background: var(--hand-line-strong); }
  .fly-card span:last-child { width: 60%; }
  @keyframes fly-to-tray {
    0% { opacity: 0; transform: translateY(-72px) scale(1.35) rotate(-4deg); }
    24% { opacity: 1; }
    100% { opacity: 0; transform: translateY(24px) scale(.3) rotate(0); }
  }
  :global(html[data-motion="none"]) .fly-card, :global(html[data-reduce-motion="true"]) .fly-card { display: none !important; animation: none; }
  @media (forced-colors: active) {
    .guide-paper, button, blockquote { border: 1px solid CanvasText; box-shadow: none; }
    button.primary { background: Highlight; color: HighlightText; }
  }
</style>
