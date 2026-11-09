<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import * as ipc from "../lib/ipc";
  import HotkeyEditor from "./HotkeyEditor.svelte";
  import CapturePage from "./CapturePage.svelte";
  import StoragePage from "./StoragePage.svelte";
  import type { AppSettings, CaptureSettings, SettingsUpdate } from "../lib/ipc";
  import {
    applyPreferences, DEFAULT_PREFERENCES, normalizePreferences,
    type DensityPreference, type MotionPreference, type Preferences, type ThemePreference,
  } from "../lib/preferences";

  const pages = [
    { id: "general", label: "通用" },
    { id: "record", label: "隐私与记录" },
    { id: "storage", label: "数据与备份" },
    { id: "interaction", label: "交互" },
    { id: "about", label: "关于" },
  ];
  let page = $state("general");
  // “数据与备份”的子页面；面板里的回收站入口打开设置时带 #trash 直接跳过来。
  let storageView = $state<"main" | "trash" | "snapshots" | "images">("main");

  function navigate(target: string) {
    if (target === "trash" || target === "snapshots" || target === "images") {
      page = "storage";
      storageView = target;
    } else if (pages.some((entry) => entry.id === target)) {
      page = target;
      storageView = "main";
    }
  }

  onMount(() => {
    navigate(decodeURIComponent(location.hash.slice(1)));
    let stop: (() => void) | undefined;
    let disposed = false;
    void listen<string>("settings:navigate", (event) => navigate(event.payload)).then((unlisten) => {
      if (disposed) unlisten(); else stop = unlisten;
    });
    return () => { disposed = true; stop?.(); };
  });
  let preferences = $state<Preferences>({ ...DEFAULT_PREFERENCES });
  let hotkey = $state("Ctrl+`");
  let capture = $state<CaptureSettings | null>(null);
  let snapshotMaxMb = $state(1024);
  let loading = $state(true);
  let saving = $state(false);
  let status = $state("正在读取设置…");
  let failed = $state(false);
  // 系统“减少动画效果”状态：生效且用户选了非“关闭”时给出醒目提示，避免“设置了却没反应”的困惑。
  let reducedMotion = $state(false);

  onMount(() => {
    const media = matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = media.matches;
    const onMedia = () => { reducedMotion = media.matches; };
    media.addEventListener("change", onMedia);
    return () => media.removeEventListener("change", onMedia);
  });

  function apply(settingsValue: AppSettings) {
    hotkey = settingsValue.hotkey ?? "Ctrl+`";
    capture = (settingsValue.capture as CaptureSettings | undefined) ?? null;
    snapshotMaxMb = settingsValue.snapshots?.max_mb ?? 1024;
    preferences = normalizePreferences(settingsValue);
    applyPreferences(preferences);
  }

  async function save(update: SettingsUpdate, message: string): Promise<AppSettings | null> {
    if (saving || loading) return null;
    saving = true; failed = false; status = "正在保存…";
    try {
      const saved = await ipc.setSettings(update);
      apply(saved);
      status = message;
      return saved;
    } catch (error) {
      failed = true;
      status = error instanceof Error ? error.message : typeof error === "string" ? error : "设置未保存，原设置已保留";
      return null;
    } finally {
      saving = false;
    }
  }

  onMount(async () => {
    try {
      apply(await ipc.getSettings());
      status = "改动自动保存，即时生效";
    } catch {
      failed = true;
      status = "设置读取失败，当前显示默认值；请关闭窗口后重试";
    } finally {
      loading = false;
    }
  });
</script>

<svelte:head><title>拈来 · 设置</title></svelte:head>

<section class="settings" aria-label="拈来设置">
  <nav class="nav" aria-label="设置分区">
    <div class="nav-brand"><span class="nav-mark">N</span><span><strong>拈来</strong><small>取历史之一缕，付当前之一键</small><em>SCOPE / SETTINGS</em></span></div>
    {#each pages as entry}
      <button type="button" class:active={page === entry.id} aria-current={page === entry.id ? "page" : undefined} onclick={() => navigate(entry.id)}>{entry.label}</button>
    {/each}
    <div class="nav-foot">本地设置<br>外观自动保存 · 快捷键确认保存</div>
  </nav>

  <main class="main">
    <header class="main-head"><h1>{pages.find((entry) => entry.id === page)?.label}</h1><span>SNAPSHOT / LOCAL ONLY</span></header>
    <div class="content">
      {#if page === "general"}
        <section class="page">
          <div><h2>面板与呼出</h2><p class="page-intro">布局、主题和动效是独立维度。A/B/C 共用历史、收藏、搜索与选中项。</p></div>
          <div class="group">
            <span class="group-title">面板布局</span>
            <div class="layout-options" role="radiogroup" aria-label="面板布局">
              <button class="layout-option" type="button" role="radio" aria-checked={preferences.layout === "a"} disabled={loading || saving} onclick={() => save({ panel: { layout: "a" } }, "已使用分栏预览")}>
                <span class="layout-check">{preferences.layout === "a" ? "✓" : ""}</span>
                <svg class="mini" viewBox="0 0 180 62" aria-hidden="true"><rect x="66" y="19" width="102" height="34" fill="currentColor" opacity=".12"/><path d="M62 0v62M0 16h180M74 25h92M74 35h74M74 45h58M10 27h40M10 39h40M10 51h40" stroke="currentColor" fill="none"/></svg>
                <strong>分栏预览</strong><small>默认。窄列表与常驻大预览，适合核对内容。</small>
              </button>
              <button class="layout-option" type="button" role="radio" aria-checked={preferences.layout === "b"} disabled={loading || saving} onclick={() => save({ panel: { layout: "b" } }, "已使用极简列表")}>
                <span class="layout-check">{preferences.layout === "b" ? "✓" : ""}</span>
                <svg class="mini" viewBox="0 0 180 62" aria-hidden="true"><path d="M0 19h180" stroke="currentColor" stroke-width="3"/><path d="M12 9h130M12 30h150M12 42h138M12 54h120" stroke="currentColor" fill="none"/><path d="M4 25v34" stroke="currentColor" stroke-width="3"/></svg>
                <strong>极简列表</strong><small>大搜索行、排印列表与底部预览抽屉。</small>
              </button>
              <button class="layout-option" type="button" role="radio" aria-checked={preferences.layout === "c"} disabled={loading || saving} onclick={() => save({ panel: { layout: "c" } }, "已使用卡片流")}>
                <span class="layout-check">{preferences.layout === "c" ? "✓" : ""}</span>
                <svg class="mini" viewBox="0 0 180 62" aria-hidden="true"><path d="M0 15h180M0 51h180" stroke="currentColor" fill="none"/><path d="M18 17h34v34H18zM128 17h34v34h-34z" fill="currentColor" opacity=".16"/><path d="M66 7h48v48H66z" fill="currentColor" opacity=".28" stroke="currentColor" stroke-width="2"/></svg>
                <strong>卡片流</strong><small>视觉缩略卡横向移动，Space 查看完整细节。</small>
              </button>
            </div>
          </div>

          <HotkeyEditor {hotkey} disabled={loading || saving} onsave={save} />

          <div class="group"><span class="group-title">外观与反馈</span><div class="card">
            <label class="setting"><span class="setting-copy"><strong>主题</strong><small>跟随系统，或独立固定为浅色 / 深色；切换布局不会覆盖它。</small></span><select value={preferences.theme} disabled={loading || saving} onchange={(event) => save({ theme: event.currentTarget.value as ThemePreference }, "主题已保存并应用")}><option value="system">跟随系统</option><option value="light">浅色</option><option value="dark">深色</option></select></label>
            <label class="setting"><span class="setting-copy"><strong>动效强度</strong><small>系统“减少动态效果”始终优先；关闭后直接落位。</small></span><select value={preferences.motion} disabled={loading || saving} onchange={(event) => save({ feedback: { motion: event.currentTarget.value as MotionPreference } }, "动效强度已保存并应用")}><option value="soft">柔和</option><option value="spring">弹簧</option><option value="none">关闭</option></select></label>
            {#if reducedMotion && preferences.motion !== "none"}
              <p class="motion-warning" role="status">系统当前开启了“减少动画效果”，动效设置未生效。如需动画：Windows 设置 → 辅助功能 → 视觉效果 → 打开“动画效果”。</p>
            {/if}
            <label class="setting"><span class="setting-copy"><strong>列表密度</strong><small>v0.1 提供紧凑密度；舒适档将在后续版本开放。</small></span><select value={preferences.density} disabled={loading || saving} onchange={(event) => save({ panel: { density: event.currentTarget.value as DensityPreference } }, "列表密度已保存")}><option value="compact">紧凑</option><option value="comfortable" disabled>舒适（后续版本）</option></select></label>
          </div></div>
        </section>
      {:else if page === "record"}
        <CapturePage {capture} disabled={loading || saving} onsave={save} />
      {:else if page === "storage"}
        <StoragePage maxMb={snapshotMaxMb} disabled={loading || saving} onsave={save} bind:view={storageView} />
      {:else}
        <section class="page placeholder">
          <h2>{pages.find((entry) => entry.id === page)?.label}</h2>
          <p>此分区将在后续版本开放。面板布局、呼出快捷键、主题、动效与紧凑密度位于“通用”，不展示未接线的假控件。</p>
        </section>
      {/if}
    </div>
    <footer class:failed role="status" aria-live="polite">{status}{saving ? " · 请稍候" : ""}</footer>
  </main>
</section>

<style>
  .settings { width: 100%; height: 100%; display: grid; grid-template-columns: 190px minmax(0, 1fr); overflow: hidden; border: 1px solid var(--settings-line); border-radius: var(--radius-md); background: var(--settings-bg); color: var(--settings-text); }
  .nav { display: flex; flex-direction: column; padding: 18px 10px 14px; border-right: 1px solid var(--settings-line); background: var(--settings-nav); }
  .nav-brand { display: flex; align-items: flex-start; gap: 10px; margin-bottom: 12px; padding: 0 9px 20px; border-bottom: 1px solid var(--settings-line); }
  .nav-mark { width: 30px; height: 30px; display: grid; place-items: center; flex: none; border: 1px solid var(--settings-accent); color: var(--settings-accent); font: 600 14px var(--font-mono); }
  .nav-brand > span:last-child { min-width: 0; display: grid; gap: 3px; }
  .nav-brand strong { font: 600 15px var(--font-display); letter-spacing: .08em; }
  .nav-brand small { color: var(--settings-muted); font-size: 8px; white-space: nowrap; }
  .nav-brand em { color: var(--settings-muted); font: 8px var(--font-mono); letter-spacing: .06em; }
  .nav > button { height: 42px; flex: none; padding: 0 12px; border: 0; border-left: 3px solid transparent; background: transparent; color: var(--settings-muted); text-align: left; font-size: 12px; }
  .nav > button:hover { background: var(--settings-selected); }
  .nav > button.active { border-left-color: var(--settings-accent); background: var(--settings-selected); color: var(--settings-text); font-weight: 600; }
  .nav-foot { margin-top: auto; padding: 13px 12px 0; border-top: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 10px; line-height: 1.6; }
  .main { min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .main-head { height: 70px; flex: none; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 0 28px; border-bottom: 1px solid var(--settings-line); }
  .main-head h1 { margin: 0; font: 600 21px var(--font-display); }
  .main-head > span { color: var(--settings-muted); font: 9px var(--font-mono); letter-spacing: .08em; }
  .content { flex: 1; min-height: 0; overflow: auto; padding: 24px 28px 30px; }
  .layout-options { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; }
  .layout-option { position: relative; display: grid; align-content: start; gap: 9px; padding: 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: transparent; color: var(--settings-text); text-align: left; }
  .layout-option[aria-checked="true"] { border-color: var(--settings-accent); background: var(--settings-selected); box-shadow: inset 3px 0 0 var(--settings-accent); }
  .layout-option strong { font-size: 12px; }
  .layout-option small { color: var(--settings-muted); font-size: 10px; line-height: 1.5; }
  .layout-check { position: absolute; right: 9px; top: 8px; color: var(--settings-accent); font: 11px var(--font-mono); }
  .mini { width: 100%; height: 62px; border: 1px solid currentColor; color: var(--settings-accent); background: var(--a-preview); }
  footer { min-height: 34px; flex: none; padding: 9px 28px; border-top: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 10px; }
  footer.failed { color: var(--state-critical); }
  .placeholder { color: var(--settings-muted); font-size: 12px; line-height: 1.8; }
  .placeholder h2 { color: var(--settings-text); }
  .placeholder p { max-width: 480px; margin: 0; }
  @media (max-width: 720px) {
    .settings { grid-template-columns: 150px minmax(0, 1fr); }
    .layout-options { grid-template-columns: 1fr; }
    .main-head, .content, footer { padding-left: 18px; padding-right: 18px; }
    .setting { align-items: flex-start; flex-direction: column; gap: 9px; }
    select { width: 100%; }
  }
</style>
