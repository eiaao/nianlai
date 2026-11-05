<script lang="ts">
  // 面板根组件只组织 A 布局与状态表面；会话、搜索、粘贴和焦点仍由 PanelState 独占。
  import { onMount } from "svelte";
  import { PanelState } from "../state.svelte";
  import SearchBox from "./SearchBox.svelte";
  import SplitPreview from "./SplitPreview.svelte";
  import RecordBanner from "./RecordBanner.svelte";
  import Footer from "./Footer.svelte";

  const panel = new PanelState();
  panel.watch();
  onMount(panel.connect);
</script>

<svelte:window onkeydown={panel.keydown} onkeyup={panel.keyup} onblur={panel.windowBlur} oncontextmenu={(event) => event.preventDefault()} />

<div class="panel-a" data-mode={panel.mode} role="dialog" tabindex="-1" aria-label="拈来剪贴板历史">
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
        <span class="mode-glyph" aria-hidden="true">{panel.mode === "quick" ? "Q" : "S"}</span>
        <span class="brand-lockup"><strong>拈来</strong><span>取历史之一缕，付当前之一键</span><small>{panel.imeActive ? "IME / 拼写中，暂不搜索" : panel.mode === "quick" ? "SCOPE / LOCAL HISTORY" : "INPUT / FOCUSED"}</small></span>
      </div>
      {#if panel.inputSurface}
        <SearchBox {panel} />
      {:else}
        <div class="search-standby"><span>{panel.interaction === "passive" ? "RESULT / LOCAL ONLY" : "INPUT / WAITING"}</span><span>{panel.history.total} 条</span></div>
      {/if}
    </header>

    <RecordBanner {panel} />

    {#if panel.interactive || panel.interaction === "passive"}
      <SplitPreview {panel} />
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
  .panel-a { position: relative; width: 100%; height: 100%; display: grid; grid-template-rows: 56px auto minmax(0, 1fr) auto; overflow: hidden; border: 1px solid var(--line); border-radius: var(--radius-md); background: var(--panel); color: var(--text); }
  /* 显式绑定 grid 行：state-banner 无状态时不渲染，auto-placement 不会把 split-host 挤进 auto 行、statusbar 拉成 1fr。
     子组件根元素不带本组件的 scoped 类，跨组件选择器用 :global 包一层。 */
  .panel-a > .topline { grid-row: 1; }
  .panel-a > :global(.split-host) { grid-row: 3; }
  .panel-a > :global(.statusbar) { grid-row: 4; min-height: 36px; }
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
  @media (max-width: 599px) {
    .topline { grid-template-columns: var(--size-a-index-narrow) minmax(0, 1fr); }
    .mode-readout { gap: 7px; padding: 0 8px; }
    .brand-lockup > span { display: none; }
    .brand-lockup strong { font-size: 13px; }
    .brand-lockup small { font-size: 7px; letter-spacing: .03em; }
    .search-standby { padding: 0 7px; font-size: 7px; }
  }
</style>
