<script lang="ts">
  // 面板根组件：持有 PanelState，摆放顶部（标题、记录状态、搜索框、按键提示）、列表和页脚。
  import { onMount } from "svelte";
  import { PanelState, formatTime } from "../state.svelte";
  import SearchBox from "./SearchBox.svelte";
  import HistoryList from "./HistoryList.svelte";
  import Footer from "./Footer.svelte";

  const panel = new PanelState();
  panel.watch();
  onMount(panel.connect);
</script>

<svelte:window onkeydown={panel.keydown} oncontextmenu={(event) => event.preventDefault()} />

<main aria-label="拈来剪贴板样机">
  {#if !panel.visible}
    <!-- 隐藏期只保留空壳，避免下次窗口先显示时露出旧会话的就绪列表。 -->
  {:else if panel.interaction === "passive"}
    <div class="passive-feedback">
      <p role="status">{panel.message || "正在确认复制结果…"}</p>
      <button type="button" onclick={panel.close}>关闭提示</button>
    </div>
  {:else}
    <header>
      <div class="heading"><strong>拈来</strong><span>G0 · 样机四</span><span class="count">{panel.query ? `${panel.filtered.length} / ` : ""}{panel.history.total} 条</span></div>
      <div class="recording" class:inactive={!panel.history.recording} role="status">{panel.history.recording ? "记录中" : "未在记录"}{panel.history.last_recorded_ms ? ` · 上次 ${formatTime(panel.history.last_recorded_ms)}` : ""}{panel.mode === "quick" && panel.history.total > 300 ? " · 显示最近 300 条" : ""}</div>
      {#if panel.history.store_status}<div class="store-status" role="status">{panel.history.store_status}</div>{/if}
      {#if panel.interactive}
        <SearchBox {panel} />
        <div class="hint">
          {#if panel.interaction === "activated"}
            {panel.mode === "quick" ? "↑ ↓ 选择 · 1–9 复制 · Enter 复制 · Ctrl+Enter 转为文本复制 · Shift+Enter 复制 · Esc 收起" : "↑ ↓ 选择 · Ctrl+1–9 复制 · Enter 复制 · Ctrl+Enter 转为文本复制 · Shift+Enter 复制 · Esc 返回"}
          {:else}
            {panel.mode === "quick" ? "↑ ↓ 选择 · 1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 收起" : "↑ ↓ 选择 · Ctrl+1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 返回"}
          {/if}
        </div>
      {/if}
    </header>

    {#if panel.interactive}
      <HistoryList {panel} />
    {:else}
      <section class="activation" aria-label="面板激活状态">
        <p role="status">{panel.interaction === "activation_pending" || panel.activationRetrying ? "正在取得键盘焦点，请稍候…" : "未取得键盘焦点，请点击面板重试"}</p>
        {#if panel.interaction === "activation_failed"}
          <button type="button" disabled={panel.activationRetrying} onclick={(event) => panel.retryActivation(event)}>点击重试</button>
        {/if}
        <button type="button" onclick={panel.close}>关闭面板</button>
      </section>
    {/if}

    <Footer {panel} />
  {/if}
</main>

<style>
  main { height: 100%; display: flex; flex-direction: column; border: 1px solid var(--color-border-panel); }
  header { padding: var(--space-header-inset) var(--space-header-inset) var(--space-2); }
  .heading { display: flex; align-items: baseline; gap: var(--space-heading-gap); margin-bottom: var(--space-3); }
  .heading strong { font-size: var(--font-size-heading); font-weight: 650; }
  .heading span, .hint { color: var(--color-text-secondary); font-size: var(--font-size-caption); }
  .count { margin-left: auto; }
  .recording { color: var(--color-accent); font-size: var(--font-size-caption); margin: var(--space-recording-before) 0 var(--space-recording-after); }
  .recording.inactive { color: var(--color-caution); }
  .hint { margin-top: var(--space-2); }
  .store-status { color: var(--color-caution); font-size: var(--font-size-caption); margin-bottom: var(--space-2); }
  .activation, .passive-feedback { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-3); padding: var(--space-3); text-align: center; }
  .activation p, .passive-feedback p { margin: 0; line-height: 1.6; }
  button { border: 1px solid var(--color-border-control); border-radius: var(--radius-md); padding: var(--space-2) var(--space-3); background: var(--color-bg-layer); }
  button:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 2px; }
</style>
