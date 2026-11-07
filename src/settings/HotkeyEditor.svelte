<script lang="ts">
  // 呼出快捷键：点一下录制框，直接按下想要的组合键；校验通过即保存并即时生效，Esc 取消。
  // 录制期间请后端暂时注销当前呼出键，否则按到同一组合会被系统拦走去呼出面板。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, HotkeyStatus, SettingsUpdate } from "../lib/ipc";

  let { hotkey, disabled, onsave }: {
    hotkey: string;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  let recording = $state(false);
  let held = $state<string[]>([]);
  let error = $state("");
  let warning = $state<string | null>(null);
  let busy = $state(false);
  let runtime = $state<HotkeyStatus | null>(null);
  let runtimeError = $state("");
  let disposed = false;
  let statusRequest = 0;

  const MODIFIER_CODES = /^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/;
  const MODIFIER_KEYCODES = new Set([16, 17, 18, 91, 92, 93]);
  const NAMED_KEYCODES: Record<number, string> = {
    8: "Backspace", 9: "Tab", 13: "Enter", 19: "Pause", 20: "CapsLock", 27: "Escape", 32: "Space",
    33: "PageUp", 34: "PageDown", 35: "End", 36: "Home", 37: "ArrowLeft", 38: "ArrowUp", 39: "ArrowRight", 40: "ArrowDown",
    44: "PrintScreen", 45: "Insert", 46: "Delete", 106: "NumpadMultiply", 107: "NumpadAdd", 109: "NumpadSubtract",
    110: "NumpadDecimal", 111: "NumpadDivide", 144: "NumLock", 145: "ScrollLock", 186: "Semicolon", 187: "Equal",
    188: "Comma", 189: "Minus", 190: "Period", 191: "Slash", 192: "Backquote", 219: "BracketLeft", 220: "Backslash",
    221: "BracketRight", 222: "Quote",
  };
  // 没有扫描码的输入（远程桌面、改键工具、SendInput 只填虚拟键）code 为空，按 keyCode 补出同名 code。
  const keyCodeName = (event: KeyboardEvent) => {
    const code = event.keyCode;
    if (code >= 65 && code <= 90) return `Key${String.fromCharCode(code)}`;
    if (code >= 48 && code <= 57) return `Digit${code - 48}`;
    if (code >= 96 && code <= 105) return `Numpad${code - 96}`;
    if (code >= 112 && code <= 135) return `F${code - 111}`;
    return NAMED_KEYCODES[code] ?? "";
  };
  const errorMessage = (value: unknown, fallback: string) => value instanceof Error ? value.message : typeof value === "string" ? value : fallback;
  const MODIFIER_ORDER = ["control", "ctrl", "alt", "shift", "super", "win"];
  const rank = (part: string) => { const at = MODIFIER_ORDER.indexOf(part.toLowerCase()); return at < 0 ? 99 : at; };
  // 后端规范化后的顺序是 shift+control+…；显示统一成 Ctrl、Alt、Shift、Win、主键。
  const display = (value: string) => value.split("+").sort((a, b) => rank(a) - rank(b)).map((part) => part
    .replace(/^control$/i, "Ctrl").replace(/^super$/i, "Win").replace(/^shift$/i, "Shift").replace(/^alt$/i, "Alt")
    .replace(/^Key([A-Z])$/, "$1").replace(/^Digit([0-9])$/, "$1").replace(/^Backquote$/, "`")
    .replace(/^Numpad(.+)$/, "小键盘 $1").replace(/^Arrow(Up|Down|Left|Right)$/, (_, side: string) => ({ Up: "↑", Down: "↓", Left: "←", Right: "→" })[side] ?? side))
    .join(" + ");
  const modifiersOf = (event: KeyboardEvent) => [
    ...(event.ctrlKey ? ["Ctrl"] : []), ...(event.altKey ? ["Alt"] : []),
    ...(event.shiftKey ? ["Shift"] : []), ...(event.metaKey ? ["Super"] : []),
  ];

  async function refreshRuntime() {
    const request = ++statusRequest;
    try {
      const value = await ipc.getHotkeyStatus();
      if (!disposed && request === statusRequest) { runtime = value; runtimeError = ""; }
    } catch (failure) {
      if (!disposed && request === statusRequest) runtimeError = errorMessage(failure, "注册状态读取失败");
    }
  }

  async function start() {
    if (disabled || recording || busy) return;
    recording = true; held = []; error = ""; warning = null;
    try { await ipc.pauseHotkey(true); } catch { /* 暂停失败也能录，只是按到当前组合会呼出面板 */ }
  }

  async function stop() {
    if (!recording) return;
    recording = false; held = [];
    try { await ipc.pauseHotkey(false); } catch { /* 后端 30 秒后会自动恢复 */ }
    await refreshRuntime();
  }

  async function capture(event: KeyboardEvent) {
    if (!recording) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.repeat || busy) return;
    const modifiers = modifiersOf(event);
    if (MODIFIER_CODES.test(event.code) || MODIFIER_KEYCODES.has(event.keyCode)) { held = modifiers; return; }
    const code = event.code || keyCodeName(event);
    if (!code) { error = "这个键不能用作快捷键"; return; }
    if (code === "Escape" && !modifiers.length) { error = ""; await stop(); return; }
    if (!modifiers.length) { error = "需要配合 Ctrl、Alt、Shift 或 Win 中的至少一个"; return; }
    const value = [...modifiers, code].join("+");
    busy = true;
    try {
      const checked = await ipc.validateHotkey(value);
      if (disposed) return;
      error = "";
      warning = checked.warning ?? null;
      if (checked.hotkey !== hotkey) await onsave({ hotkey: checked.hotkey }, `呼出快捷键已改为 ${display(checked.hotkey)}`);
      await stop();
    } catch (failure) {
      // 系统保留或无效：留在录制状态，直接再按一个。
      error = errorMessage(failure, "这个组合不能用，换一个试试");
      held = [];
    } finally {
      busy = false;
    }
  }

  function release(event: KeyboardEvent) {
    if (!recording) return;
    event.preventDefault();
    held = modifiersOf(event);
  }

  onMount(() => {
    void refreshRuntime();
    let unlisten: ipc.UnlistenFn | undefined;
    void ipc.onSettingsChanged(() => { void refreshRuntime(); }).then((stopListening) => {
      if (disposed) stopListening(); else unlisten = stopListening;
    }).catch(() => {});
    const onBlur = () => { void stop(); };
    window.addEventListener("blur", onBlur);
    return () => {
      disposed = true; unlisten?.();
      window.removeEventListener("blur", onBlur);
      if (recording) void ipc.pauseHotkey(false).catch(() => {});
    };
  });
</script>

<svelte:window onkeydown={capture} onkeyup={release} />

<section class="hotkey-group" aria-labelledby="hotkey-title">
  <h3 id="hotkey-title">呼出快捷键</h3>
  <div class="hotkey-card">
    <div class="recorder-row">
      <button
        type="button"
        class="recorder"
        class:recording
        disabled={disabled && !recording}
        aria-describedby="hotkey-help"
        onclick={() => { if (recording) void stop(); else void start(); }}
      >
        {#if recording}
          <span class="live">{held.length ? `${held.map(display).join(" + ")} + …` : "请按下新的组合键"}</span>
        {:else}
          <kbd>{display(hotkey)}</kbd>
        {/if}
      </button>
      <span id="hotkey-help" class="help">{recording ? "Esc 取消" : "点击后直接按下组合键"}</span>
    </div>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if warning && !recording}<p class="hint">{warning}</p>{/if}
    {#if runtimeError}<p class="error">{runtimeError}</p>{/if}
    {#if runtime && !recording}
      {#if runtime.error}
        <p class="error">{runtime.error}{#if runtime.detail}（{runtime.detail}）{/if}</p>
      {/if}
      {#if runtime.overridden}<p class="hint">本进程由 NIANLAI_HOTKEY 测试覆盖为 {display(runtime.requested)}；保存值下次启动使用。</p>{/if}
      {#if runtime.disabled}<p class="hint">自动循环测试已暂停注册呼出键。</p>{/if}
    {/if}
  </div>
</section>

<style>
  .hotkey-group { display: grid; gap: 8px; }
  h3 { margin: 0; color: var(--settings-muted); font-size: 11px; font-weight: 600; letter-spacing: .04em; }
  .hotkey-card { display: grid; gap: 8px; padding: 16px; border: 1px solid var(--settings-line); border-radius: var(--radius-md); background: var(--settings-card); }
  .recorder-row { display: flex; align-items: center; gap: 14px; }
  .recorder { min-width: 220px; min-height: 36px; display: flex; align-items: center; justify-content: center; padding: 5px 14px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-field); color: var(--settings-text); font-size: 12px; cursor: pointer; }
  .recorder:hover:not(:disabled) { border-color: var(--settings-accent); }
  .recorder.recording { border-color: var(--settings-accent); background: var(--settings-selected); }
  .recorder:focus-visible { outline: 2px solid var(--settings-accent); outline-offset: 2px; }
  .recorder:disabled { opacity: .65; cursor: default; }
  kbd { font: 600 12px var(--font-mono); }
  .live { color: var(--settings-muted); font: 12px var(--font-mono); }
  .help { color: var(--settings-muted); font-size: 11px; }
  p { margin: 0; font-size: 11px; line-height: 1.6; overflow-wrap: anywhere; }
  .hint { color: var(--settings-muted); }
  .error { color: var(--state-critical); }
</style>
