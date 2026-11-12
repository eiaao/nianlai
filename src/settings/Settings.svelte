<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import * as ipc from "../lib/ipc";
  import HotkeyEditor from "./HotkeyEditor.svelte";
  import CapturePage from "./CapturePage.svelte";
  import StoragePage from "./StoragePage.svelte";
  import InteractionPage from "./InteractionPage.svelte";
  import OrganizePage from "./OrganizePage.svelte";
  import type { AppSettings, CaptureSettings, InteractionSettings, SettingsUpdate } from "../lib/ipc";
  import {
    applyPreferences, DEFAULT_PREFERENCES, normalizePreferences,
    type MotionPreference, type Preferences, type ThemePreference,
  } from "../lib/preferences";

  const pages = [
    { id: "general", label: "通用" },
    { id: "record", label: "隐私与记录" },
    { id: "storage", label: "数据与备份" },
    { id: "organize", label: "整理" },
    { id: "interaction", label: "交互" },
    { id: "about", label: "关于" },
  ];
  let page = $state("general");
  // “数据与备份”的子页面；面板里的回收站入口打开设置时带 #trash 直接跳过来。
  let storageView = $state<"main" | "trash" | "snapshots" | "images" | "export" | "import">("main");

  function navigate(target: string) {
    if (target === "trash" || target === "snapshots" || target === "images" || target === "export" || target === "import") {
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
  let interaction = $state<InteractionSettings | null>(null);
  let reuseToTop = $state(true);
  let loading = $state(true);
  let saving = $state(false);
  let status = $state("正在读取设置…");
  let failed = $state(false);
  // 系统当前是否开了“减少动画效果”：只用来在“跟随系统”开关旁说明现在的状态。
  let systemReduced = $state(false);

  onMount(() => {
    const media = matchMedia("(prefers-reduced-motion: reduce)");
    systemReduced = media.matches;
    const onMedia = () => { systemReduced = media.matches; };
    media.addEventListener("change", onMedia);
    return () => media.removeEventListener("change", onMedia);
  });

  function apply(settingsValue: AppSettings) {
    hotkey = settingsValue.hotkey ?? "Ctrl+`";
    capture = (settingsValue.capture as CaptureSettings | undefined) ?? null;
    snapshotMaxMb = settingsValue.snapshots?.max_mb ?? 1024;
    interaction = (settingsValue.interaction as InteractionSettings | undefined) ?? null;
    reuseToTop = settingsValue.history?.reuse_to_top !== false;
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
    <div class="nav-brand"><span class="nav-mark">N</span><span><strong>拈来</strong><small>取历史之一缕，付当前之一键</small><em>本机设置</em></span></div>
    {#each pages as entry}
      <button type="button" class:active={page === entry.id} aria-current={page === entry.id ? "page" : undefined} onclick={() => navigate(entry.id)}>{entry.label}</button>
    {/each}
    <div class="nav-foot">本地设置<br>外观自动保存 · 快捷键确认保存</div>
  </nav>

  <main class="main">
    <header class="main-head"><h1>{pages.find((entry) => entry.id === page)?.label}</h1><span>本地记录 · 仅此设备</span></header>
    <div class="content">
      {#if page === "general"}
        <section class="page">
          <div><h2>面板与呼出</h2><p class="page-intro">面板使用手牌布局；主题和动效独立保存。</p></div>
          <HotkeyEditor {hotkey} disabled={loading || saving} onsave={save} />

          <div class="group"><span class="group-title">外观与反馈</span><div class="card">
            <label class="setting"><span class="setting-copy"><strong>主题</strong><small>跟随系统，或独立固定为浅色 / 深色；只改变配色，不改变手牌操作。</small></span><select value={preferences.theme} disabled={loading || saving} onchange={(event) => save({ theme: event.currentTarget.value as ThemePreference }, "主题已保存并应用")}><option value="system">跟随系统</option><option value="light">浅色</option><option value="dark">深色</option></select></label>
            <label class="setting"><span class="setting-copy"><strong>动效强度</strong><small>默认“弹簧”，动画最完整；“关闭”后直接落位。</small></span><select value={preferences.motion} disabled={loading || saving} onchange={(event) => save({ feedback: { motion: event.currentTarget.value as MotionPreference } }, "动效强度已保存并应用")}><option value="spring">弹簧</option><option value="soft">柔和</option><option value="none">关闭</option></select></label>
            <label class="setting"><span class="setting-copy"><strong>跟随系统的“减少动画效果”</strong><small>打开后，Windows 关掉动画效果时（例如远程桌面连接）拈来也只做简单淡出。{systemReduced ? "系统现在关着动画效果。" : ""}</small></span><input class="switch" type="checkbox" role="switch" checked={preferences.followSystemMotion} disabled={loading || saving} onchange={(event) => save({ feedback: { follow_system: event.currentTarget.checked } }, event.currentTarget.checked ? "已改为跟随系统的动画设置" : "已改为始终播放动画")}></label>
          </div></div>
        </section>
      {:else if page === "record"}
        <CapturePage {capture} disabled={loading || saving} onsave={save} />
      {:else if page === "storage"}
        <StoragePage maxMb={snapshotMaxMb} disabled={loading || saving} onsave={save} bind:view={storageView} />
      {:else if page === "organize"}
        <OrganizePage {reuseToTop} disabled={loading || saving} onsave={save} />
      {:else if page === "interaction"}
        <InteractionPage {interaction} disabled={loading || saving} onsave={save} />
      {:else}
        <section class="page placeholder">
          <h2>{pages.find((entry) => entry.id === page)?.label}</h2>
          <p>此分区将在后续版本开放。</p>
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
  .main-head h1 { margin: 0; font: 600 20px var(--font-ui); }
  .main-head > span { color: var(--settings-muted); font: 9px var(--font-mono); letter-spacing: .08em; }
  .content { flex: 1; min-height: 0; overflow: auto; padding: 24px 28px 30px; }
  footer { min-height: 34px; flex: none; padding: 9px 28px; border-top: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 10px; }
  footer.failed { color: var(--state-critical); }
  .placeholder { color: var(--settings-muted); font-size: 12px; line-height: 1.8; }
  .placeholder h2 { color: var(--settings-text); }
  .placeholder p { max-width: 480px; margin: 0; }
  @media (max-width: 720px) {
    .settings { grid-template-columns: 150px minmax(0, 1fr); }
    .main-head, .content, footer { padding-left: 18px; padding-right: 18px; }
    .setting { align-items: flex-start; flex-direction: column; gap: 9px; }
    select { width: 100%; }
  }
</style>
