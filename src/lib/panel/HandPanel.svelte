<script lang="ts">
  // 手牌的唯一摆放入口：按姿态（扇面 / 摊开）算出每张牌的目标位置和尺寸，同一个以 id 为 key 的元素在两种姿态之间补间。
  import { untrack } from "svelte";
  import type { PanelState } from "../state.svelte";
  import { reducedMotion } from "../preferences";
  import HandCard from "./HandCard.svelte";
  import SearchBox from "./SearchBox.svelte";
  import PreviewOverlay from "./PreviewOverlay.svelte";
  import Preview from "./Preview.svelte";
  import RecordBanner from "./RecordBanner.svelte";
  import Footer from "./Footer.svelte";
  import {
    ARC, backBox, CARD_H, cardLabel, COMP_H, DECK_H, DECK_W, DECK_X, DECK_Y, FAN, fanBox, HAND_TOP, HAND_W, HEAD_H, LIFT_GAP, liftHeight,
    ORIGIN_X, ORIGIN_Y, PITCH, ROW_W, ROW_X, SPREAD_TOP, SPREAD_VIEW, SPREAD_W, type Box,
  } from "../cards";

  let { panel }: { panel: PanelState } = $props();
  const WINDOW_W = 960;
  const MORPH_MS = 240, MOVE_MS = 180, STAGGER_MS = 10;

  const spread = $derived(panel.pose === "spread");
  const view = { top: ORIGIN_Y + SPREAD_TOP, bottom: ORIGIN_Y + SPREAD_TOP + SPREAD_VIEW };

  // 姿态切换的那一段时间用 240ms + 逐张错峰 10ms（最多 8 张）；平时换牌、滚动用 180ms。
  let morphing = $state(false);
  let morphTimer: ReturnType<typeof setTimeout> | undefined;
  $effect.pre(() => {
    panel.pose;
    untrack(() => {
      morphing = true;
      clearTimeout(morphTimer);
      morphTimer = setTimeout(() => { morphing = false; }, MORPH_MS + 8 * STAGGER_MS + 120);
    });
    return () => clearTimeout(morphTimer);
  });
  const instant = () => panel.motion === "none" || reducedMotion();

  // 底板和搜索框的点击区域：摊开那一帧就加上；收回时等淡出（200ms）结束再撤，免得淡出途中点击漏到下面。
  let matHit = $state(false);
  let matTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const on = spread;
    clearTimeout(matTimer);
    if (on) matHit = true;
    else matTimer = setTimeout(() => { matHit = false; }, instant() ? 0 : 220);
    return () => clearTimeout(matTimer);
  });

  type Entry = { index: number; box: Box; keycap: string; z: number; order: number; clip: string };
  const entries = $derived.by((): Entry[] => {
    const items = panel.handSource;
    if (!spread) {
      const selected = panel.selected < panel.fanItems.length && !panel.dragging ? panel.selected : -1;
      const gap = panel.backItems.length ? LIFT_GAP : 12;
      return panel.fanItems.map((item, index) => ({
        index, order: index, clip: "",
        box: fanBox(index, selected, index === selected ? liftHeight(item, panel.kinds[index]) : undefined, gap),
        keycap: String(index + 1), z: index === selected ? 80 : 20 + index,
      }));
    }
    const layout = panel.spread;
    if (!layout) return [];
    return layout.rendered.map((index, order) => {
      const row = layout.rows[index];
      const y = view.top + row.y - layout.scroll;
      const digit = layout.visible.indexOf(index);
      const keycap = digit >= 0 && digit < FAN ? (panel.searching ? `Ctrl ${digit + 1}` : String(digit + 1)) : index === panel.selected ? "↵" : "";
      // 摊开时按可视区裁掉越界部分；形变过程中不裁（否则扇面牌会被切）。
      const top = Math.max(0, view.top - y), bottom = Math.max(0, y + row.h - view.bottom);
      const clip = !morphing && (top > 0.5 || bottom > 0.5) ? `inset(${top}px -24px ${bottom}px -24px)` : "";
      return { index, order, clip, box: { x: ORIGIN_X + ROW_X, y, w: ROW_W, h: row.h, r: 0 }, keycap, z: index === panel.selected ? 60 : 20 + Math.min(order, 30) };
    }).filter((entry) => items[entry.index]);
  });

  const heads = $derived(spread && panel.spread ? panel.spread.heads.map((head) => ({ ...head, y: view.top + head.y - panel.spread!.scroll }))
    .filter((head) => head.y >= view.top - 2 && head.y + HEAD_H <= view.bottom + 2) : []);
  // 扇面：时间段变化处的轻分隔（牌脚下方），搜索时不出现。
  const separators = $derived(spread ? [] : panel.fanItems.flatMap((item, index) => {
    const group = panel.groupOf(item);
    return index === 0 || group !== panel.groupOf(panel.fanItems[index - 1]) ? [{ index, group }] : [];
  }));
  const rest = $derived(Math.max(0, panel.handSource.length - FAN));

  // 拖动松手：还在手牌 / 一列里算取消。
  const inside = (x: number, y: number) => spread
    ? x >= ORIGIN_X && x <= ORIGIN_X + SPREAD_W && y >= ORIGIN_Y && y <= ORIGIN_Y + COMP_H
    : x >= ORIGIN_X - 20 && x <= DECK_X + DECK_W && y >= HAND_TOP - 28 && y <= HAND_TOP + 216;

  // 新出现的行（扇面 7 张以外）从下方 12px 淡入：延迟 90ms 起，每条 +14ms。不再匹配的原地缩到 0.96 淡出。
  function rowIn(_node: HTMLElement, { order }: { order: number }) {
    if (instant()) return { duration: 100, css: (t: number) => `opacity:${t}` };
    if (!morphing) return { duration: 140, css: (t: number) => `opacity:${t}` };
    return { delay: 90 + Math.min(order, 10) * 14, duration: 200, css: (t: number, u: number) => `opacity:${t};transform:translateY(${u * 12}px)` };
  }
  function cardOut(_node: HTMLElement) {
    if (instant()) return { duration: 100, css: (t: number) => `opacity:${t}` };
    return { duration: 160, css: (t: number) => `opacity:${t};transform:scale(${0.96 + 0.04 * t})`, };
  }

  // 按键提示只有一处：扇面在拈起那张旁边，摊开在一列底部；只列当前能用的键。
  let hintWidth = $state(300);
  const lifted = $derived(!spread && panel.selected < panel.fanItems.length ? entries[panel.selected]?.box ?? null : null);
  const hintPlace = $derived.by(() => {
    if (spread) return { x: ORIGIN_X + ROW_X + 4, y: ORIGIN_Y + COMP_H - 38 };
    const box = lifted;
    if (!box) return { x: ORIGIN_X + HAND_W / 2 - hintWidth / 2, y: HAND_TOP - 60 };
    const fits = (side: number) => side > 0 ? box.x + box.w + 12 + hintWidth <= WINDOW_W - 8 : box.x - 12 - hintWidth >= 8;
    let side = panel.selected <= 3 ? 1 : -1;
    if (!fits(side)) side = fits(-side) ? -side : 0;
    if (!side) return { x: Math.max(8, Math.min(WINDOW_W - 8 - hintWidth, box.x + box.w / 2 - hintWidth / 2)), y: Math.max(4, box.y - 40) };
    return { x: side > 0 ? box.x + box.w + 12 : box.x - 12 - hintWidth, y: box.y + box.h / 2 - 16 };
  });
  const toast = $derived(!!(panel.feedback || panel.notice || panel.undo || panel.message || panel.holdDelete?.phase === "hint"));
  const showHint = $derived(panel.handSource.length > 0 && !panel.previewOpen && !panel.dragging && !(spread && toast));

  let wheelSum = 0;
  function wheel(event: WheelEvent) {
    wheelSum += event.deltaY || event.deltaX;
    if (Math.abs(wheelSum) < 50) return;
    const direction = wheelSum > 0 ? 1 : -1;
    wheelSum = 0;
    panel.wheel(direction);
  }
</script>

{#snippet keys(list: string[], label: string)}<span>{#each list as key}<kbd>{key}</kbd>{/each}{label}</span>{/snippet}

<section class="hand-view" class:spread class:searching={panel.searching} aria-label="手牌" onwheel={wheel}>
  {#if panel.interactive || (panel.warming && !panel.visible)}
    <div class="banner-float"><RecordBanner {panel} /></div>

    <!-- 摊开时垫的纸色底板（616 × 514）；姿态切换的那一帧就成为命中区域。 -->
    <div class="mat" class:show={spread} data-hit={spread || matHit ? "" : undefined} style={`translate:${ORIGIN_X}px ${ORIGIN_Y}px;width:${SPREAD_W}px;height:${COMP_H}px`}></div>

    <!-- 真实输入框始终在 DOM 里（焦点和输入法要靠它），扇面时看不见。 -->
    {#if panel.inputSurface}
      <div class="search-float" class:show={spread} data-hit={spread || matHit ? "" : undefined} style={`translate:${ORIGIN_X + ROW_X}px ${ORIGIN_Y + 8}px`}><SearchBox {panel} /></div>
    {/if}

    {#if spread}
      <div class="position top" data-hit style={`right:${WINDOW_W - ORIGIN_X - SPREAD_W + 12}px;top:${ORIGIN_Y + 12}px`}>
        {#if !panel.handSource.length}{panel.searching ? "一条也没找到" : "还没有复制过东西"}
        {:else if panel.searching}找到 <b>{panel.handSource.length}</b> 条 · 第 {panel.selected + 1} 条
        {:else}第 <b>{panel.selected + 1}</b> 条，共 {panel.handSource.length} 条{/if}
      </div>
      {#each heads as head (`${head.name}:${head.index}`)}
        <div class="group-head" class:late={morphing} style={`translate:${ORIGIN_X + ROW_X}px ${head.y}px;width:${ROW_W}px`}><b>{head.name}</b><span>{head.count} 条</span></div>
      {/each}
      {#if !panel.handSource.length}
        <div class="blank-row" data-hit style={`translate:${ORIGIN_X + ROW_X}px ${view.top}px;width:${ROW_W}px`}>
          {panel.searching ? `没找到“${panel.query}”，换个词试试` : "还没有复制过东西"}
        </div>
      {/if}
    {:else}
      {#if panel.backItems.length}
        <button class="tag" type="button" tabindex="-1" data-hit style={`translate:${ORIGIN_X - 12}px ${HAND_TOP - 30}px`}
          onpointerdown={(event) => event.preventDefault()} onclick={() => panel.switchHandList(panel.handList === "favorites" ? "recent" : "favorites")}>
          <kbd>{panel.handList === "favorites" ? "↓" : "↑"}</kbd>{panel.handList === "favorites" ? "最近" : "常用"}
        </button>
        {#each panel.backItems as item, index (item.id)}
          {@const box = backBox(index)}
          <button class="back-head" type="button" tabindex="-1" data-hit style={`translate:${box.x}px ${box.y}px;rotate:${box.r}deg;z-index:${index + 1}`}
            onpointerdown={(event) => event.preventDefault()} onclick={() => panel.switchHandList(panel.handList === "favorites" ? "recent" : "favorites")}>
            {#if panel.handList === "recent"}<i>常</i>{/if}<span>{cardLabel(item)}</span>
          </button>
        {/each}
      {/if}
      {#each separators as separator (separator.index)}
        <div class="time-sep" style={`translate:${ORIGIN_X + separator.index * PITCH + 4}px ${HAND_TOP + CARD_H + ARC + 8}px`}>{separator.group}</div>
      {/each}
      {#if !panel.handSource.length}
        <div class="blank-card" data-hit style={`translate:${ORIGIN_X + 3 * PITCH}px ${HAND_TOP - 24}px`}>
          <strong>复制点什么，<br>它就会出现在这里</strong><span>文字、图片、文件都可以</span>
        </div>
      {/if}
      {#if rest > 0}
        <button class="deck" type="button" tabindex="-1" data-hit style={`translate:${DECK_X}px ${DECK_Y}px;width:${DECK_W}px;height:${DECK_H}px`}
          onpointerdown={(event) => event.preventDefault()} onclick={() => panel.spreadOpen(FAN)} aria-label="摊开">
          {#each { length: Math.min(3, Math.ceil(rest / 12)) } as _, layer}<i style={`translate:${(3 - layer) * 3}px ${-(3 - layer) * 3}px`}></i>{/each}
          <span class="deck-top"><b>拈</b><strong>还有 {rest} 张</strong><small><kbd>{panel.handList === "favorites" ? "PgDn" : "↓"}</kbd>摊开</small></span>
        </button>
      {/if}
    {/if}

    {#each entries as entry (panel.handSource[entry.index].id)}
      {@const item = panel.handSource[entry.index]}
      <div class="slot" style={`z-index:${entry.z}`} in:rowIn={{ order: entry.order }} out:cardOut>
        <HandCard {panel} {item} index={entry.index} kind={panel.kinds[entry.index]} box={entry.box} flat={spread}
          selected={entry.index === panel.selected} keycap={entry.keycap} clip={entry.clip} {inside}
          duration={morphing ? MORPH_MS : MOVE_MS} delay={morphing ? Math.min(entry.order, 8) * STAGGER_MS : 0} />
      </div>
    {/each}

    {#if panel.stack.length}
      <div class="stack" data-hit>
        <span>已选 <b>{panel.stack.length}</b> 张</span>
        {#each panel.stack.slice(-6) as item, index (item.id)}<div><em>{panel.stack.length - Math.min(6, panel.stack.length) + index + 1}</em>{cardLabel(item)}</div>{/each}
      </div>
    {/if}

    {#if showHint}
      <div class="hint" class:flat={spread} data-hit bind:clientWidth={hintWidth} style={`translate:${hintPlace.x}px ${hintPlace.y}px`}>
        {#if panel.stack.length}
          {#if panel.heldModifier === "ctrl"}{@render keys(["Ctrl", "↵"], "按纯文本一起粘贴")}{@render keys(["Ctrl", "Shift", "↵"], "逐张放")}
          {:else}{@render keys(["↵"], "一起粘贴")}{@render keys(["Ctrl", "Shift", "↵"], "逐张放")}{#if !panel.searching}{@render keys(["Backspace"], "放回一张")}{/if}{/if}
        {:else if panel.heldModifier === "shift"}
          {@render keys(["Shift", "↵"], "只复制这一张，不粘贴")}
        {:else if panel.heldModifier === "ctrl"}
          {#if panel.searching}{@render keys(["Ctrl", "1–7"], "粘贴对应一条")}{/if}{@render keys(["Ctrl", "↵"], "按纯文本粘贴")}{@render keys(["Ctrl", "D"], panel.currentItem?.favorite ? "移出常用" : "加入常用")}{#if panel.undo}{@render keys(["Ctrl", "Z"], "撤销删除")}{/if}
        {:else if spread}
          {@render keys(["↑", "↓"], "上一条 / 下一条")}{@render keys(["↵"], "粘贴")}{#if panel.searching}{@render keys(["Ctrl"], "+数字 粘贴")}{:else}{@render keys(["1–7"], "粘贴")}{@render keys(["空格"], "多选")}{/if}{@render keys(["Tab"], "看全")}{@render keys(["Esc"], panel.searching ? "清空搜索" : "收回")}
        {:else}
          {@render keys(["↵"], "粘贴")}{@render keys(["空格"], "多选")}{@render keys(["Tab"], "看全")}
        {/if}
      </div>
    {/if}

    {#if !spread && !toast}
      <div class="position bottom" data-hit style={`left:${ORIGIN_X + HAND_W / 2}px;top:${HAND_TOP + 216}px`}>
        {#if !panel.handSource.length}还没有复制过东西
        {:else if panel.handList === "favorites"}<b>常用</b> {panel.fanItems.length} 张{#if rest} · 共 {panel.handSource.length} 张{/if} · 顺序固定
        {:else}<b>最近</b> {panel.fanItems.length} 张 · 共 {panel.handSource.length} 张{/if}{#if panel.arrivals} · 新来 {panel.arrivals} 张{/if}
      </div>
    {/if}
    {#if toast}
      <div class="toast" class:flat={spread} style={spread ? `left:${ORIGIN_X + ROW_X + 4}px;top:${ORIGIN_Y + COMP_H - 44}px` : `left:${ORIGIN_X + HAND_W / 2}px;top:${HAND_TOP + 212}px`}><Footer {panel} /></div>
    {/if}
    {#if !spread}
      <button class="trash-link" type="button" tabindex="-1" data-hit title="在设置里查看回收站" style={`left:${DECK_X}px;top:${HAND_TOP + 220}px`}
        onpointerdown={(event) => event.preventDefault()} onclick={panel.openTrash}>回收站</button>
    {/if}
    <PreviewOverlay {panel} />
  {:else if panel.interaction === "passive"}
    <div class="passive-card" data-hit><Preview {panel} item={panel.currentItem} compact /></div>
    <div class="toast passive" style={`left:480px;top:${HAND_TOP + 160}px`}><Footer {panel} /></div>
  {/if}
</section>

<style>
  .hand-view { position: relative; width: 100%; height: 100%; color: var(--hand-ink); pointer-events: none; --accent:var(--hand-seal); --text:var(--hand-ink); --muted:var(--hand-ink-2); --faint:var(--hand-ink-3); --line:var(--hand-line-strong); --line-soft:var(--hand-line); --panel:var(--hand-card); --panel-raised:var(--hand-paper); --preview:var(--hand-card); }
  .hand-view :global([data-hit]) { pointer-events: auto; }
  .hand-view > :is(.mat, .search-float, .position, .group-head, .blank-row, .tag, .back-head, .time-sep, .blank-card, .deck, .slot, .hint) { position: absolute; left: 0; top: 0; }
  .banner-float { position: absolute; z-index: 150; left: 50%; top: 52px; width: 520px; transform: translateX(-50%); }
  .mat { z-index: 4; border-radius: 12px; background: var(--hand-paper); box-shadow: var(--hand-pop); opacity: 0; transition: opacity 200ms; }
  .mat.show { opacity: 1; }
  .search-float { z-index: 130; width: 320px; height: 32px; opacity: 0; transition: opacity 160ms; }
  .search-float.show { opacity: 1; }
  .position { z-index: 100; height: 24px; padding: 0 10px; border-radius: 12px; background: var(--hand-paper); box-shadow: 0 0 0 1px var(--hand-line); color: var(--hand-ink-3); font-size: 12px; line-height: 24px; white-space: nowrap; }
  .position.top { left: auto; }
  .position.bottom { translate: -50% 0; }
  .position b { color: var(--hand-ink-2); font-weight: 600; }
  .group-head { z-index: 12; height: 28px; display: flex; align-items: flex-end; gap: 8px; padding: 0 8px 6px; color: var(--hand-ink-3); font-size: 12px; line-height: 16px; white-space: nowrap; }
  .group-head::after { content: ""; flex: 1; height: 1px; margin-bottom: 7px; background: var(--hand-line); }
  .group-head b { color: var(--hand-ink-2); font-weight: 600; }
  .group-head.late { animation: appear 160ms linear 140ms backwards; }
  .blank-row { z-index: 20; height: 64px; display: grid; place-items: center; border-radius: 8px; outline: 1.5px dashed var(--hand-line-strong); outline-offset: -1.5px; color: var(--hand-ink-2); font-size: 13px; }
  .tag { z-index: 60; height: 26px; display: flex; align-items: center; gap: 6px; padding: 0 10px 0 4px; border: 0; border-radius: 13px; background: var(--hand-paper); box-shadow: 0 0 0 1px var(--hand-line); color: var(--hand-ink-2); font-size: 12px; }
  .tag:hover { color: var(--hand-ink); }
  .hand-view > .tag { transform: translateX(-100%); }
  .back-head { width: 136px; height: 30px; display: flex; align-items: center; gap: 6px; padding: 0 10px; overflow: hidden; border: 0; border-radius: 10px 10px 0 0; background: var(--hand-card-back); box-shadow: 0 0 0 1px var(--hand-line); color: var(--hand-ink-2); font-size: 12px; text-align: left; transform-origin: 50% 100%; }
  .back-head i { width: 8px; height: 8px; flex: none; border-radius: 2px; background: var(--hand-seal); font-size: 0; }
  .back-head span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .time-sep { z-index: 9; display: flex; align-items: center; gap: 4px; color: var(--hand-ink-3); font-size: 12px; line-height: 16px; white-space: nowrap; }
  .time-sep::before { content: ""; width: 1px; height: 12px; background: var(--hand-line-strong); }
  .blank-card { z-index: 20; width: 150px; height: 190px; display: grid; align-content: center; gap: 12px; padding: 16px; border-radius: 10px; outline: 1.5px dashed var(--hand-line-strong); outline-offset: -1.5px; background: var(--hand-card); box-shadow: var(--hand-pop); text-align: center; }
  .blank-card strong { font: 500 15px/22px var(--font-ui); }
  .blank-card span { color: var(--hand-ink-3); font-size: 12px; }
  .deck { z-index: 10; padding: 0; border: 0; background: transparent; color: var(--hand-ink); }
  .deck > i { position: absolute; inset: 0; border-radius: 9px; background: var(--hand-card-back); box-shadow: 0 0 0 1px var(--hand-line); }
  .deck-top { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; border-radius: 9px; background: var(--hand-card-back); box-shadow: 0 0 0 1px var(--hand-line-strong), var(--hand-card-shadow); }
  .deck-top b { width: 26px; height: 26px; display: grid; place-items: center; border-radius: 5px; box-shadow: inset 0 0 0 1.5px var(--hand-seal); color: var(--hand-seal); font: 600 13px/1 var(--font-display); }
  .deck-top strong { font-size: 13px; font-weight: 600; }
  .deck-top small { display: flex; align-items: center; gap: 4px; color: var(--hand-ink-2); font-size: 12px; }
  .slot { width: 0; height: 0; }
  .slot:has(:global(.dragging)) { z-index: 400 !important; }
  .stack { position: absolute; z-index: 110; left: 8px; bottom: 13px; width: 96px; display: grid; gap: 4px; }
  .stack > span { color: var(--hand-ink-2); font-size: 12px; }
  .stack > span b { color: var(--hand-seal); }
  .stack > div { height: 24px; display: flex; align-items: center; gap: 5px; padding: 0 6px; overflow: hidden; border-radius: 6px; background: var(--hand-paper); box-shadow: 0 0 0 1px var(--hand-line); font-size: 12px; white-space: nowrap; }
  .stack em { color: var(--hand-ink-3); font-style: normal; }
  .hint { z-index: 120; height: 32px; display: flex; align-items: center; gap: 12px; padding: 0 12px; border-radius: 16px; background: var(--hand-paper); box-shadow: var(--hand-pop); color: var(--hand-ink-2); font-size: 12px; white-space: nowrap; }
  .hint.flat { background: transparent; box-shadow: none; padding: 0 4px; }
  .hint span { display: inline-flex; align-items: center; gap: 4px; }
  kbd { min-width: 20px; height: 20px; display: inline-flex; align-items: center; justify-content: center; padding: 0 5px; border-radius: 4px; background: var(--hand-key); box-shadow: inset 0 0 0 1px var(--hand-key-line), inset 0 -1px 0 var(--hand-key-line); color: var(--hand-ink); font: 600 12px/1 var(--font-ui); }
  .toast { position: absolute; z-index: 140; translate: -50% 0; max-width: 720px; }
  .toast.flat { translate: none; max-width: 600px; }
  .trash-link { position: absolute; z-index: 100; height: 24px; padding: 0 6px; border: 0; background: transparent; color: var(--hand-ink-3); font-size: 12px; }
  .trash-link:hover { color: var(--hand-ink); }
  .passive-card { position: absolute; left: 50%; top: 40px; width: 520px; max-height: 360px; padding: 20px; overflow: auto; transform: translateX(-50%); border-radius: 12px; background: var(--hand-card); box-shadow: var(--hand-pop); }
  @keyframes appear { from { opacity: 0; } to { opacity: 1; } }
  :global(html[data-motion="none"]) .group-head.late, :global(html[data-reduce-motion="true"]) .group-head.late { animation-delay: 0ms; animation-duration: 100ms; }
</style>
