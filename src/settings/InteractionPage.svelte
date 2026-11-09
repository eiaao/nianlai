<script lang="ts">
  // “交互”分区（M2-8）：面板呼出位置（D12）和鼠标点条目的行为（G09）。改动即时生效，下次呼出 / 下次点击就按新设置。
  import type { AppSettings, ClickAction, InteractionSettings, Placement, SettingsUpdate } from "../lib/ipc";

  let { interaction, disabled, onsave }: {
    interaction: InteractionSettings | null;
    disabled: boolean;
    onsave: (update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
  } = $props();

  const off = $derived(!interaction || disabled);
  const placement = $derived(interaction?.placement ?? "caret");

  const places: { id: Placement; label: string; hint: string }[] = [
    { id: "caret", label: "输入光标旁", hint: "找不到光标时退回鼠标旁" },
    { id: "mouse", label: "鼠标旁", hint: "" },
    { id: "center", label: "屏幕中央", hint: "" },
  ];
  const actions: { id: ClickAction; label: string }[] = [
    { id: "paste", label: "粘贴" },
    { id: "copy", label: "只复制" },
    { id: "text", label: "纯文本粘贴" },
    { id: "none", label: "不做额外操作" },
  ];

  const setPlacement = (value: Placement, label: string) => onsave({ interaction: { placement: value } }, `呼出位置：${label}`);
  const setClick = (value: ClickAction) => onsave({ interaction: { click: value } }, "单击条目的行为已保存");
  const setMiddle = (value: ClickAction) => onsave({ interaction: { middle_click: value } }, "中键点击条目的行为已保存");
</script>

<section class="page">
  <div><h2>交互</h2><p class="page-intro">面板在哪里出现、鼠标点一下做什么。键盘的用法不受这里影响。</p></div>

  <div class="group">
    <span class="group-title">呼出位置<em class="title-note">总在当前光标所在的屏幕</em></span>
    <div class="card">
      <div class="places" role="radiogroup" aria-label="呼出位置">
        {#each places as place (place.id)}
          <button class="place" type="button" role="radio" aria-checked={placement === place.id} disabled={off} onclick={() => setPlacement(place.id, place.label)}>
            <span class="screen-mini" aria-hidden="true">
              {#if place.id === "caret"}
                <span class="c" style="left:40px;top:18px"></span><span class="p" style="left:36px;top:30px"></span>
              {:else if place.id === "mouse"}
                <span class="m" style="left:70px;top:16px">↖</span><span class="p" style="left:78px;top:24px"></span>
              {:else}
                <span class="p" style="left:calc(50% - 17px);top:22px"></span>
              {/if}
            </span>
            {place.label}<small>{place.hint}</small>
          </button>
        {/each}
      </div>
    </div>
  </div>

  <div class="group">
    <span class="group-title">鼠标</span>
    <div class="card">
      <label class="setting"><span class="setting-copy"><strong>单击条目</strong><small>键盘的 Enter 不受影响</small></span>
        <select value={interaction?.click ?? "paste"} disabled={off} onchange={(event) => setClick(event.currentTarget.value as ClickAction)}>
          {#each actions as action (action.id)}<option value={action.id}>{action.label}</option>{/each}
        </select>
      </label>
      <label class="setting"><span class="setting-copy"><strong>中键点击条目</strong><small>按下鼠标滚轮</small></span>
        <select value={interaction?.middle_click ?? "text"} disabled={off} onchange={(event) => setMiddle(event.currentTarget.value as ClickAction)}>
          {#each actions as action (action.id)}<option value={action.id}>{action.label}</option>{/each}
        </select>
      </label>
    </div>
  </div>
</section>

<style>
  .title-note { margin-left: 10px; font: 400 10px var(--font-ui); font-style: normal; }
  .places { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; padding: 14px 16px; }
  .place { display: grid; align-content: start; gap: 8px; padding: 10px; border: 1px solid var(--settings-line); border-radius: 3px; background: transparent; color: var(--settings-text); text-align: center; font-size: 12px; }
  .place[aria-checked="true"] { border-color: var(--settings-accent); box-shadow: 0 0 0 1px var(--settings-accent); font-weight: 600; }
  .place small { min-height: 1.4em; color: var(--settings-muted); font-size: 10px; font-weight: 400; }
  .screen-mini { position: relative; height: 70px; border: 1px solid var(--settings-line); border-radius: 2px; background: var(--settings-bg); }
  .screen-mini .p { position: absolute; width: 34px; height: 24px; border: 1px solid var(--settings-accent); background: color-mix(in srgb, var(--settings-accent) 18%, var(--settings-card)); }
  .screen-mini .c { position: absolute; width: 1px; height: 10px; background: var(--settings-text); }
  .screen-mini .m { position: absolute; font-size: 11px; line-height: 1; }
  @media (max-width: 720px) { .places { grid-template-columns: 1fr; } }
</style>
