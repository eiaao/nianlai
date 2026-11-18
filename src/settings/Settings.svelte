<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import * as ipc from "../lib/ipc";
  import GeneralPage from "./GeneralPage.svelte";
  import CapturePage from "./CapturePage.svelte";
  import StoragePage from "./StoragePage.svelte";
  import InteractionPage from "./InteractionPage.svelte";
  import OrganizePage from "./OrganizePage.svelte";
  import AboutPage from "./AboutPage.svelte";
  import SaveMark, { type SaveMarkState } from "./SaveMark.svelte";
  import { navigate, type SettingsPageId, type StorageView } from "../lib/settingsNavigation";
  import type { AppSettings, CaptureSettings, GuideSettings, InteractionSettings, SettingsUpdate } from "../lib/ipc";
  import { defaultGuideSettings } from "../lib/guideCards";
  import {
    applyPreferences, DEFAULT_PREFERENCES, normalizePreferences,
    type Preferences,
  } from "../lib/preferences";

  const pages: { id: SettingsPageId; label: string; icon: string }[] = [
    { id: "general", label: "通用", icon: '<path d="M3.5 6h8M15.5 6h1M3.5 14h1M8.5 14h8"/><circle cx="13.5" cy="6" r="2"/><circle cx="6.5" cy="14" r="2"/>' },
    { id: "fav", label: "收藏", icon: '<rect x="4.75" y="2.75" width="10.5" height="14.5" rx="2"/><path d="M8 6.5h4v4H8z"/>' },
    { id: "privacy", label: "记录与隐私", icon: '<path d="M10 2.75l6 2.4v4.4c0 3.6-2.6 6.3-6 7.2-3.4-.9-6-3.6-6-7.2v-4.4z"/><path d="M7.5 10l1.8 1.8 3.2-3.4"/>' },
    { id: "data", label: "数据与备份", icon: '<path d="M2.75 7L10 3.25 17.25 7 10 10.75z"/><path d="M2.75 10.5L10 14.25l7.25-3.75M2.75 14L10 17.75 17.25 14"/>' },
    { id: "habits", label: "操作习惯", icon: '<rect x="5.75" y="2.75" width="8.5" height="14.5" rx="4.25"/><path d="M10 2.75V8"/>' },
    { id: "about", label: "关于", icon: '<circle cx="10" cy="10" r="7.25"/><path d="M10 9v5M10 6.3v.2"/>' },
  ];
  const IDLE_MARK: SaveMarkState = { phase: "idle", message: "", error: "" };
  let page = $state<SettingsPageId>("general");
  let recordHotkey = $state(false);
  let hotkeyRequest = $state<string>();
  // 面板里的回收站入口仍通过 #trash 和 settings:navigate 到达原组件。
  let storageView = $state<StorageView>("main");

  function show(target: string) {
    const next = navigate(target);
    page = next.page;
    storageView = next.storageView;
    recordHotkey = next.recordHotkey === true;
    hotkeyRequest = next.hotkeyRequest;
  }

  onMount(() => {
    show(location.hash.slice(1));
    let stop: (() => void) | undefined;
    let disposed = false;
    void listen<string>("settings:navigate", (event) => show(event.payload)).then((unlisten) => {
      if (disposed) unlisten(); else stop = unlisten;
    });
    return () => { disposed = true; stop?.(); };
  });

  // 新版本状态：后端 update 模块是唯一来源，设置窗口只读它并响应 update:changed。
  let update = $state<ipc.UpdateStatus | null>(null);
  onMount(() => {
    let disposed = false;
    let stop: ipc.UnlistenFn | undefined;
    void ipc.getUpdateStatus().then((value) => { if (!disposed) update = value; }).catch(() => {});
    void ipc.onUpdateChanged((value) => { if (!disposed) update = value; })
      .then((unlisten) => { if (disposed) unlisten(); else stop = unlisten; }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  });
  const showBanner = $derived(page !== "about" && !!update?.show_banner && !!update.latest_version);
  const bannerVersion = $derived(update?.latest_version ?? "");
  function dismissBanner() { void ipc.dismissUpdateBanner().catch(() => {}); }

  let preferences = $state<Preferences>({ ...DEFAULT_PREFERENCES });
  let hotkey = $state("Ctrl+`");
  let capture = $state<CaptureSettings | null>(null);
  let snapshotMaxMb = $state(1024);
  let interaction = $state<InteractionSettings | null>(null);
  let guide = $state<GuideSettings>(defaultGuideSettings());
  // open 已成功保存 await_copy 后的失败恢复属于设置窗口，而不是会被页面切换销毁的 About 局部状态。
  let aboutGuideOpen = $state<{ pending: boolean } | null>(null);
  async function openAboutGuide(): Promise<boolean> {
    if (aboutGuideOpen?.pending) return false;
    aboutGuideOpen = { pending: true };
    try {
      await ipc.openGuide();
      aboutGuideOpen = null;
      return true;
    } catch {
      aboutGuideOpen = { pending: false };
      return false;
    }
  }
  // deal 从保存到定向通知完成只有这一把父层锁；About 重挂不会释放它，旧操作结束前也不能启动新操作。
  let aboutGuideDealPending = $state(false);
  async function dealAboutGuide(): Promise<string | null> {
    if (aboutGuideDealPending) return null;
    aboutGuideDealPending = true;
    try {
      const saved = await save("about.guide", { guide: { cards_dealt: true, dismissed: [] } }, "新手牌设置已保存");
      if (!saved) return null;
      try {
        await ipc.notifyGuideCardsDealt();
        return "新手牌已重新发出";
      } catch {
        // 设置确已保存；通知失败只影响当前可见面板，下一次 shown 会从持久 guide 恢复。
        return "新手牌已保存，下次呼出会看到";
      }
    } finally {
      aboutGuideDealPending = false;
    }
  }
  let reuseToTop = $state(true);
  let loading = $state(true);
  let loadError = $state("");
  let systemReduced = $state(false);
  let saveStates = $state<Record<string, SaveMarkState>>({});

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
    if (settingsValue.guide) guide = settingsValue.guide;
    reuseToTop = settingsValue.history?.reuse_to_top !== false;
    preferences = normalizePreferences(settingsValue);
    applyPreferences(preferences);
  }

  type SaveOperation = {
    id: number;
    key: string;
    update: SettingsUpdate;
    message: string;
    status: "pending" | "success" | "failed";
  };
  type UnknownRecord = Record<string, unknown>;
  const isRecord = (value: unknown): value is UnknownRecord => !!value && typeof value === "object" && !Array.isArray(value);

  // 与 Rust settings::merge 一致：对象逐字段合并，数组和标量整体替换。
  function mergeValue(base: unknown, update: unknown): unknown {
    if (isRecord(base) && isRecord(update)) {
      const merged = { ...base };
      for (const [key, value] of Object.entries(update)) merged[key] = mergeValue(merged[key], value);
      return merged;
    }
    return update;
  }
  const mergeSettings = (base: AppSettings, update: SettingsUpdate) => mergeValue(base, update) as AppSettings;

  // 跨行响应可能乱序：最新响应只是基底，本轮所有成功/等待中的局部更新按调用顺序叠回。
  // 全部结束后把最终视图压成一个快照，操作历史不会随窗口打开时间无限增长。
  let baseSettings: AppSettings = {};
  let operations: SaveOperation[] = [];
  let nextOperation = 0;
  const latestByKey = new Map<string, number>();
  const queues = new Map<string, Promise<void>>();
  const markTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function recomputeSettings() {
    let view = baseSettings;
    for (const operation of operations) {
      if (operation.status !== "failed") view = mergeSettings(view, operation.update);
    }
    if (!operations.some((operation) => operation.status === "pending")) {
      baseSettings = view;
      operations = [];
    }
    apply(view);
  }

  function row(key: string): SaveMarkState { return saveStates[key] ?? IDLE_MARK; }
  function clearMarkTimer(key: string) {
    const timer = markTimers.get(key);
    if (timer !== undefined) clearTimeout(timer);
    markTimers.delete(key);
  }
  function showSaved(operation: SaveOperation) {
    clearMarkTimer(operation.key);
    saveStates[operation.key] = { phase: "saved", message: operation.message, error: "" };
    markTimers.set(operation.key, setTimeout(() => {
      // 新调用会先清旧计时器；再核对标记，避免旧回调清掉新一轮状态。
      if (latestByKey.get(operation.key) === operation.id && saveStates[operation.key]?.phase === "saved") {
        saveStates[operation.key] = IDLE_MARK;
      }
      markTimers.delete(operation.key);
    }, 1500));
  }
  const errorMessage = (error: unknown) => error instanceof Error ? error.message : typeof error === "string" ? error : "没保存上，还是原来的设置";

  async function performSave(operation: SaveOperation): Promise<AppSettings | null> {
    try {
      const saved = await ipc.setSettings(operation.update);
      operation.status = "success";
      baseSettings = saved;
      recomputeSettings();
      if (latestByKey.get(operation.key) === operation.id) showSaved(operation);
      return saved;
    } catch (error) {
      operation.status = "failed";
      recomputeSettings();
      if (latestByKey.get(operation.key) === operation.id) {
        clearMarkTimer(operation.key);
        saveStates[operation.key] = { phase: "failed", message: operation.message, error: errorMessage(error) };
      }
      return null;
    }
  }

  export function save(key: string, update: SettingsUpdate, message: string): Promise<AppSettings | null> {
    if (loading) return Promise.resolve(null);
    const operation: SaveOperation = { id: ++nextOperation, key, update, message, status: "pending" };
    operations.push(operation);
    latestByKey.set(key, operation.id);
    clearMarkTimer(key);
    saveStates[key] = { phase: "saving", message, error: "" };
    recomputeSettings();

    // 同一行按调用顺序串行；不同 key 不共享 saving 锁，可以独立进行。
    const previous = queues.get(key) ?? Promise.resolve();
    const result = previous.then(() => performSave(operation));
    const queued = result.then(() => undefined);
    queues.set(key, queued);
    void queued.then(() => { if (queues.get(key) === queued) queues.delete(key); });
    return result;
  }

  const saveCapture = (update: SettingsUpdate, message: string) => save("privacy", update, message);
  const saveStorage = (update: SettingsUpdate, message: string) => save("data", update, message);
  const saveInteraction = (key: string, update: SettingsUpdate, message: string) => save(key, update, message);

  onMount(async () => {
    try {
      baseSettings = await ipc.getSettings();
      recomputeSettings();
      loadError = "";
    } catch {
      loadError = "没读到你的设置，先按默认的显示。关掉窗口再开试试";
    } finally {
      loading = false;
    }
  });
  onMount(() => {
    let disposed = false;
    let stop: ipc.UnlistenFn | undefined;
    void ipc.onSettingsChanged((settings) => {
      if (disposed) return;
      baseSettings = settings;
      recomputeSettings();
    }).then((unlisten) => {
      if (disposed) unlisten(); else stop = unlisten;
    }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  });
  onDestroy(() => { for (const key of markTimers.keys()) clearMarkTimer(key); });
</script>

<svelte:head><title>拈来 · 设置</title></svelte:head>

<section class="settings" aria-label="拈来设置">
  <nav class="nav" aria-label="设置分区">
    <div class="nav-brand"><span class="nav-mark">拈</span><span><strong>拈来</strong><small>复制过的，信手拈来</small></span></div>
    <div class="nav-pages">
      {#each pages as entry}
        <button type="button" data-page={entry.id} class:active={page === entry.id} aria-current={page === entry.id ? "page" : undefined} onclick={() => show(entry.id)}>
          <svg viewBox="0 0 20 20" aria-hidden="true">{@html entry.icon}</svg><span>{entry.label}</span>
          {#if entry.id === "about" && update?.show_dot}<i class="update-dot" data-update-dot role="img" aria-label="有新版本"></i>{/if}
        </button>
      {/each}
    </div>
    <div class="nav-foot">设置只存在这台电脑上</div>
  </nav>

  <main class="main" data-settings-page data-page={page} data-storage-view={storageView} data-settings-loading={loading ? "true" : "false"}>
    <header class="main-head">
      <h1>{pages.find((entry) => entry.id === page)?.label}</h1>
      {#if page === "privacy" || page === "data" || page === "fav"}<div data-save-row={page}><SaveMark state={row(page)} /></div>{/if}
    </header>
    {#if showBanner}
      <div class="update-banner" data-update-banner role="status">
        <span>发现新版本 {bannerVersion}</span>
        <button type="button" class="banner-link" onclick={() => show("about")}>查看</button>
        <button type="button" class="banner-close" aria-label="关闭提示" onclick={dismissBanner}>
          <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M5.5 5.5l9 9M14.5 5.5l-9 9"/></svg>
        </button>
      </div>
    {/if}
    {#if loading}<p class="load-status" role="status">正在读取设置…</p>{/if}
    {#if loadError}<p class="load-error" role="alert">{loadError}</p>{/if}
    <div class="content" data-settings-scroll>
      {#if page === "general"}
        <GeneralPage {hotkey} {interaction} {preferences} {systemReduced} {recordHotkey} {hotkeyRequest} onRecordHotkeyConsumed={() => { recordHotkey = false; hotkeyRequest = undefined; }} disabled={loading} onsave={save} saveState={row} />
      {:else if page === "privacy"}
        <CapturePage {capture} disabled={loading} onsave={saveCapture} />
      {:else if page === "data"}
        <StoragePage maxMb={snapshotMaxMb} disabled={loading} onsave={saveStorage} bind:view={storageView} />
      {:else if page === "fav"}
        <OrganizePage disabled={loading} />
      {:else if page === "habits"}
        <InteractionPage {interaction} {reuseToTop} disabled={loading} onsave={saveInteraction} saveState={row} />
      {:else}
        <AboutPage {guide} {update} disabled={loading} onsave={save} saveState={row} openRecovery={aboutGuideOpen} onopenguide={openAboutGuide} dealPending={aboutGuideDealPending} ondeal={dealAboutGuide} />
      {/if}
    </div>
  </main>
</section>

<style>
  .settings { width: 100%; height: 100%; display: grid; grid-template-columns: 184px minmax(0, 1fr); overflow: hidden; border: 1px solid var(--hand-line-strong); border-radius: 8px; background: var(--hand-paper); color: var(--hand-ink); }
  .nav { min-height: 0; display: flex; flex-direction: column; padding: 20px 12px 16px; border-right: 1px solid var(--hand-line); background: var(--hand-card-back); }
  .nav-brand { display: flex; align-items: center; gap: 12px; margin-bottom: 20px; padding: 0 8px 20px; border-bottom: 1px solid var(--hand-line); }
  .nav-mark { width: 32px; height: 32px; display: grid; place-items: center; flex: none; border-radius: 4px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 20px var(--font-display); }
  .nav-brand > span:last-child { min-width: 0; display: grid; gap: 4px; }
  .nav-brand strong { font: 600 20px var(--font-display); letter-spacing: .08em; }
  .nav-brand small { color: var(--hand-ink-2); font-size: 12px; white-space: nowrap; }
  .nav-pages { display: grid; gap: 8px; }
  .nav-pages > button { min-height: 44px; display: flex; align-items: center; gap: 12px; padding: 8px 12px; border: 1px solid transparent; border-radius: 4px; background: transparent; color: var(--hand-ink-2); text-align: left; font-size: 13px; box-shadow: none; }
  .nav-pages > button:hover { border-color: var(--hand-line); background: var(--hand-paper); }
  .nav-pages > button.active { border-color: var(--hand-line-strong); border-left: 4px solid var(--hand-seal); background: var(--hand-card); color: var(--hand-ink); font-weight: 600; box-shadow: var(--hand-row-shadow); }
  .nav-pages svg { width: 20px; height: 20px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; }
  .nav-pages > button { position: relative; }
  .update-dot { width: 8px; height: 8px; margin-left: auto; flex: none; border-radius: 50%; background: var(--state-success); }
  .update-banner { flex: none; min-height: 36px; display: flex; align-items: center; gap: 8px; padding: 6px 16px 6px 24px; border-bottom: 1px solid var(--hand-line); background: var(--state-success-bg); color: var(--state-success); font-size: 13px; }
  .update-banner > span { font-weight: 600; }
  .banner-link { min-height: 24px; padding: 0 4px; border: 0; background: transparent; color: inherit; font-size: 13px; text-decoration: underline; box-shadow: none; }
  .banner-close { width: 24px; height: 24px; margin-left: auto; padding: 0; display: grid; place-items: center; border: 0; border-radius: 4px; background: transparent; color: inherit; box-shadow: none; }
  .banner-close:hover { background: color-mix(in srgb, currentColor 12%, transparent); }
  .banner-close svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; }
  .nav-foot { margin-top: auto; padding: 16px 8px 0; border-top: 1px solid var(--hand-line); color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .main { min-width: 0; min-height: 0; display: flex; flex-direction: column; background: var(--hand-paper); }
  .main-head { min-height: 64px; flex: none; display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 0 24px; border-bottom: 1px solid var(--hand-line); }
  .main-head h1 { margin: 0; color: var(--hand-ink); font: 600 20px var(--font-ui); }
  .content { flex: 1; min-height: 0; overflow: auto; padding: 24px; }
  .load-status, .load-error { flex: none; margin: 0; padding: 12px 24px; border-bottom: 1px solid var(--hand-line); font-size: 13px; line-height: 1.6; }
  .load-status { color: var(--hand-ink-2); }
  .load-error { background: var(--state-critical-bg); color: var(--state-critical); }
  @media (max-width: 1000px) {
    .settings { grid-template-columns: 168px minmax(0, 1fr); }
    .content { padding: 20px; }
  }
  @media (max-width: 720px) {
    .main-head, .content, .load-status, .load-error { padding-left: 16px; padding-right: 16px; }
  }
  @media (forced-colors: active) {
    .nav-pages > button.active { outline: 2px solid Highlight; outline-offset: -2px; }
    .settings, .nav-pages > button { border-color: CanvasText; }
  }
</style>
