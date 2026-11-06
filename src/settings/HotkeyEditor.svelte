<script lang="ts">
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, HotkeyStatus, HotkeyValidation, SettingsUpdate } from "../lib/ipc";

  let { hotkey, disabled, onsave }: {
    hotkey: string;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  const keyGroups = [
    { label: "字母", keys: Array.from({ length: 26 }, (_, index) => ({ value: `Key${String.fromCharCode(65 + index)}`, label: String.fromCharCode(65 + index) })) },
    { label: "数字", keys: Array.from({ length: 10 }, (_, index) => ({ value: `Digit${index}`, label: String(index) })) },
    { label: "功能键", keys: Array.from({ length: 24 }, (_, index) => ({ value: `F${index + 1}`, label: `F${index + 1}` })) },
    { label: "常用键", keys: [
      ["Backquote", "反引号 `"], ["Space", "空格"], ["Tab", "Tab"], ["Enter", "Enter"],
      ["Escape", "Esc"], ["Backspace", "Backspace"], ["Delete", "Delete"], ["Insert", "Insert"],
      ["Home", "Home"], ["End", "End"], ["PageUp", "PageUp"], ["PageDown", "PageDown"],
      ["ArrowLeft", "←"], ["ArrowRight", "→"], ["ArrowUp", "↑"], ["ArrowDown", "↓"],
      ["CapsLock", "CapsLock"], ["NumLock", "NumLock"], ["ScrollLock", "ScrollLock"],
      ["PrintScreen", "PrintScreen"], ["Pause", "Pause"],
    ].map(([value, label]) => ({ value, label })) },
    { label: "符号", keys: [
      ["Minus", "-"], ["Equal", "="], ["BracketLeft", "["], ["BracketRight", "]"], ["Backslash", "\\"],
      ["Semicolon", ";"], ["Quote", "'"], ["Comma", ","], ["Period", "."], ["Slash", "/"],
    ].map(([value, label]) => ({ value, label })) },
    { label: "数字小键盘", keys: [
      ...Array.from({ length: 10 }, (_, index) => ({ value: `Numpad${index}`, label: `小键盘 ${index}` })),
      ...[["NumpadAdd", "+"], ["NumpadSubtract", "-"], ["NumpadMultiply", "*"], ["NumpadDivide", "/"],
        ["NumpadDecimal", "."], ["NumpadEnter", "Enter"]].map(([value, label]) => ({ value, label: `小键盘 ${label}` })),
    ] },
  ];

  let control = $state(false);
  let alt = $state(false);
  let shift = $state(false);
  let win = $state(false);
  let key = $state("");
  let loadedHotkey = $state<string | null>(null);
  let checking = $state(false);
  let validation = $state<HotkeyValidation | null>(null);
  let validatedDraft = $state("");
  let validationError = $state("");
  let loadError = $state("");
  let runtime = $state<HotkeyStatus | null>(null);
  let runtimeError = $state("");
  let disposed = false;
  let statusRequest = 0;
  const draft = $derived([
    ...(control ? ["Ctrl"] : []), ...(alt ? ["Alt"] : []),
    ...(shift ? ["Shift"] : []), ...(win ? ["Super"] : []), key,
  ].join("+"));
  const draftReason = $derived(!key ? "主键不能为空，请选择一个主键"
    : !control && !alt && !shift && !win ? "至少选择一个修饰键（Ctrl / Alt / Shift / Win）" : "");
  const errorMessage = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === "string" ? error : fallback;
  const display = (value: string) => value.replace(/control/gi, "Ctrl").replace(/super/gi, "Win").replace(/shift/gi, "Shift").replace(/alt/gi, "Alt").replace(/Key([A-Z])/g, "$1").replace(/Digit([0-9])/g, "$1").replace(/Backquote/g, "反引号");

  // 后端是校验规则的唯一来源；别名和旧设置由后端规范化后再填入编辑器。
  $effect(() => {
    const saved = hotkey;
    if (disabled || saved === loadedHotkey) return;
    let cancelled = false;
    void ipc.validateHotkey(saved).then((value) => {
      if (cancelled) return;
      control = value.control; alt = value.alt; shift = value.shift; win = value.win; key = value.key;
      loadedHotkey = saved;
      loadError = "";
    }).catch((error: unknown) => {
      if (!cancelled) loadError = errorMessage(error, "呼出键读取失败，请重新打开设置");
    });
    return () => { cancelled = true; };
  });

  $effect(() => {
    const value = draft;
    const reason = draftReason;
    if (loadedHotkey === null) return;
    validation = null;
    validationError = reason;
    checking = !reason;
    if (reason) return;
    let cancelled = false;
    void ipc.validateHotkey(value).then((result) => {
      if (cancelled) return;
      validation = result;
      validatedDraft = value;
      checking = false;
    }).catch((error: unknown) => {
      if (cancelled) return;
      validationError = errorMessage(error, "快捷键校验失败，请重试");
      checking = false;
    });
    // 较早的校验回执不得放行较新的非法草稿。
    return () => { cancelled = true; };
  });

  async function refreshRuntime() {
    const request = ++statusRequest;
    try {
      const value = await ipc.getHotkeyStatus();
      if (!disposed && request === statusRequest) { runtime = value; runtimeError = ""; }
    } catch (error) {
      if (!disposed && request === statusRequest) runtimeError = errorMessage(error, "注册状态读取失败，请点击刷新状态");
    }
  }

  async function saveHotkey() {
    if (disabled || checking || draftReason || !validation || validatedDraft !== draft) return;
    const saved = await onsave({ hotkey: validation.hotkey }, "呼出快捷键已保存；实际注册状态见上方，无需重启");
    if (saved) await refreshRuntime();
  }

  onMount(() => {
    void refreshRuntime();
    let unlisten: ipc.UnlistenFn | undefined;
    void ipc.onSettingsChanged(() => { void refreshRuntime(); }).then((stop) => {
      if (disposed) stop(); else unlisten = stop;
    }).catch(() => { /* 命令保存后仍会主动回读；广播监听失败不伪造成功状态。 */ });
    return () => { disposed = true; unlisten?.(); };
  });
</script>

<section class="hotkey-group" aria-labelledby="hotkey-title">
  <h3 id="hotkey-title">呼出快捷键</h3>
  <div class="hotkey-card">
    <p class="intro">选择修饰键与一个主键，再确认保存。即时生效，无需重启；不会改变面板的焦点行为。</p>
    <p class="current">已保存：<strong>{display(hotkey)}</strong></p>
    <fieldset disabled={disabled || loadedHotkey === null}>
      <legend>修饰键（至少一个）</legend>
      <div class="modifiers">
        <label><input type="checkbox" bind:checked={control} />Ctrl</label>
        <label><input type="checkbox" bind:checked={alt} />Alt</label>
        <label><input type="checkbox" bind:checked={shift} />Shift</label>
        <label><input type="checkbox" bind:checked={win} />Win</label>
      </div>
      <label class="main-key" for="hotkey-main">主键
        <select id="hotkey-main" bind:value={key} aria-invalid={!!validationError} aria-describedby="hotkey-validation">
          <option value="">请选择一个主键</option>
          {#each keyGroups as group}
            <optgroup label={group.label}>
              {#each group.keys as entry}<option value={entry.value}>{entry.label}</option>{/each}
            </optgroup>
          {/each}
        </select>
      </label>
    </fieldset>
    <div id="hotkey-validation" class:error={!!validationError || !!loadError} class="validation" role="status" aria-live="polite">
      {loadError || validationError || (checking ? "正在校验…" : validation ? `待保存：${display(validation.hotkey)}` : "正在读取快捷键…")}
    </div>
    {#if validation?.warning}<p class="hint">提示：{validation.warning}</p>{/if}
    <div class="actions">
      <button type="button" disabled={disabled || checking || loadedHotkey === null || !!draftReason || !validation || validatedDraft !== draft} onclick={saveHotkey}>{runtime?.error ? "保存并重试注册" : "保存快捷键"}</button>
      <button type="button" class="secondary" disabled={disabled} onclick={refreshRuntime}>刷新状态</button>
    </div>
    <div class="runtime" class:error={!!runtime?.error || !!runtimeError} role="status" aria-live="polite">
      {#if runtimeError}<p>{runtimeError}</p>{/if}
      {#if runtime}
        <p>当前生效：<strong>{runtime.active ? display(runtime.active) : "无（可从托盘打开面板）"}</strong></p>
        {#if runtime.error}<p>{runtime.error}</p>{/if}
        {#if runtime.detail}<p class="detail">系统返回：{runtime.detail}</p>{/if}
        {#if runtime.overridden}<p>本进程由 NIANLAI_HOTKEY 测试覆盖：{display(runtime.requested)}。保存值已保留，移除覆盖后下次启动使用。</p>{/if}
        {#if runtime.disabled}<p>自动循环测试已暂停注册呼出键；保存值不受影响。</p>{/if}
      {:else if !runtimeError}<p>正在读取注册状态…</p>{/if}
    </div>
    <p class="hint">已知系统保留组合会阻止保存；常见应用冲突只提示，不强制阻止。Win 键可选，但最终可用性以系统注册结果为准。</p>
  </div>
</section>

<style>
  .hotkey-group { display: grid; gap: 8px; }
  h3 { margin: 0; color: var(--settings-muted); font-size: 11px; font-weight: 600; letter-spacing: .04em; }
  .hotkey-card { display: grid; gap: 10px; padding: 16px; border: 1px solid var(--settings-line); border-radius: var(--radius-md); background: var(--settings-card); }
  p { margin: 0; font-size: 11px; line-height: 1.6; }
  .intro, .hint, .detail { color: var(--settings-muted); }
  .current strong, .runtime strong { font-family: var(--font-mono); font-weight: 600; }
  fieldset { min-width: 0; display: grid; gap: 12px; margin: 0; padding: 0; border: 0; }
  legend { margin-bottom: 8px; padding: 0; font-size: 11px; }
  .modifiers { display: flex; flex-wrap: wrap; gap: 8px 16px; }
  .modifiers label { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  input { accent-color: var(--settings-accent); }
  .main-key { display: flex; align-items: center; gap: 16px; font-size: 11px; }
  select { min-width: 180px; min-height: 33px; padding: 5px 9px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-field); color: var(--settings-text); font-size: 11px; }
  .validation { min-height: 16px; color: var(--settings-muted); font-size: 11px; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; }
  button { min-height: 33px; padding: 5px 12px; border: 1px solid var(--settings-accent); border-radius: var(--radius-sm); background: var(--settings-selected); color: var(--settings-text); font-size: 11px; }
  button.secondary { border-color: var(--settings-line); background: transparent; }
  :disabled { opacity: .65; }
  .runtime { display: grid; gap: 4px; padding-top: 10px; border-top: 1px solid var(--settings-line); overflow-wrap: anywhere; }
  .error { color: var(--state-critical); }
</style>
