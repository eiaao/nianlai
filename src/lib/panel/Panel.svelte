<script lang="ts">
  // 面板根组件只组织三套布局与共享状态表面；会话、搜索、粘贴和焦点仍由 PanelState 独占。
  import { onMount } from "svelte";
  import { PanelState } from "../state.svelte";
  import SearchBox from "./SearchBox.svelte";
  import SplitPreview from "./SplitPreview.svelte";
  import CommandList from "./CommandList.svelte";
  import CardFlow from "./CardFlow.svelte";
  import RecordBanner from "./RecordBanner.svelte";
  import Footer from "./Footer.svelte";

  const panel = new PanelState();
  panel.watch();
  onMount(panel.connect);
</script>

<svelte:window onkeydown={panel.keydown} onkeyup={panel.keyup} onblur={panel.windowBlur} oncontextmenu={(event) => event.preventDefault()} />

<div class="panel-shell" class:panel-a={panel.preferences.layout === "a"} class:panel-b={panel.preferences.layout === "b"} class:panel-c={panel.preferences.layout === "c"} class:ctrl-down={panel.ctrlDown} data-layout={panel.preferences.layout} data-mode={panel.mode} role="dialog" tabindex="-1" aria-label="拈来剪贴板历史">
  {#if !panel.visible}
    <!-- 隐藏期不保留旧会话列表。 -->
  {:else if panel.interaction === "close_only"}
    <section class="terminal-feedback">
      <span class="terminal-mark" aria-hidden="true">△</span>
      <p role="status">{panel.message || "本次未提交，请关闭面板后重新呼出"}</p>
      <button type="button" onclick={panel.close}>关闭面板</button>
    </section>
  {:else}
    <header class="topline">
      <div class="mode-readout">
        {#if panel.preferences.layout === "a"}
          <span class="mode-glyph" aria-hidden="true">{panel.mode === "quick" ? "Q" : "S"}</span>
          <span class="brand-lockup"><strong>拈来</strong><span>取历史之一缕，付当前之一键</span><small>{panel.imeActive ? "IME / 拼写中，暂不搜索" : panel.mode === "quick" ? "SCOPE / LOCAL HISTORY" : "INPUT / FOCUSED"}</small></span>
        {:else if panel.preferences.layout === "b"}
          <span class="b-wordmark">NIANLAI / CLIPBOARD</span>
          <span class="b-mode-label">{panel.imeActive ? "IME / 拼写中" : panel.mode === "quick" ? "Q · 不拿焦点" : "S · 正在输入"}</span>
          <div class="mast-actions">
            <button class:active={panel.currentItem?.favorite} type="button" tabindex="-1" title="收藏 / 取消收藏" aria-label={panel.currentItem?.favorite ? "取消收藏" : "收藏"} aria-pressed={!!panel.currentItem?.favorite} disabled={!panel.currentItem || !panel.interactive || panel.favoritePending !== null} onpointerdown={(event) => event.preventDefault()} onclick={() => { if (panel.currentItem) void panel.toggleFavorite(panel.currentItem.id); }}>
              <svg viewBox="0 0 20 20" fill={panel.currentItem?.favorite ? "currentColor" : "none"} aria-hidden="true"><path d="m10 2.8 2.15 4.35 4.8.7-3.47 3.38.82 4.77L10 13.74 5.7 16l.82-4.77L3.05 7.85l4.8-.7L10 2.8Z" stroke="currentColor" stroke-width="1.3"/></svg><span>收藏</span>
            </button>
          </div>
        {:else}
          <span class="c-brand-copy"><strong>NIANLAI</strong><small>{panel.imeActive ? "IME / COMPOSING" : panel.mode === "quick" ? "QUICK / NO FOCUS" : "SEARCH / FOCUSED"}</small></span>
        {/if}
      </div>
      {#if panel.inputSurface}
        <SearchBox {panel} />
      {:else}
        <div class="search-standby"><span>{panel.interaction === "passive" ? "RESULT / LOCAL ONLY" : "INPUT / WAITING"}</span><span>{panel.history.total} 条</span></div>
      {/if}
    </header>

    <RecordBanner {panel} />

    {#if panel.interactive || panel.interaction === "passive"}
      {#if panel.preferences.layout === "a"}
        <SplitPreview {panel} />
      {:else if panel.preferences.layout === "b"}
        <CommandList {panel} />
      {:else}
        <CardFlow {panel} />
      {/if}
    {:else}
      <section class="activation" aria-label="面板激活状态">
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
    {/if}

    <Footer {panel} />
  {/if}
</div>

<style>
  .panel-shell { position: relative; width: 100%; height: 100%; overflow: hidden; }
  .panel-a { display: grid; grid-template-rows: 56px auto minmax(0, 1fr) auto; border: 1px solid var(--line); border-radius: var(--radius-md); background: var(--panel); color: var(--text); }
  .topline { min-height: 0; display: grid; grid-template-columns: var(--size-a-index) minmax(0, 1fr); border-bottom: 1px solid var(--line); }
  .mode-readout { min-width: 0; display: flex; align-items: center; gap: 11px; padding: 0 15px; border-right: 1px solid var(--line); }
  .mode-glyph { width: 18px; height: 18px; display: grid; place-items: center; flex: none; border: 1px solid var(--faint); color: var(--muted); font: 10px var(--font-mono); }
  .brand-lockup { min-width: 0; display: grid; gap: 1px; }
  .brand-lockup strong { font: 600 15px/1.15 var(--font-display); letter-spacing: .08em; }
  .brand-lockup > span { overflow: hidden; color: var(--muted); font-size: 9px; white-space: nowrap; text-overflow: ellipsis; }
  .brand-lockup small { overflow: hidden; color: var(--faint); font: 8px/1.4 var(--font-mono); letter-spacing: .1em; white-space: nowrap; text-overflow: ellipsis; }
  .panel-a[data-mode="search"] .mode-glyph { border-color: var(--accent); background: var(--accent); color: var(--accent-ink); }
  .panel-a[data-mode="search"] .brand-lockup strong { color: var(--accent); }
  .search-standby { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 0 12px; color: var(--faint); font: 9px var(--font-mono); }
  .activation, .terminal-feedback { min-height: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-3); padding: var(--space-5); color: var(--muted); text-align: center; }
  .activation { background: var(--preview); }
  .activation p, .terminal-feedback p { max-width: 440px; margin: 0; font-size: 12px; line-height: 1.8; }
  .activation button, .terminal-feedback button { min-height: 32px; padding: 5px 12px; border: 1px solid var(--line); border-radius: var(--radius-sm); background: var(--panel-raised); color: var(--text); font-size: 11px; }
  .terminal-feedback { grid-row: 1 / -1; }
  .terminal-mark { color: var(--state-critical); font: 26px var(--font-mono); }
  button:focus-visible { outline: 2px solid var(--focus); outline-offset: 3px; }

  /* 显式绑定所有 grid 行：RecordBanner 不渲染时，后续组件也不能自动提前一行。 */
  .panel-a > .topline { grid-row: 1; }
  .panel-a > :global(.state-banner) { grid-row: 2; }
  .panel-a > :global(.split-host), .panel-a > .activation { grid-row: 3; }
  .panel-a > :global(.statusbar) { grid-row: 4; min-height: 36px; }

  .panel-b { display: grid; grid-template-rows: 34px auto auto minmax(0, 1fr) auto; border: 1px solid var(--text); border-top: 4px solid var(--text); border-radius: 0; background: var(--panel); color: var(--text); }
  .panel-b > .topline { display: contents; }
  .panel-b > .topline > .mode-readout { grid-row: 1; gap: 14px; padding: 0 21px; border-right: 0; }
  .panel-b > .topline > :global(.search-box), .panel-b > .topline > .search-standby { grid-row: 2; }
  .panel-b > :global(.state-banner) { grid-row: 3; }
  .panel-b > :global(.command-host), .panel-b > .activation { grid-row: 4; }
  .panel-b > :global(.statusbar) { grid-row: 5; min-height: 38px; padding: 0 20px; border-color: var(--text); color: var(--faint); }
  .panel-b > :global(.statusbar.has-result) { min-height: 56px; }
  .panel-b :global(.normal-footer) { min-height: 37px; }
  .b-wordmark { margin-right: auto; color: var(--faint); font: 10px var(--font-mono); letter-spacing: .13em; white-space: nowrap; }
  .b-mode-label { display: flex; align-items: center; gap: 6px; color: var(--muted); font: 10px var(--font-mono); white-space: nowrap; }
  .b-mode-label::before { content: ""; width: 6px; height: 6px; border: 1px solid currentColor; }
  .panel-b[data-mode="search"] .b-mode-label { color: var(--accent); }
  .panel-b[data-mode="search"] .b-mode-label::before { border-color: var(--accent); background: var(--accent); }
  .mast-actions { display: flex; align-items: center; gap: 8px; }
  .mast-actions button { display: flex; align-items: center; gap: 4px; padding: 2px; border: 0; border-radius: 0; background: transparent; color: var(--faint); font-size: 10px; }
  .mast-actions button:hover:not(:disabled), .mast-actions button.active { color: var(--accent); }
  .mast-actions button.active { color: var(--star); }
  .mast-actions button:disabled { opacity: .45; }
  .mast-actions svg { width: 13px; height: 13px; }

  .panel-c { display: grid; grid-template-rows: 62px auto minmax(0, 1fr) 65px auto; border: 1px solid var(--c-line); border-radius: var(--radius-sm); background: var(--c-bg); color: var(--c-text); }
  .panel-c > .topline { grid-row: 1; grid-template-columns: 152px minmax(0, 1fr); border-color: var(--c-line); }
  .panel-c > .topline > .mode-readout { padding: 0 15px; border-color: var(--c-line); }
  .panel-c > :global(.state-banner) { grid-row: 2; }
  .panel-c :global(.flow-viewport) { grid-row: 3; }
  .panel-c :global(.info-strip) { grid-row: 4; }
  .panel-c > .activation { grid-row: 3 / 5; }
  .panel-c > :global(.statusbar) { grid-row: 5; min-height: 37px; padding: 0 15px; border-color: var(--c-line); color: var(--c-faint); }
  .panel-c > :global(.statusbar.has-result) { min-height: 56px; }
  .c-brand-copy { min-width: 0; display: grid; gap: 4px; }
  .c-brand-copy strong { font: 600 12px var(--font-mono); letter-spacing: .12em; }
  .c-brand-copy small { overflow: hidden; color: var(--c-faint); font: 9px var(--font-mono); letter-spacing: .06em; text-overflow: ellipsis; white-space: nowrap; }

  @media (max-width: 599px) {
    .panel-a > .topline { grid-template-columns: var(--size-a-index-narrow) minmax(0, 1fr); }
    .panel-a .mode-readout { gap: 7px; padding: 0 8px; }
    .brand-lockup > span { display: none; }
    .brand-lockup strong { font-size: 13px; }
    .brand-lockup small { font-size: 7px; letter-spacing: .03em; }
    .search-standby { padding: 0 7px; font-size: 7px; }
    .panel-b > .topline > .mode-readout { gap: 8px; padding: 0 12px; }
    .b-wordmark { font-size: 8px; letter-spacing: .06em; }
    .b-mode-label { font-size: 8px; }
    .mast-actions button span { display: none; }
    .panel-c > .topline { grid-template-columns: 112px minmax(0, 1fr); }
    .panel-c > .topline > .mode-readout { padding: 0 10px; }
    .c-brand-copy strong { font-size: 10px; }
    .c-brand-copy small { font-size: 7px; letter-spacing: .02em; }
  }
</style>
