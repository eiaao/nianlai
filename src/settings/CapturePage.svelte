<script lang="ts">
  // “记录”分区：采集规则（PRD 6.1，A07–A11）。规则改动即时生效；暂停和“忽略下一次”只在内存里。
  import { onMount } from "svelte";
  import * as ipc from "../lib/ipc";
  import type { AppSettings, CaptureSettings, CaptureState, SettingsUpdate } from "../lib/ipc";

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

  function same(a: string, b: string) {
    return a.toLowerCase() === b.toLowerCase();
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        status = await ipc.getCaptureState();
      } catch {
        stateError = "读取记录状态失败，请关闭窗口后重试";
      }
      try {
        // 最近复制过内容的来源程序，方便直接点选加入排除名单。
        const history = await ipc.getHistory();
        recentApps = [...new Set(history.items.map((item) => item.source).filter((source) => /\.exe$/i.test(source)))];
      } catch {
        recentApps = [];
      }
      unlisten = await ipc.onCaptureChanged((payload) => {
        if (status) status = { ...status, ...payload };
      });
    })();
    return () => unlisten?.();
  });

  async function setPaused(paused: boolean) {
    await ipc.setCapturePaused(paused);
    if (status) status = { ...status, paused };
  }

  async function setIgnoreNext(ignore: boolean) {
    await ipc.setIgnoreNext(ignore);
    if (status) status = { ...status, ignore_next: ignore };
  }

  function saveApps(apps: string[], message: string) {
    return onsave({ capture: { excluded_apps: apps } }, message);
  }

  async function addApp(name: string) {
    const value = name.trim();
    draftError = "";
    if (!value) return;
    if (/[\\/]/.test(value)) { draftError = "只填程序文件名，例如 KeePass.exe，不要带路径"; return; }
    if (excluded.some((app) => same(app, value))) { draftError = `${value} 已在名单里`; return; }
    if (await saveApps([...excluded, value], `已排除 ${value}`)) draft = "";
  }

  function toggleRule(id: string, enabled: boolean) {
    const current = capture?.sensitive_off ?? [];
    const next = enabled ? current.filter((rule) => rule !== id) : [...current, id];
    return onsave({ capture: { sensitive_off: next } }, enabled ? "已开启这条规则" : "已关闭这条规则");
  }

  function setType(key: "text" | "rich_text" | "images" | "files", value: boolean, label: string) {
    return onsave({ capture: { [key]: value } }, `${value ? "记录" : "不再记录"}${label}`);
  }
</script>

<section class="page">
  <div><h2>隐私与记录</h2><p class="page-intro">每次复制依次检查：暂停 → 排除的程序 → 敏感内容 → 类型和大小 → 临时内容。被跳过的内容不会写入磁盘，日志只记跳过的原因。</p></div>

  <div class="group"><span class="group-title">记录状态</span><div class="card">
    <label class="setting">
      <span class="setting-copy"><strong>暂停记录</strong><small>暂停期间复制的内容都不保存。只在本次运行有效，重启拈来后自动恢复记录；托盘菜单里也能切换。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={status?.paused ?? false} disabled={!status} onchange={(event) => setPaused(event.currentTarget.checked)}>
    </label>
    <label class="setting">
      <span class="setting-copy"><strong>忽略下一次复制</strong><small>下一次复制的内容不保存，之后照常记录。适合临时复制一次不想留下的内容。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={status?.ignore_next ?? false} disabled={!status} onchange={(event) => setIgnoreNext(event.currentTarget.checked)}>
    </label>
    {#if stateError}<p class="note error">{stateError}</p>{/if}
  </div></div>

  <div class="group"><span class="group-title">排除的程序</span><div class="card">
    <div class="setting column">
      <span class="setting-copy"><strong>从这些程序复制的内容不记录</strong><small>按程序文件名匹配，不区分大小写。默认包含常见的密码管理器。</small></span>
      <ul class="chips" aria-label="排除的程序">
        {#each excluded as app (app)}
          <li><span>{app}</span><button type="button" aria-label={`移除 ${app}`} disabled={off} onclick={() => saveApps(excluded.filter((name) => name !== app), `已移除 ${app}`)}>×</button></li>
        {:else}
          <li class="empty">名单为空，所有程序的复制都会记录</li>
        {/each}
      </ul>
      <form class="add" onsubmit={(event) => { event.preventDefault(); void addApp(draft); }}>
        <input type="text" placeholder="程序文件名，例如 KeePass.exe" bind:value={draft} disabled={off} spellcheck="false" autocomplete="off">
        <button type="submit" disabled={off || !draft.trim()}>添加</button>
        {#if status && status.default_excluded_apps.some((app) => !excluded.some((name) => same(name, app)))}
          <button type="button" class="link" disabled={off} onclick={() => saveApps([...excluded, ...status!.default_excluded_apps.filter((app) => !excluded.some((name) => same(name, app)))], "已补回默认的密码管理器")}>补回默认名单</button>
        {/if}
      </form>
      {#if draftError}<p class="note error">{draftError}</p>{/if}
      {#if suggestions.length}
        <div class="suggest"><small>最近复制过的程序：</small>
          {#each suggestions as app (app)}<button type="button" disabled={off} onclick={() => addApp(app)}>+ {app}</button>{/each}
        </div>
      {/if}
    </div>
  </div></div>

  <div class="group"><span class="group-title">敏感内容</span><div class="card">
    <p class="note">密码管理器写入的“不要记录”标记始终遵守。下面的规则再按内容识别常见的密钥和令牌，命中就不记录；只认格式明确的内容，普通文字不会被误拦。</p>
    {#each status?.rules ?? [] as rule (rule.id)}
      <label class="setting compact">
        <span class="setting-copy"><strong>{rule.label}</strong><small>{rule.example}</small></span>
        <input class="switch" type="checkbox" role="switch" checked={!(capture?.sensitive_off ?? []).includes(rule.id)} disabled={off} onchange={(event) => toggleRule(rule.id, event.currentTarget.checked)}>
      </label>
    {/each}
  </div></div>

  <div class="group"><span class="group-title">记录的类型</span><div class="card">
    <label class="setting compact">
      <span class="setting-copy"><strong>文字</strong><small>关闭后富文本也不再记录。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.text ?? true} disabled={off} onchange={(event) => setType("text", event.currentTarget.checked, "文字")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>富文本格式</strong><small>网页、Word 里复制的格式（HTML、RTF）。关闭后只保留纯文字。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={(capture?.text ?? true) && (capture?.rich_text ?? true)} disabled={off || !(capture?.text ?? true)} onchange={(event) => setType("rich_text", event.currentTarget.checked, "富文本格式")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>图片</strong><small>截图和“复制图片”。单张超过 20 MB 的不记录。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.images ?? true} disabled={off} onchange={(event) => setType("images", event.currentTarget.checked, "图片")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>文件</strong><small>只记录文件路径，不复制文件本身。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.files ?? true} disabled={off} onchange={(event) => setType("files", event.currentTarget.checked, "文件")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>单条文字上限</strong><small>超过的整条不记录。</small></span>
      <select value={String(capture?.max_text_kb ?? 2048)} disabled={off || !(capture?.text ?? true)} onchange={(event) => onsave({ capture: { max_text_kb: Number(event.currentTarget.value) } }, "文字上限已保存")}>
        <option value="64">64 KB</option><option value="256">256 KB</option><option value="1024">1 MB</option><option value="2048">2 MB（默认）</option>
      </select>
    </label>
  </div></div>

  <div class="group"><span class="group-title">临时内容</span><div class="card">
    <label class="setting">
      <span class="setting-copy"><strong>忽略临时借用剪贴板的内容</strong><small>文本扩展、自动化脚本等工具会先把内容放进剪贴板粘贴，随后还原。新复制的内容先等一小段时间再保存，期间剪贴板被还原就不记录。呼出面板时会立即保存，不影响查看刚复制的内容。</small></span>
      <input class="switch" type="checkbox" role="switch" checked={capture?.transient ?? true} disabled={off} onchange={(event) => onsave({ capture: { transient: event.currentTarget.checked } }, event.currentTarget.checked ? "已开启临时内容识别" : "已关闭临时内容识别，复制后立即保存")}>
    </label>
    <label class="setting compact">
      <span class="setting-copy"><strong>等待时间</strong><small>越长识别得越全；这段时间内程序被强行结束，最后一条会丢失。</small></span>
      <select value={String(capture?.transient_ms ?? 1000)} disabled={off || !(capture?.transient ?? true)} onchange={(event) => onsave({ capture: { transient_ms: Number(event.currentTarget.value) } }, "等待时间已保存")}>
        <option value="500">0.5 秒</option><option value="1000">1 秒（默认）</option><option value="1500">1.5 秒</option><option value="2000">2 秒</option>
      </select>
    </label>
  </div></div>
</section>

<style>
  .setting.compact { min-height: 52px; padding-top: 10px; padding-bottom: 10px; }
  .setting.column { flex-direction: column; align-items: stretch; gap: 10px; }
  .switch { width: 34px; height: 18px; flex: none; accent-color: var(--settings-accent); }
  .note { margin: 0; padding: 11px 16px; border-bottom: 1px solid var(--settings-line); color: var(--settings-muted); font-size: 11px; line-height: 1.6; }
  .setting .note { padding: 0; border: 0; }
  .note.error { color: var(--state-critical); }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .chips li { display: inline-flex; align-items: center; gap: 2px; padding: 2px 4px 2px 9px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-field); font: 11px var(--font-mono); }
  .chips li.empty { padding-right: 9px; color: var(--settings-muted); font-family: var(--font-ui); }
  .chips button { width: 20px; height: 20px; border: 0; background: transparent; color: var(--settings-muted); }
  .chips button:hover:not(:disabled) { color: var(--state-critical); }
  .add { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .add input { min-width: 220px; flex: 1; min-height: 33px; padding: 5px 9px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-field); font-size: 11px; }
  .add button, .suggest button { min-height: 30px; padding: 4px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; }
  .add button.link { border: 0; background: transparent; color: var(--settings-accent); text-decoration: underline; }
  .suggest { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .suggest small { color: var(--settings-muted); font-size: 10px; }
  .suggest button { min-height: 24px; padding: 2px 8px; font: 10px var(--font-mono); }
  button:disabled, input:disabled { opacity: .6; }
  @media (max-width: 720px) {
    .setting:not(.column) { align-items: flex-start; flex-direction: column; gap: 9px; }
    select { width: 100%; }
  }
</style>
