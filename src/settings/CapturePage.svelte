<script lang="ts">
  // 暂停和“下一次复制不记”只改运行状态；其余规则仍按原有设置字段保存。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, CaptureSettings, CaptureState, SettingsUpdate } from "../lib/ipc";
  import { message } from "./format";

  let { capture, disabled, onsave }: {
    capture: CaptureSettings | null;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  let status = $state<CaptureState | null>(null);
  let recentApps = $state<string[]>([]);
  let draft = $state("");
  let draftError = $state("");
  let stateError = $state("");

  const off = $derived(!capture || disabled);
  const excluded = $derived(capture?.excluded_apps ?? []);
  const suggestions = $derived(recentApps.filter((app) => !excluded.some((name) => same(name, app))).slice(0, 8));

  function same(a: string, b: string) { return a.toLowerCase() === b.toLowerCase(); }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const value = await ipc.getCaptureState();
        if (!disposed) status = value;
      } catch {
        if (!disposed) stateError = "没读到记录状态，关掉窗口再开试试";
      }
      try {
        const history = await ipc.getHistory();
        if (!disposed) recentApps = [...new Set(history.items.map((item) => item.source).filter((source) => /\.exe$/i.test(source)))];
      } catch {
        if (!disposed) recentApps = [];
      }
      try {
        const stop = await ipc.onCaptureChanged((payload) => {
          if (status) status = { ...status, ...payload };
        });
        if (disposed) stop(); else unlisten = stop;
      } catch (value) {
        if (!disposed) stateError = `记录状态变化没收到：${message(value)}`;
      }
    })();
    return () => { disposed = true; unlisten?.(); };
  });

  async function setPaused(paused: boolean, input: HTMLInputElement) {
    stateError = "";
    try {
      await ipc.setCapturePaused(paused);
      if (status) status = { ...status, paused };
    } catch (value) {
      input.checked = status?.paused ?? false;
      stateError = `暂停状态没改成：${message(value)}`;
    }
  }
  async function setIgnoreNext(ignore: boolean, input: HTMLInputElement) {
    stateError = "";
    try {
      await ipc.setIgnoreNext(ignore);
      if (status) status = { ...status, ignore_next: ignore };
    } catch (value) {
      input.checked = status?.ignore_next ?? false;
      stateError = `下一次复制不记没改成：${message(value)}`;
    }
  }

  function saveApps(apps: string[], text: string) {
    return onsave({ capture: { excluded_apps: apps } }, text);
  }
  async function addApp(name: string) {
    const value = name.trim();
    draftError = "";
    if (!value) return;
    if (/[\\/]/.test(value)) { draftError = "只填程序名，比如 KeePass.exe"; return; }
    if (excluded.some((app) => same(app, value))) { draftError = `${value} 已在名单里`; return; }
    if (await saveApps([...excluded, value], `已排除 ${value}`)) draft = "";
  }
  function toggleRule(id: string, enabled: boolean) {
    const current = capture?.sensitive_off ?? [];
    const next = enabled ? current.filter((rule) => rule !== id) : [...current, id];
    return onsave({ capture: { sensitive_off: next } }, enabled ? "已开启这条规则" : "已关闭这条规则");
  }
  function setType(key: "text" | "rich_text" | "images" | "files", value: boolean, label: string) {
    return onsave({ capture: { [key]: value } }, `${value ? "记" : "不记"}${label}`);
  }
</script>

<section class="page">
  <div><h2>记录与隐私</h2><p class="page-intro">你复制的东西，哪些记、哪些不记。不记的，不会留下任何痕迹。</p></div>

  <div class="state-grid">
    <label class="setting state-card">
      <span class="setting-copy"><strong>暂停记录</strong><small>暂停时复制的都不记。拈来重新打开后会接着记；托盘图标上也能点。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={status?.paused ?? false} disabled={!status} onchange={(event) => setPaused(event.currentTarget.checked, event.currentTarget)}>
    </label>
    <label class="setting state-card">
      <span class="setting-copy"><strong>下一次复制不记</strong><small>只跳过一次，之后照常记。适合临时复制一次密码。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={status?.ignore_next ?? false} disabled={!status} onchange={(event) => setIgnoreNext(event.currentTarget.checked, event.currentTarget)}>
    </label>
  </div>
  {#if stateError}<p class="state-error" role="alert">{stateError}</p>{/if}

  <div class="group"><span class="group-title split">这些程序里复制的不记<em>已经放了常见的密码管理器</em></span><div class="card">
    <div class="setting column">
      <span class="setting-copy"><strong>名单里的程序，复制的内容不记</strong><small>按程序名匹配，不区分大小写。点 × 拿掉。</small></span>
      <ul class="chips" aria-label="排除的程序">
        {#each excluded as app (app)}
          <li><span>{app}</span><button type="button" aria-label={`拿掉 ${app}`} disabled={off} onclick={() => saveApps(excluded.filter((name) => name !== app), `已拿掉 ${app}`)}>×</button></li>
        {:else}
          <li class="empty">还没有，所有程序里复制的都记</li>
        {/each}
      </ul>
      <form class="add" onsubmit={(event) => { event.preventDefault(); void addApp(draft); }}>
        <input type="text" aria-label="添加程序" placeholder="程序名，比如 KeePass.exe" bind:value={draft} disabled={off} spellcheck="false" autocomplete="off">
        <button type="submit" disabled={off || !draft.trim()}>添加程序</button>
        {#if status && status.default_excluded_apps.some((app) => !excluded.some((name) => same(name, app)))}
          <button type="button" class="link" disabled={off} onclick={() => saveApps([...excluded, ...status!.default_excluded_apps.filter((app) => !excluded.some((name) => same(name, app)))], "已放回默认的密码管理器")}>放回默认的密码管理器</button>
        {/if}
      </form>
      {#if draftError}<p class="note error" role="alert">{draftError}</p>{/if}
      {#if suggestions.length}
        <div class="suggest"><small>最近复制过的程序：</small>
          {#each suggestions as app (app)}<button type="button" disabled={off} onclick={() => addApp(app)}>+ {app}</button>{/each}
        </div>
      {/if}
    </div>
  </div></div>

  <div class="group"><span class="group-title split">像密码的内容不记<em>格式明确，普通文字不会被误拦</em></span><div class="card">
    <p class="note">密码管理器里复制的一律不记；下面每条规则都配一个打码示例。</p>
    <div class="rules">
      {#each status?.rules ?? [] as rule (rule.id)}
        <label class="setting compact">
          <span class="setting-copy"><strong>{rule.label}</strong><small class="masked">{rule.example}</small></span>
          <input class="switch" type="checkbox" role="switch" checked={!(capture?.sensitive_off ?? []).includes(rule.id)} disabled={off} onchange={(event) => toggleRule(rule.id, event.currentTarget.checked)}>
        </label>
      {/each}
    </div>
  </div></div>

  <div class="group"><span class="group-title">记哪些类型</span><div class="card types">
    <label class="setting compact">
      <span class="setting-copy"><strong>文字</strong><small>关闭后带格式的文字也不记。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.text ?? true} disabled={off} onchange={(event) => setType("text", event.currentTarget.checked, "文字")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>带格式的文字</strong><small>网页、Word 里的字体和颜色。关闭后只留纯文字。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={(capture?.text ?? true) && (capture?.rich_text ?? true)} disabled={off || !(capture?.text ?? true)} onchange={(event) => setType("rich_text", event.currentTarget.checked, "带格式的文字")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>图片</strong><small>截图和复制图片，单张超过 20 MB 的不记。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.images ?? true} disabled={off} onchange={(event) => setType("images", event.currentTarget.checked, "图片")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>文件</strong><small>只记文件在哪，不存文件本身。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.files ?? true} disabled={off} onchange={(event) => setType("files", event.currentTarget.checked, "文件")}>
    </label>
  </div></div>

  <details class="capture-more">
    <summary>了解更多：拈来怎么判断记不记</summary>
    <div class="more-body">
      <p>每次复制，拈来按这个顺序看：是不是暂停了 → 是不是来自上面那些程序 → 像不像密码 → 是不是要记的类型、是不是太大 → 是不是别的工具临时借用剪贴板。有一条对上就不记，也不会留下任何记录。</p>
      <div class="card">
        <label class="setting compact">
          <span class="setting-copy"><strong>一条文字最长多少</strong><small>超过的整条不记。</small></span>
          <select value={String(capture?.max_text_kb ?? 2048)} disabled={off || !(capture?.text ?? true)} onchange={(event) => onsave({ capture: { max_text_kb: Number(event.currentTarget.value) } }, "文字上限已保存")}>
            <option value="64">64 KB</option><option value="256">256 KB</option><option value="1024">1 MB</option><option value="2048">2 MB（默认）</option>
          </select>
        </label>
        <label class="setting">
          <span class="setting-copy"><strong>别的工具临时借用剪贴板时不记</strong><small>有些工具会借剪贴板贴一下再放回，这种不记。呼出面板时会立即记下，不耽误看刚复制的内容。</small></span>
          <input class="switch" type="checkbox" role="switch" checked={capture?.transient ?? true} disabled={off} onchange={(event) => onsave({ capture: { transient: event.currentTarget.checked } }, event.currentTarget.checked ? "已开启临时内容识别" : "已关闭临时内容识别")}>
        </label>
        <label class="setting compact">
          <span class="setting-copy"><strong>等多久再记</strong><small>越久认得越准；这段时间里强行关掉拈来，最后一条会丢。</small></span>
          <select value={String(capture?.transient_ms ?? 1000)} disabled={off || !(capture?.transient ?? true)} onchange={(event) => onsave({ capture: { transient_ms: Number(event.currentTarget.value) } }, "等待时间已保存")}>
            <option value="500">0.5 秒</option><option value="1000">1 秒（默认）</option><option value="1500">1.5 秒</option><option value="2000">2 秒</option>
          </select>
        </label>
      </div>
    </div>
  </details>
</section>

<style>
  .state-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  .state-card { border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); }
  .state-error { margin: -12px 0 0; color: var(--state-critical); font-size: 12px; }
  .split { display: flex; justify-content: space-between; gap: 12px; }
  .split em { font-style: normal; font-weight: 400; letter-spacing: 0; }
  .setting.compact { min-height: 56px; padding-top: 12px; padding-bottom: 12px; }
  .setting.column { flex-direction: column; align-items: stretch; gap: 12px; }
  .note { margin: 0; padding: 12px 16px; border-bottom: 1px solid var(--hand-line); color: var(--hand-ink-2); font-size: 12px; line-height: 1.6; }
  .setting .note { padding: 0; border: 0; }
  .note.error { color: var(--state-critical); }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; margin: 0; padding: 4px 0; list-style: none; }
  .chips li { display: inline-flex; align-items: center; gap: 4px; padding: 4px 4px 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); box-shadow: var(--hand-row-shadow); font: 12px var(--font-mono); transform: rotate(-1deg); }
  .chips li:nth-child(even) { transform: rotate(1deg); }
  .chips li.empty { padding-right: 12px; color: var(--hand-ink-2); font-family: var(--font-ui); transform: none; }
  .chips button { width: 24px; height: 24px; border: 0; background: transparent; color: var(--hand-ink-2); }
  .chips button:hover:not(:disabled) { color: var(--state-critical); }
  .add { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .add input { min-width: 220px; flex: 1; min-height: 32px; padding: 4px 8px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card); font-size: 12px; }
  .add button, .suggest button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); font-size: 12px; }
  .add button.link { border: 0; background: transparent; color: var(--hand-seal); text-decoration: underline; }
  .suggest { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .suggest small { color: var(--hand-ink-2); font-size: 12px; }
  .suggest button { font-family: var(--font-mono); }
  button:disabled, input:disabled { opacity: .6; }
  .rules { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .rules .setting:nth-child(odd) { border-right: 1px solid var(--hand-line); }
  .masked { font-family: var(--font-mono); user-select: text; }
  .types { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .types .setting:nth-child(odd) { border-right: 1px solid var(--hand-line); }
  .capture-more { border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); }
  .capture-more summary { min-height: 44px; display: flex; align-items: center; padding: 8px 16px; cursor: pointer; color: var(--hand-ink); font-size: 13px; font-weight: 600; }
  .more-body { display: grid; gap: 12px; padding: 0 16px 16px; }
  .more-body > p { margin: 0; color: var(--hand-ink-2); font-size: 12px; line-height: 1.7; }
  .more-body .card { background: var(--hand-card); }
  @media (max-width: 800px) { .state-grid, .rules, .types { grid-template-columns: 1fr; } .rules .setting:nth-child(odd), .types .setting:nth-child(odd) { border-right: 0; } }
  @media (max-width: 720px) {
    .setting:not(.column) { align-items: flex-start; flex-direction: column; gap: 8px; }
    select { width: 100%; }
    .split { flex-direction: column; gap: 4px; }
  }
</style>
