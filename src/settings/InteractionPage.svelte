<script lang="ts">
  import { keyLabel } from "../lib/platform";
  // 操作习惯只负责鼠标和旧内容顺序；呼出位置已移到通用页，存储字段保持原样。
  import SaveMark, { type SaveMarkState } from "./SaveMark.svelte";
  import type { AppSettings, ClickAction, InteractionSettings, SettingsUpdate } from "../lib/ipc";

  let { interaction, reuseToTop, disabled, onsave, saveState }: {
    interaction: InteractionSettings | null;
    reuseToTop: boolean;
    disabled: boolean;
    onsave: (key: string, update: SettingsUpdate, message: string) => Promise<AppSettings | null>;
    saveState: (key: string) => SaveMarkState;
  } = $props();

  const off = $derived(!interaction || disabled);
  const actions: { id: ClickAction; label: string }[] = [
    { id: "paste", label: "粘贴" },
    { id: "copy", label: "只复制" },
    { id: "text", label: "按纯文本粘贴" },
    { id: "none", label: "不做什么" },
  ];
</script>

<section class="page habits-page">
  <div><h2>操作习惯</h2><p class="page-intro">用鼠标时的习惯。键盘怎么按不受影响。</p></div>

  <div class="mouse-figure">
    <svg viewBox="0 0 44 64" aria-hidden="true"><rect x="2" y="2" width="40" height="60" rx="20" fill="var(--hand-card)" stroke="var(--hand-ink-2)" stroke-width="1.5"/><path d="M2 26h40M22 2v24" stroke="var(--hand-ink-2)" stroke-width="1.5"/><path d="M22 2.75A19.25 19.25 0 0 0 2.75 22v3.25H22z" fill="var(--hand-seal)" opacity=".85"/><rect x="19" y="9" width="6" height="11" rx="3" fill="var(--hand-ink-2)"/></svg>
    <p>左键点牌，按滚轮是中键。两个可以分开设，比如左键直接粘贴，中键只复制。拖一张牌出去也是粘贴，这个不用设。</p>
  </div>

  <div class="group">
    <span class="group-title">鼠标</span>
    <div class="card">
      <label class="setting" data-save-row="habits.click"><span class="setting-copy"><strong>鼠标点一下牌</strong><small>键盘的 {keyLabel("Enter")} 不受影响。</small></span><SaveMark state={saveState("habits.click")} />
        <select data-setting="click" aria-label="鼠标点一下牌" value={interaction?.click ?? "paste"} disabled={off} onchange={(event) => onsave("habits.click", { interaction: { click: event.currentTarget.value as ClickAction } }, "鼠标点一下牌的行为已保存")}>
          {#each actions as action (action.id)}<option value={action.id}>{action.label}</option>{/each}
        </select>
      </label>
      <label class="setting" data-save-row="habits.middle-click"><span class="setting-copy"><strong>中键点一下</strong><small>按下鼠标滚轮。</small></span><SaveMark state={saveState("habits.middle-click")} />
        <select data-setting="middle-click" aria-label="中键点一下" value={interaction?.middle_click ?? "text"} disabled={off} onchange={(event) => onsave("habits.middle-click", { interaction: { middle_click: event.currentTarget.value as ClickAction } }, "中键点一下的行为已保存")}>
          {#each actions as action (action.id)}<option value={action.id}>{action.label}</option>{/each}
        </select>
      </label>
    </div>
  </div>

  <div class="group">
    <span class="group-title">旧内容顺序</span>
    <div class="card">
      <label class="setting reuse-row" data-save-row="habits.reuse-to-top"><span class="setting-copy"><strong>贴过的旧内容挪到最前</strong><small>下次呼出时它是第 1 张。收藏的顺序不受影响。</small><span class="reuse-figure" aria-hidden="true"><i>1</i><i>2</i><i class="changed">5</i><em>贴过之后 →</em><i class="changed">1</i><i>2</i><i>3</i></span></span><SaveMark state={saveState("habits.reuse-to-top")} />
        <input class="switch" data-setting="reuse-to-top" aria-label="贴过的旧内容挪到最前" type="checkbox" role="switch" checked={reuseToTop} disabled={disabled}
          onchange={(event) => onsave("habits.reuse-to-top", { history: { reuse_to_top: event.currentTarget.checked } }, event.currentTarget.checked ? "贴过的旧内容会挪到最前" : "贴过的旧内容保持原位置")} />
      </label>
    </div>
  </div>
</section>

<style>
  .mouse-figure { display: flex; align-items: center; gap: 16px; padding: 16px; border: 1px solid var(--hand-line); border-radius: 4px; background: var(--hand-card-back); }
  .mouse-figure svg { width: 44px; height: 64px; flex: none; }
  .mouse-figure p { margin: 0; color: var(--hand-ink-2); font-size: 12px; line-height: 1.7; }
  .reuse-row { align-items: flex-start; }
  .reuse-row .switch { margin-top: 4px; }
  .reuse-figure { display: flex; align-items: center; flex-wrap: wrap; gap: 4px; margin-top: 12px; }
  .reuse-figure i { width: 24px; height: 28px; display: grid; place-items: center; border: 1px solid var(--hand-line-strong); border-radius: 2px; background: var(--hand-card-back); color: var(--hand-ink-2); font: normal 12px var(--font-mono); }
  .reuse-figure i.changed { border-color: var(--hand-seal); color: var(--hand-seal); }
  .reuse-figure em { margin: 0 4px; color: var(--hand-ink-2); font-size: 12px; font-style: normal; }
  @media (forced-colors: active) {
    .mouse-figure, .reuse-figure i { border-color: CanvasText; }
  }
</style>
