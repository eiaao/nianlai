<script lang="ts">
  import { onMount, tick } from "svelte";
  import * as ipc from "../lib/ipc";
  import { isMac } from "../lib/platform";
  import type { AppSettings, GuideSettings, LicenseNotices, SettingsUpdate } from "../lib/ipc";
  import appIcon from "../../src-tauri/icons/128x128.png?url";
  import { message } from "./format";
  import SaveMark, { type SaveMarkState } from "./SaveMark.svelte";

  const IDLE: SaveMarkState = { phase: "idle", message: "", error: "" };
  let { guide, disabled = false, onsave, saveState = () => IDLE, openRecovery, onopenguide, dealPending, ondeal }: {
    guide: GuideSettings;
    disabled?: boolean;
    onsave: (key: string, update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
    saveState?: (key: string) => SaveMarkState;
    openRecovery: { pending: boolean } | null;
    onopenguide: () => Promise<boolean>;
    dealPending: boolean;
    ondeal: () => Promise<string | null>;
  } = $props();

  let version = $state("正在读取版本…");
  let dataDir = $state("正在读取目录…");
  let notices = $state<LicenseNotices | null>(null);
  let noticesError = $state("");
  let directoryError = $state("");
  let openingDirectory = $state(false);
  let licenseOpen = $state(false);
  let licenseDialog = $state<HTMLDialogElement>();
  let licenseButton = $state<HTMLButtonElement>();
  let actionPending = $state<"replay" | "deal" | "open" | null>(null);
  let actionNotice = $state("");
  let accessibility = $state("正在读取权限…");
  const inFirstRun = $derived(guide.phase === "await_copy" || guide.phase === "await_show" || guide.phase === "await_paste");
  const actionDisabled = $derived(disabled || inFirstRun || dealPending || actionPending !== null || saveState("about.guide").phase === "saving");

  onMount(() => {
    let alive = true;
    const refreshAccessibility = () => {
      void ipc.macAccessibilityStatus()
        .then((enabled) => { if (alive) accessibility = enabled ? "已打开，可以直接贴进去" : "还没打开，只能先复制"; })
        .catch(() => { if (alive) accessibility = "权限没读到，请重新打开这页"; });
    };
    if (isMac) { refreshAccessibility(); window.addEventListener("focus", refreshAccessibility); }
    void ipc.appVersion()
      .then((value) => { if (alive) version = value; })
      .catch(() => { if (alive) version = "版本没读到，重新打开这页试试"; });
    void ipc.getDataDir()
      .then((value) => { if (alive) dataDir = value; })
      .catch(() => { if (alive) dataDir = "目录没读到，重新打开这页试试"; });
    void loadNotices();
    return () => { alive = false; if (isMac) window.removeEventListener("focus", refreshAccessibility); };
  });

  async function loadNotices() {
    try {
      const value = await ipc.getLicenseNotices();
      notices = value;
      noticesError = "";
    } catch (value) {
      noticesError = `许可没读到：${message(value)}`;
    }
  }

  async function openDirectory() {
    if (openingDirectory || dataDir.startsWith("正在") || dataDir.startsWith("目录没读到")) return;
    openingDirectory = true;
    directoryError = "";
    try { await ipc.openDataDir(); }
    catch (value) { directoryError = `文件夹没打开：${message(value)}`; }
    finally { openingDirectory = false; }
  }

  async function guideAction(action: "replay" | "deal") {
    if (actionDisabled) return;
    actionPending = action;
    actionNotice = "";
    try {
      if (action === "deal") {
        const notice = await ondeal();
        if (notice) actionNotice = notice;
        return;
      }
      const saved = await onsave("about.guide", { guide: { version: 1, phase: "await_copy", baseline_ms: null, baseline_total: 0, sample_id: null, cards_dealt: false } }, "首次启动设置已保存");
      if (!saved) return;
      // 已保存的 await_copy 不回滚；父层保留 open 失败恢复，页面重挂后仍能只重试开窗。
      if (await onopenguide()) actionNotice = "首次启动已重新打开";
    } finally {
      actionPending = null;
    }
  }

  async function retryGuideOpen() {
    if (disabled || actionPending || openRecovery?.pending) return;
    actionPending = "open";
    actionNotice = "";
    try {
      if (await onopenguide()) actionNotice = "首次启动已重新打开";
    } finally {
      actionPending = null;
    }
  }

  async function viewLicenses() {
    licenseOpen = true;
    await tick();
    licenseDialog?.showModal();
  }
  function closeLicenses() { licenseDialog?.close(); }
  async function closedLicenses() {
    licenseOpen = false;
    await tick();
    licenseButton?.focus();
  }
</script>

<section class="page about-page">
  <div class="identity">
    <img src={appIcon} alt="拈来" width="72" height="72" />
    <div class="wordmark">拈来</div>
    <div class="pinyin">niān lái</div>
    <p class="slogan">复制过的，信手拈来</p>
    <p class="gloss"><strong>拈</strong><span>用两三个指头，轻轻拿起来。</span></p>
  </div>

  <div class="card about-rows">
    {#if isMac}
      <div class="setting">
        <span class="setting-copy"><strong>辅助功能</strong><small>{accessibility}</small></span>
        <button type="button" onclick={() => { void ipc.macOpenAccessibilitySettings().catch(() => { accessibility = "系统设置没打开，请手动打开辅助功能设置"; }); }}>去设置</button>
      </div>
    {/if}
    <div class="setting">
      <span class="setting-copy"><strong>版本 <span data-about-version>{version}</span></strong><small>东西都存在这台电脑上，不联网。</small></span>
    </div>
    <div class="setting directory-row">
      <span class="setting-copy"><strong>东西存在哪</strong><small class="path" data-about-dir>{dataDir}</small>{#if directoryError}<small class="error" role="alert">{directoryError}</small>{/if}</span>
      <button type="button" disabled={openingDirectory || dataDir.startsWith("正在") || dataDir.startsWith("目录没读到")} onclick={openDirectory}>{openingDirectory ? "正在打开…" : "打开文件夹"}</button>
    </div>
    <div class="setting">
      <span class="setting-copy"><strong>再看一次首次启动</strong><small>{inFirstRun ? "首次启动结束后可以再看一次或再发新手牌。" : "从复制一句话开始，重新走一遍首次启动。新手牌的丢弃记录会保留。"}</small></span>
      <button type="button" data-guide-action="replay" disabled={actionDisabled} onclick={() => { void guideAction("replay"); }}>{actionPending === "replay" ? "正在保存…" : "再看一次"}</button>
    </div>
    <div class="setting">
      <span class="setting-copy"><strong>再发一遍新手牌</strong><small>{inFirstRun ? "首次启动结束后可以再看一次或再发新手牌。" : "保留当前首次启动状态，把四张新手牌重新放进最近。"}</small></span>
      <button type="button" data-guide-action="deal" disabled={actionDisabled} onclick={() => { void guideAction("deal"); }}>{actionPending === "deal" ? "正在保存…" : "再发一遍"}</button>
    </div>
    {#if openRecovery}
      <div class="setting" data-guide-open-recovery>
        <span class="setting-copy"><strong>首次启动窗口没有打开</strong><small class="error" role="alert">设置已保存，但窗口没打开，请再试一次</small></span>
        <button type="button" data-guide-action="open" disabled={disabled || actionPending !== null || openRecovery.pending} onclick={() => { void retryGuideOpen(); }}>{openRecovery.pending || actionPending === "open" ? "正在打开…" : "重新打开窗口"}</button>
      </div>
    {/if}
    <div class="guide-feedback" data-save-row="about.guide">
      <SaveMark state={saveState("about.guide")} />
      {#if actionNotice}<p class="notice" role="status">{actionNotice}</p>{/if}
    </div>
    <div class="setting">
      <span class="setting-copy"><strong>开源许可</strong><small>拈来 MIT 全文和随包第三方许可说明，都在本机查看。</small></span>
      <button bind:this={licenseButton} type="button" class="link" onclick={viewLicenses}>查看许可</button>
    </div>
  </div>
</section>

{#if licenseOpen}
  <dialog bind:this={licenseDialog} data-license-dialog aria-labelledby="license-title"
    onclose={closedLicenses}
    oncancel={(event) => { event.preventDefault(); closeLicenses(); }}
    onkeydown={(event) => { if (event.key === "Escape") { event.preventDefault(); closeLicenses(); } }}>
    <div class="license-head"><h2 id="license-title">开源许可</h2><button type="button" onclick={closeLicenses}>关闭</button></div>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div class="license-scroll" role="region" aria-label="许可正文" tabindex="0" data-settings-scroll>
      {#if noticesError}
        <p class="error" role="alert">{noticesError}</p>
        <button type="button" onclick={() => { void loadNotices(); }}>重新读取</button>
      {:else if !notices}
        <p>正在读取本机许可…</p>
      {:else}
        <section><h3>拈来 MIT 许可</h3><pre>{notices.nianlai}</pre></section>
        <section><h3>随包第三方许可说明</h3><pre>{notices.third_party}</pre></section>
      {/if}
    </div>
  </dialog>
{/if}

<style>
  .about-page { gap: 20px; }
  .identity { display: grid; justify-items: center; gap: 4px; padding: 8px 0 0; text-align: center; }
  .identity img { width: 72px; height: 72px; margin-bottom: 4px; }
  .wordmark { font: 600 20px var(--font-display); letter-spacing: .08em; }
  .pinyin { color: var(--hand-ink-2); font-size: 12px; }
  .slogan { margin: 4px 0 0; font-size: 15px; }
  .gloss { display: flex; align-items: center; gap: 8px; margin: 4px 0 0; color: var(--hand-ink-2); font-size: 12px; }
  .gloss strong { width: 28px; height: 28px; display: grid; place-items: center; border-radius: 4px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 20px var(--font-display); }
  .about-rows { overflow: hidden; }
  .setting { min-height: 64px; }
  .setting strong span { font-weight: 400; }
  .path { font-family: var(--font-mono); overflow-wrap: anywhere; user-select: text; }
  button { min-height: 32px; flex: none; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); color: var(--hand-ink); font-size: 12px; }
  button.link { border-color: transparent; background: transparent; color: var(--hand-seal); text-decoration: underline; }
  button:disabled { opacity: .6; }
  .guide-feedback { padding: 0 16px; }
  .guide-feedback p { margin: 8px 0 12px; font-size: 12px; line-height: 1.6; }
  .error { color: var(--state-critical); }
  .notice { color: var(--hand-ink-2); }
  dialog { width: min(760px, calc(100vw - 64px)); height: min(600px, calc(100vh - 64px)); padding: 0; overflow: hidden; border: 1px solid var(--hand-line-strong); border-radius: 8px; background: var(--hand-paper); color: var(--hand-ink); box-shadow: var(--hand-lift); }
  dialog::backdrop { background: rgba(0, 0, 0, .42); }
  .license-head { height: 56px; display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 0 20px; border-bottom: 1px solid var(--hand-line); }
  .license-head h2 { margin: 0; }
  .license-scroll { height: calc(100% - 56px); overflow: auto; padding: 20px; }
  .license-scroll section + section { margin-top: 24px; padding-top: 20px; border-top: 1px solid var(--hand-line); }
  h3 { margin: 0 0 12px; font-size: 15px; }
  pre { margin: 0; color: var(--hand-ink); font: 12px/1.65 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; }
  @media (max-width: 720px) { .directory-row { align-items: flex-start; flex-direction: column; } }
</style>
