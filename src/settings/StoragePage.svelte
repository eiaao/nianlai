<script lang="ts">
  // “数据与备份”分区（M2-3 / M2-D）：占用 → 垃圾清理 → 回收站 → 快照 → 备份。
  // 回收站、全部快照、手动挑选截图是这一页的子页面，由 view 切换；面板里的回收站入口直接跳到 trash。
  import * as ipc from "../lib/ipc";
  import TrashSection from "./TrashSection.svelte";
  import SnapshotSection from "./SnapshotSection.svelte";
  import ImagePicker from "./ImagePicker.svelte";
  import ConfirmButton from "./ConfirmButton.svelte";
  import RollingNumber from "./RollingNumber.svelte";
  import type { AppSettings, SettingsUpdate, SnapshotState, StorageOverview } from "../lib/ipc";
  import { count, message, shortTime, size, time } from "./format";

  type StorageView = "main" | "trash" | "snapshots" | "images";

  let { maxMb, disabled, onsave, view = $bindable("main") }: {
    maxMb: number;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
    view?: StorageView;
  } = $props();

  let overview = $state<StorageOverview | null>(null);
  let snapshots = $state<SnapshotState | null>(null);
  let loadError = $state("");
  let busy = $state("");
  // 清理后“+N”飞向回收站那一行；落下时回收站的数字弹一下，再刷新数字让它们滚过去。
  let trashTarget = $state<HTMLElement>();
  let trashNumber = $state<ReturnType<typeof RollingNumber>>();
  function landed() {
    trashNumber?.bump();
    void load();
  }
  let imageRange = $state<"30" | "90" | "all">("30");
  let notices = $state<Record<string, string>>({});
  let errors = $state<Record<string, string>>({});

  const TITLES: Record<StorageView, string> = { main: "", trash: "回收站", snapshots: "全部快照", images: "截图类图片" };
  // 占比色只用现有的强调色和状态色。
  const SEGMENTS = [
    { key: "文本", label: "文本", tone: "text" },
    { key: "富文本", label: "富文本", tone: "rich" },
    { key: "图片", label: "图片", tone: "image" },
    { key: "文件", label: "文件（只存路径）", tone: "file" },
  ];

  const kinds = $derived.by(() => {
    const all = overview?.kinds ?? [];
    const known = SEGMENTS.map((segment) => {
      const found = all.find((kind) => kind.kind === segment.key);
      return { ...segment, count: found?.count ?? 0, bytes: found?.bytes ?? 0 };
    });
    const other = all.filter((kind) => !SEGMENTS.some((segment) => segment.key === kind.kind));
    if (other.length) {
      known.push({ key: "其他", label: "其他", tone: "rich", count: other.reduce((s, k) => s + k.count, 0), bytes: other.reduce((s, k) => s + k.bytes, 0) });
    }
    return known;
  });
  const parts = $derived(overview ? [
    ...kinds.map((kind) => ({ tone: kind.tone, bytes: kind.bytes })),
    { tone: "trash", bytes: overview.trashed.bytes },
    { tone: "snap", bytes: overview.snapshots.bytes },
    { tone: "temp", bytes: overview.exports.bytes + overview.leftovers.bytes },
  ] : []);
  const total = $derived(overview ? overview.database_bytes + overview.snapshots.bytes + overview.exports.bytes + overview.leftovers.bytes : 0);
  const partsTotal = $derived(parts.reduce((sum, part) => sum + part.bytes, 0) || 1);
  const imageChoice = $derived(overview ? (imageRange === "30" ? overview.images.older_30 : imageRange === "90" ? overview.images.older_90 : overview.images.all) : null);
  const latestSnapshot = $derived(snapshots?.items[0] ?? null);
  const restoredItem = $derived(snapshots?.restored ? snapshots.items.find((item) => item.reason === "before_restore") : undefined);
  const compactNeeds = $derived(overview ? overview.database_bytes + overview.database_bytes / 10 + 16 * 1024 * 1024 : 0);
  const compactBlocked = $derived(!!overview && overview.free_bytes !== null && overview.free_bytes < compactNeeds);

  async function load() {
    try {
      [overview, snapshots] = await Promise.all([ipc.getStorage(), ipc.getSnapshots()]);
      loadError = "";
    } catch (error) {
      loadError = message(error);
    }
  }
  // 打开时和从子页面回来时刷新数字。
  $effect(() => { if (view === "main") void load(); });

  async function act(key: string, action: () => Promise<string>) {
    busy = key; errors = { ...errors, [key]: "" }; notices = { ...notices, [key]: "" };
    try {
      notices = { ...notices, [key]: await action() };
      await load();
    } catch (error) {
      errors = { ...errors, [key]: message(error) };
    } finally {
      busy = "";
    }
  }
</script>

<section class="page">
  {#if view !== "main"}
    <div class="crumb">
      <button type="button" class="back" onclick={() => { view = "main"; }}>‹ 数据与备份</button>
      <h2>{TITLES[view]}</h2>
    </div>
    {#if view === "trash"}
      <TrashSection {disabled} />
    {:else if view === "snapshots"}
      <SnapshotSection {snapshots} {maxMb} {disabled} {onsave} onchange={load} />
    {:else}
      <ImagePicker {disabled} onchange={() => { void load(); }} />
    {/if}
  {:else}
    <div><h2>数据与备份</h2><p class="page-intro">拈来的数据都存在这台电脑上。文件类条目只记路径，不复制文件本身；截图、浏览器里复制的图片没有源文件，存在历史库里。这里不会自动删除任何东西。</p></div>

    {#if snapshots?.restored}
      {#if snapshots.restored.error}
        <p class="banner error">上次恢复没有成功，历史保持原样。</p>
      {:else}
        <p class="banner">已恢复到所选快照。恢复前的历史另存成了“恢复前”快照{restoredItem ? `（${time(restoredItem.created_ms)}）` : ""}，需要时可以在“全部快照”里恢复回去。</p>
      {/if}
    {/if}
    {#if loadError}<p class="banner error">{loadError}</p>{/if}

    <div class="group"><span class="group-title split">占用{#if overview?.free_bytes != null}<em>所在磁盘剩余 {size(overview.free_bytes)}</em>{/if}</span><div class="card usage">
      {#if !overview}
        <p class="note">正在统计…</p>
      {:else}
        <div class="usage-top"><strong>{size(total)}</strong><span>历史库 {size(overview.database_bytes)} · 快照 {size(overview.snapshots.bytes)}{overview.exports.bytes + overview.leftovers.bytes ? ` · 临时文件 ${size(overview.exports.bytes + overview.leftovers.bytes)}` : ""}</span></div>
        <div class="meter" aria-hidden="true">{#each parts as part}{#if part.bytes}<i class={part.tone} style={`width:${(part.bytes / partsTotal) * 100}%`}></i>{/if}{/each}</div>
        <div class="legend">
          {#each kinds as kind}
            <div><i class={kind.tone}></i><b>{kind.label}</b><small>{count(kind.count)} 条 · {size(kind.bytes)}</small></div>
          {/each}
          <div><i class="trash"></i><b>回收站</b><small>{count(overview.trashed.count)} 条 · {size(overview.trashed.bytes)}</small></div>
          <div><i class="snap"></i><b>快照</b><small>{snapshots?.items.length ?? 0} 份 · {size(overview.snapshots.bytes)}</small></div>
          <div><i class="temp"></i><b>临时文件</b><small>{count(overview.exports.count + overview.leftovers.count)} 个 · {size(overview.exports.bytes + overview.leftovers.bytes)}</small></div>
        </div>
      {/if}
    </div></div>

    <div class="group"><span class="group-title split">垃圾清理<em>清理的条目先进回收站，7 天内可以恢复；收藏的不会被清理</em></span><div class="card">
      <div class="setting">
        <span class="setting-copy"><strong>失效的文件条目</strong><small>原文件已移走或删除的条目。{overview?.files.checked_ms ? `上次检查：${shortTime(overview.files.checked_ms)}` : "还没检查过"}{overview?.files.partial ? `；另有 ${overview.files.partial} 条部分失效，其余文件还能用，不清理` : ""}</small>
          {#if notices.missing}<small class="ok">{notices.missing}</small>{/if}{#if errors.missing}<small class="error">{errors.missing}</small>{/if}</span>
        <span class="num"><RollingNumber value={overview?.files.missing ?? 0} /> 条</span>
        <button type="button" class="ghost" disabled={!!busy || disabled || !overview} onclick={() => act("missing", async () => { await ipc.checkFiles(); return "检查完了"; })}>{busy === "missing" ? "正在检查…" : "重新检查"}</button>
        <ConfirmButton label="清理" confirmLabel={`确认清理 ${overview?.files.missing ?? 0} 条`} disabled={!!busy || disabled || !overview?.files.missing}
          action={() => ipc.cleanMissing()} flyTo={() => trashTarget} onlanded={landed} />
      </div>

      <div class="setting">
        <span class="setting-copy"><strong>截图类图片</strong><small>没有源文件、存在历史库里的图片，共 {count(overview?.images.all.count ?? 0)} 张 · {size(overview?.images.all.bytes ?? 0)}</small>
          {#if notices.images}<small class="ok">{notices.images}</small>{/if}{#if errors.images}<small class="error">{errors.images}</small>{/if}</span>
        <select bind:value={imageRange} disabled={!overview}>
          <option value="30">30 天前的（{count(overview?.images.older_30.count ?? 0)} 张 · {size(overview?.images.older_30.bytes ?? 0)}）</option>
          <option value="90">90 天前的（{count(overview?.images.older_90.count ?? 0)} 张 · {size(overview?.images.older_90.bytes ?? 0)}）</option>
          <option value="all">全部（{count(overview?.images.all.count ?? 0)} 张 · {size(overview?.images.all.bytes ?? 0)}）</option>
        </select>
        <button type="button" class="ghost" disabled={!overview?.images.all.count} onclick={() => { view = "images"; }}>手动挑选</button>
        <ConfirmButton label="清理" confirmLabel={`确认清理 ${imageChoice?.count ?? 0} 张`} disabled={!!busy || disabled || !imageChoice?.count}
          action={() => ipc.cleanImages(imageRange === "all" ? null : Number(imageRange))} flyTo={() => trashTarget} onlanded={landed} />
      </div>

      <div class="setting">
        <span class="setting-copy"><strong>临时导出的图片</strong><small>“粘贴为文件”时临时生成的 PNG，粘贴完就没用了。直接删除，不进回收站；10 分钟内生成的先留着</small>
          {#if notices.exports}<small class="ok">{notices.exports}</small>{/if}{#if errors.exports}<small class="error">{errors.exports}</small>{/if}</span>
        <span class="num">{count(overview?.exports.count ?? 0)} 个<small>{size(overview?.exports.bytes ?? 0)}</small></span>
        <button type="button" disabled={!!busy || disabled || !overview?.exports.count} onclick={() => act("exports", async () => { const removed = await ipc.cleanExports(); return `已删除 ${removed.count} 个 · ${size(removed.bytes)}`; })}>删除</button>
      </div>

      {#if overview?.leftovers.count}
        <div class="setting">
          <span class="setting-copy"><strong>恢复时留下的旧库副本</strong><small>从快照恢复时，原来的历史库另存了一份；确认恢复后的历史没问题就可以删除</small>
            {#if errors.leftovers}<small class="error">{errors.leftovers}</small>{/if}</span>
          <span class="num">{count(overview.leftovers.count)} 个<small>{size(overview.leftovers.bytes)}</small></span>
          <button type="button" disabled={!!busy || disabled} onclick={() => act("leftovers", async () => { const removed = await ipc.cleanLeftovers(); return `已删除 ${removed.count} 个`; })}>删除</button>
        </div>
      {/if}

      <div class="setting">
        <span class="setting-copy"><strong>压缩历史库</strong><small>{compactBlocked ? `磁盘剩余空间不够：压缩需要约 ${size(compactNeeds)}` : `删除过的内容占的空间要压缩后才还给磁盘。需要约 ${size(compactNeeds)} 剩余空间，期间暂停记录几秒`}</small>
          {#if notices.compact}<small class="ok">{notices.compact}</small>{/if}{#if errors.compact}<small class="error">{errors.compact}</small>{/if}</span>
        <span class="num">约 {size(overview?.reclaimable_bytes ?? 0)}<small>可回收</small></span>
        <button type="button" disabled={!!busy || disabled || !overview || compactBlocked} onclick={() => act("compact", async () => `已压缩，省下 ${size(await ipc.compactHistory())}`)}>{busy === "compact" ? "正在压缩…" : "压缩"}</button>
      </div>
    </div></div>

    <div class="group"><span class="group-title split">回收站<em>保留 7 天</em></span><div class="card">
      <div class="setting">
        <span class="setting-copy"><strong><span class="trash-count" bind:this={trashTarget}><RollingNumber bind:this={trashNumber} value={overview?.trashed.count ?? 0} /></span> 条</strong><small>删除和清理的条目先放在这里，7 天后彻底删除。面板页脚的“回收站”也能打开这里</small></span>
        <button type="button" onclick={() => { view = "trash"; }}>查看</button>
      </div>
    </div></div>

    <div class="group"><span class="group-title split">自动快照<em>保留 {snapshots?.keep ?? 7} 份 · 上限 {maxMb >= 1024 ? `${maxMb / 1024} GB` : `${maxMb} MB`}</em></span><div class="card">
      <div class="setting">
        <span class="setting-copy"><strong>{latestSnapshot ? `最近一份：${shortTime(latestSnapshot.created_ms)}` : "还没有快照"}</strong><small>{snapshots?.items.length ?? 0} 份，共 {size(overview?.snapshots.bytes ?? 0)}。出问题时可以把历史恢复到其中任何一份</small></span>
        <button type="button" onclick={() => { view = "snapshots"; }}>全部快照</button>
      </div>
    </div></div>

    <div class="group later"><span class="group-title">备份</span><div class="card">
      <div class="setting">
        <span class="setting-copy"><strong>导出 / 导入备份</strong><small>历史库、图标和清单打成一个文件，不加密；导入前先校验。下个版本（beta.2）开放</small></span>
        <button type="button" disabled>导出…</button><button type="button" disabled>导入…</button>
      </div>
    </div></div>
  {/if}
</section>

<style>
  .crumb { display: grid; gap: 4px; }
  .crumb h2 { margin: 0; }
  .back { justify-self: start; min-height: 0; padding: 0; border: 0; background: transparent; color: var(--settings-accent); font-size: 11px; }
  .banner { margin: 0; padding: 10px 14px; border: 1px solid var(--settings-accent); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; line-height: 1.6; }
  .banner.error { border-color: var(--state-critical); color: var(--state-critical); }
  .split { display: flex; justify-content: space-between; gap: 12px; }
  .split em { font-style: normal; font-weight: 400; font-size: 10px; letter-spacing: 0; }
  .note { margin: 0; color: var(--settings-muted); font-size: 11px; }
  .usage { display: grid; gap: 12px; padding: 16px; }
  .usage-top { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  .usage-top strong { font: 600 22px var(--font-display); }
  .usage-top span { color: var(--settings-muted); font-size: 11px; text-align: right; }
  .meter { height: 10px; display: flex; overflow: hidden; border-radius: var(--radius-sm); background: var(--settings-selected); }
  .meter i { display: block; height: 100%; min-width: 2px; }
  .legend { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px 14px; font-size: 11px; }
  .legend div { display: grid; grid-template-columns: 10px minmax(0, 1fr); column-gap: 7px; align-items: center; }
  .legend i { width: 10px; height: 10px; border-radius: var(--radius-sm); }
  .legend b { font-weight: 600; }
  .legend small { grid-column: 2; color: var(--settings-muted); font-size: 10px; }
  .text { background: var(--settings-accent); }
  .rich { background: color-mix(in srgb, var(--settings-accent) 55%, var(--settings-card)); }
  .image { background: var(--state-caution); }
  .file { background: var(--settings-muted); }
  .trash { background: color-mix(in srgb, var(--state-caution) 45%, var(--settings-card)); }
  .snap { background: color-mix(in srgb, var(--settings-muted) 45%, var(--settings-card)); }
  .temp { background: var(--state-critical); }
  .num { min-width: 70px; color: var(--settings-text); font: 600 13px var(--font-mono); text-align: right; white-space: nowrap; }
  .num small { display: block; color: var(--settings-muted); font: 10px var(--font-ui); }
  .setting-copy small.ok { color: var(--settings-accent); }
  .setting-copy small.error { color: var(--state-critical); }
  .setting select { min-width: 0; max-width: 220px; }
  button { min-height: 28px; flex: none; padding: 3px 12px; border: 1px solid var(--settings-line); border-radius: var(--radius-sm); background: var(--settings-selected); font-size: 11px; white-space: nowrap; }
  button.ghost { background: transparent; }
  button:disabled { opacity: .6; }
  .later { opacity: .55; }
  .trash-count { display: inline-block; }
</style>
