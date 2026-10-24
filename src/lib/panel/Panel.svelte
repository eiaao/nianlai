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
  <header>
    <div class="heading"><strong>拈来</strong><span>G0 · 样机四</span><span class="count">{panel.query ? `${panel.filtered.length} / ` : ""}{panel.history.total} 条</span></div>
    <div class="recording" class:inactive={!panel.history.recording} role="status">{panel.history.recording ? "记录中" : "未在记录"}{panel.history.last_recorded_ms ? ` · 上次 ${formatTime(panel.history.last_recorded_ms)}` : ""}{panel.mode === "quick" && panel.history.total > 300 ? " · 显示最近 300 条" : ""}</div>
    {#if panel.history.store_status}<div class="store-status" role="status">{panel.history.store_status}</div>{/if}
    <SearchBox {panel} />
    <div class="hint">{panel.mode === "quick" ? "↑ ↓ 选择 · 1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 收起" : "↑ ↓ 选择 · Ctrl+1–9 取用 · Enter 粘贴 · Ctrl+Enter 粘贴为文本 · Shift+Enter 复制 · Esc 返回"}</div>
  </header>

  <HistoryList {panel} />

  <Footer {panel} />
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
</style>
