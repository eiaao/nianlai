<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type {
    AppSettings, InteractionSettings, Placement, SettingsUpdate,
  } from "../lib/ipc";
  import type { MotionPreference, Preferences, ThemePreference } from "../lib/preferences";
  import HotkeyEditor from "./HotkeyEditor.svelte";
  import SaveMark, { type SaveMarkState } from "./SaveMark.svelte";

  let { hotkey, interaction, preferences, systemReduced, disabled, onsave, saveState }: {
    hotkey: string;
    interaction: InteractionSettings | null;
    preferences: Preferences;
    systemReduced: boolean;
    disabled: boolean;
    onsave: (key: string, update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
    saveState: (key: string) => SaveMarkState;
  } = $props();

  const places: { id: Placement; label: string; hint: string }[] = [
    { id: "caret", label: "光标旁", hint: "找不到光标时，出现在鼠标旁" },
    { id: "mouse", label: "鼠标旁", hint: "跟随当前鼠标位置" },
    { id: "center", label: "屏幕中央", hint: "固定在当前屏幕中央" },
  ];
  const placement = $derived(interaction?.placement ?? "caret");
  const reduceMotion = $derived(preferences.motion === "none" || (preferences.followSystemMotion && systemReduced));
  const saveHotkey = (update: SettingsUpdate, message: string) => onsave("general.hotkey", update, message);
  const errorMessage = (value: unknown) => value instanceof Error ? value.message : typeof value === "string" ? value : "操作没有完成，再试一次";

  function displayHotkey(value: string) {
    const order = ["control", "ctrl", "alt", "shift", "super", "win"];
    const parts = (value.trim() || "Ctrl+`").split("+").filter(Boolean);
    return parts.sort((a, b) => {
      const ai = order.indexOf(a.toLowerCase());
      const bi = order.indexOf(b.toLowerCase());
      return (ai < 0 ? 99 : ai) - (bi < 0 ? 99 : bi);
    }).map((part) => part
      .replace(/^control$/i, "Ctrl").replace(/^super$/i, "Win")
      .replace(/^shift$/i, "Shift").replace(/^alt$/i, "Alt")
      .replace(/^Key([A-Z])$/, "$1").replace(/^Digit([0-9])$/, "$1")
      .replace(/^Backquote$/, "`"))
      .join(" + ");
  }

  // 预览只持有自己的演示次数。设置改变会重播，但不写存储，也不调用任何面板命令。
  let demoCount = $state(0);
  let settingsSignature = "";
  export function demonstrate() {
    if (!disabled) demoCount += 1;
  }
  $effect(() => {
    const signature = JSON.stringify([
      hotkey, placement, preferences.theme, preferences.motion,
      preferences.followSystemMotion, systemReduced,
    ]);
    if (settingsSignature && settingsSignature !== signature) demonstrate();
    settingsSignature = signature;
  });

  // autostart 不属于 settings.json：初次进入、重进组件和窗口聚焦都直接读插件真实状态。
  const IDLE: SaveMarkState = { phase: "idle", message: "", error: "" };
  let autostart = $state(false);
  let autostartReady = $state(false);
  let autostartBusy = $state(false);
  let autostartMark = $state<SaveMarkState>(IDLE);
  let autostartRequest = 0;
  let autostartRefreshPending = false;
  let autostartTimer: ReturnType<typeof setTimeout> | undefined;

  function clearAutostartTimer() {
    if (autostartTimer !== undefined) clearTimeout(autostartTimer);
    autostartTimer = undefined;
  }
  function autostartSaved(actual: boolean) {
    clearAutostartTimer();
    autostartMark = { phase: "saved", message: actual ? "开机启动已开启" : "开机启动已关闭", error: "" };
    autostartTimer = setTimeout(() => { autostartMark = IDLE; }, 1500);
  }
  async function refreshAutostart() {
    if (autostartBusy) {
      // 保存途中的聚焦只登记；set/catch 的旧回读结束后，必须再读一次最新插件状态。
      autostartRefreshPending = true;
      return;
    }
    const request = ++autostartRequest;
    clearAutostartTimer();
    try {
      const actual = await ipc.getAutostart();
      if (request !== autostartRequest) return;
      autostart = actual;
      autostartReady = true;
      autostartMark = IDLE;
    } catch (failure) {
      if (request !== autostartRequest) return;
      autostartReady = true;
      autostartMark = { phase: "failed", message: "", error: errorMessage(failure) };
    }
  }
  async function changeAutostart(event: Event) {
    const control = event.currentTarget as HTMLInputElement;
    const requested = control.checked;
    control.checked = autostart;
    if (disabled || autostartBusy) return;
    const request = ++autostartRequest;
    autostartBusy = true;
    clearAutostartTimer();
    autostartMark = { phase: "saving", message: "", error: "" };
    try {
      const actual = await ipc.setAutostart(requested);
      if (request !== autostartRequest) return;
      autostart = actual;
      autostartReady = true;
      autostartSaved(actual);
    } catch (failure) {
      // 命令失败不带来源不明的布尔值；再读一次，能读到就展示外部实际状态，错误标记仍保留。
      try {
        const actual = await ipc.getAutostart();
        if (request === autostartRequest) { autostart = actual; autostartReady = true; }
      } catch { /* 保留请求前的状态，绝不能按请求值勾选。 */ }
      if (request === autostartRequest) {
        autostartMark = { phase: "failed", message: "", error: errorMessage(failure) };
      }
    } finally {
      if (request === autostartRequest) {
        autostartBusy = false;
        if (autostartRefreshPending) {
          autostartRefreshPending = false;
          void refreshAutostart();
        }
      }
    }
  }
  onMount(() => {
    void refreshAutostart();
    const onFocus = () => { void refreshAutostart(); };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });
  onDestroy(() => { autostartRequest += 1; clearAutostartTimer(); });
</script>

<section class="page general-page" aria-label="通用设置">
  <div
    class="mini-preview"
    data-mini-preview
    data-placement={placement}
    data-theme={preferences.theme}
    data-motion={preferences.motion}
    data-reduced-motion={reduceMotion ? "true" : "false"}
    data-demo-count={demoCount}
    data-demo-state={demoCount ? "spread" : "stacked"}
    aria-label="面板位置与动效预览"
  >
    <div class="mini-screen" aria-hidden="true">
      <div class="mini-document"><i></i><i></i><i></i><i></i></div>
      {#if placement === "caret"}<span class="mini-caret"></span>{:else if placement === "mouse"}<span class="mini-mouse">↖</span>{/if}
      {#key demoCount}
        <div class:spread={demoCount > 0} class="mini-hand">
          {#each [0, 1, 2, 3, 4] as index}
            <span
              class="mini-card"
              class:selected={index === 0}
              style={`--x:${(index - 2) * 44}px;--y:${Math.abs(index - 2) * 4}px;--r:${(index - 2) * 3}deg;--delay:${index * 30}ms`}
            ><b>{index + 1}</b><i></i><i></i></span>
          {/each}
        </div>
      {/key}
    </div>
    <div class="preview-caption">
      <span>当前呼出键 <kbd>{displayHotkey(hotkey)}</kbd>。用按钮只在这里演示，不会打开真实面板。</span>
      <button type="button" data-action="demonstrate" disabled={disabled} onclick={demonstrate}>演示一次摊开</button>
    </div>
  </div>

  <div class="group">
    <span class="group-title">呼出与位置</span>
    <div data-setting="hotkey" data-save-row="general.hotkey">
      <HotkeyEditor {hotkey} {disabled} onsave={saveHotkey} saveState={saveState("general.hotkey")} />
    </div>
    <div class="card setting placement-row" data-save-row="general.placement">
      <span class="setting-copy"><strong>出现在哪</strong><small>找不到光标时，出现在鼠标旁。</small></span>
      <SaveMark state={saveState("general.placement")} />
      <div class="placement-options" data-setting="placement" role="radiogroup" aria-label="出现在哪">
        {#each places as place (place.id)}
          <button type="button" role="radio" data-value={place.id} aria-checked={placement === place.id} title={place.hint} disabled={disabled || !interaction}
            onclick={() => onsave("general.placement", { interaction: { placement: place.id } }, `呼出位置已改为${place.label}`)}>{place.label}</button>
        {/each}
      </div>
    </div>
  </div>

  <div class="group">
    <span class="group-title">外观与反馈</span>
    <div class="card">
      <label class="setting" data-save-row="general.theme"><span class="setting-copy"><strong>主题</strong><small>浅色、深色，或跟随系统。</small></span><SaveMark state={saveState("general.theme")} /><select data-setting="theme" aria-label="主题" value={preferences.theme} disabled={disabled} onchange={(event) => onsave("general.theme", { theme: event.currentTarget.value as ThemePreference }, "主题已保存并应用")}><option value="light">浅色</option><option value="dark">深色</option><option value="system">跟随系统</option></select></label>
      <label class="setting" data-save-row="general.motion"><span class="setting-copy"><strong>动效</strong><small>关闭后牌直接到位，没有飞的过程。</small></span><SaveMark state={saveState("general.motion")} /><select data-setting="motion" aria-label="动效" value={preferences.motion} disabled={disabled} onchange={(event) => onsave("general.motion", { feedback: { motion: event.currentTarget.value as MotionPreference } }, "动效已保存并应用")}><option value="spring">完整</option><option value="soft">柔和</option><option value="none">关闭</option></select></label>
      <label class="setting" data-save-row="general.follow-system"><span class="setting-copy"><strong>系统关了动画时，我也跟着简化</strong><small>比如远程桌面时。{systemReduced ? "系统现在关着动画效果。" : ""}</small></span><SaveMark state={saveState("general.follow-system")} /><input class="switch" data-setting="follow-system" aria-label="系统关了动画时，我也跟着简化" type="checkbox" role="switch" checked={preferences.followSystemMotion} disabled={disabled} onchange={(event) => onsave("general.follow-system", { feedback: { follow_system: event.currentTarget.checked } }, event.currentTarget.checked ? "已改为跟随系统的动画设置" : "已改为始终播放动画")}></label>
      <label class="setting" data-save-row="general.autostart"><span class="setting-copy"><strong>开机时启动</strong><small>和托盘里的“开机时启动”是同一个开关。</small></span><SaveMark state={autostartMark} /><input class="switch" data-setting="autostart" aria-label="开机时启动" type="checkbox" role="switch" checked={autostart} disabled={disabled || autostartBusy || !autostartReady} onchange={changeAutostart}></label>
    </div>
  </div>
</section>

<style>
  .mini-preview { overflow: hidden; border: 1px solid var(--hand-line-strong); border-radius: 8px; background: var(--hand-card-back); }
  .mini-screen { position: relative; height: 184px; overflow: hidden; background: var(--hand-paper); }
  .mini-document { position: absolute; inset: 20px 24px; border: 1px solid var(--hand-line); border-radius: 4px; background: var(--hand-card); }
  .mini-document i { position: absolute; left: 24px; height: 4px; border-radius: 2px; background: var(--hand-line-strong); }
  .mini-document i:nth-child(1) { top: 24px; width: 112px; }
  .mini-document i:nth-child(2) { top: 44px; width: 192px; }
  .mini-document i:nth-child(3) { top: 64px; width: 88px; }
  .mini-document i:nth-child(4) { top: 104px; width: 152px; opacity: .45; }
  .mini-caret { position: absolute; left: 39%; top: 56px; width: 2px; height: 20px; background: var(--hand-ink); }
  .mini-mouse { position: absolute; right: 20%; top: 40px; color: var(--hand-ink); font-size: 20px; }
  .mini-hand { position: absolute; left: 50%; top: 92px; width: 48px; height: 68px; transform: translate(-50%, -50%); }
  .mini-preview[data-placement="caret"] .mini-hand { left: 42%; top: 108px; }
  .mini-preview[data-placement="mouse"] .mini-hand { left: 74%; top: 96px; }
  .mini-preview[data-placement="center"] .mini-hand { left: 50%; top: 96px; }
  .mini-card { position: absolute; left: 0; top: 0; width: 48px; height: 68px; display: grid; align-content: start; gap: 8px; padding: 8px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); box-shadow: var(--hand-row-shadow); opacity: 1; transform: rotate(var(--r)); }
  .mini-hand:not(.spread) .mini-card.selected { z-index: 1; transform: translateY(-8px); }
  .mini-card.selected { border-color: var(--hand-seal); }
  .mini-card b { color: var(--hand-seal); font: 600 12px var(--font-mono); }
  .mini-card i { display: block; width: 100%; height: 4px; background: var(--hand-line-strong); }
  .mini-card i:last-child { width: 64%; }
  .mini-hand.spread .mini-card { opacity: 1; transform: translate(var(--x), var(--y)) rotate(var(--r)); animation: mini-spread 360ms cubic-bezier(.2, .9, .25, 1.2) var(--delay) both; }
  .mini-preview[data-motion="soft"] .mini-hand.spread .mini-card { animation-duration: 220ms; animation-timing-function: ease-out; }
  .mini-preview[data-reduced-motion="true"] .mini-hand.spread .mini-card { animation: none; opacity: 1; transform: translate(var(--x), var(--y)) rotate(var(--r)); }
  @keyframes mini-spread {
    from { opacity: 0; transform: translate(0, 28px) scale(.4); }
    to { opacity: 1; transform: translate(var(--x), var(--y)) rotate(var(--r)); }
  }
  .preview-caption { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 12px 16px; color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .preview-caption kbd { color: var(--hand-ink); font: 600 12px var(--font-mono); }
  .preview-caption button, .placement-options button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); color: var(--hand-ink); font-size: 12px; }
  .preview-caption button { flex: none; }
  .preview-caption button:hover:not(:disabled), .placement-options button:hover:not(:disabled) { border-color: var(--hand-seal); }
  .placement-row { flex-wrap: wrap; }
  .placement-options { display: flex; flex: none; gap: 4px; }
  .placement-options button[aria-checked="true"] { border-color: var(--hand-seal); background: var(--hand-card-back); color: var(--hand-seal); font-weight: 600; }
  @media (max-width: 720px) {
    .preview-caption { align-items: flex-start; flex-direction: column; gap: 8px; }
    .placement-options { width: 100%; }
    .placement-options button { flex: 1; }
  }
  @media (forced-colors: active) {
    .mini-preview, .mini-document, .mini-card, .preview-caption button, .placement-options button { border-color: CanvasText; }
    .placement-options button[aria-checked="true"] { outline: 2px solid Highlight; outline-offset: -4px; }
  }
</style>
