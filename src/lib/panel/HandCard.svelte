<script lang="ts">
  // 一张牌 = 一个元素，两种姿态：扇面（竖牌）和摊开（横条）。位置、尺寸、旋转由 HandPanel 算好传进来，
  // 姿态切换时同一个元素补间 translate / rotate / 宽高，两层内容交叉淡入（对象恒常性）。
  import * as ipc from "../ipc";
  import type { HistoryItem, ImageView } from "../ipc";
  import type { PanelState } from "../state.svelte";
  import { type Box, type CardKind, colorValue, fileExt, firstLine, imageSize, KIND_NAME, linkParts, mailParts, relativeTime, sourceName } from "../cards";

  let { panel, item, index, kind, box, flat, selected, keycap, clip, delay, duration, inside }: {
    panel: PanelState; item: HistoryItem; index: number; kind: CardKind; box: Box; flat: boolean; selected: boolean;
    /** 键帽文字："1"…"7"、"Ctrl 1"…、"↵" 或空。 */
    keycap: string; clip: string; delay: number; duration: number;
    /** 松手位置是否还在手牌 / 一列里（在里面算取消、弹回）。 */
    inside: (x: number, y: number) => boolean;
  } = $props();

  const text = $derived(item.preview ?? "");
  const link = $derived(kind === "link" ? linkParts(text) : null);
  const color = $derived(kind === "color" ? colorValue(text) : null);
  const mail = $derived(kind === "mail" ? mailParts(text) : null);
  const lines = $derived(text.split("\n").filter((line) => line.trim()));
  const taken = $derived(panel.isTaken(item.id));
  const favorite = $derived(item.favorite);
  // 常用列表里每张都是常用，印记省掉；只在最近、搜索混排里出现。
  const showSeal = $derived(favorite && !(panel.handList === "favorites" && !panel.searching));
  const holding = $derived(panel.holdDelete?.id === item.id && panel.holdDelete.phase === "holding" ? panel.holdDelete : null);
  const meta = $derived(item.title ? item.title : `${sourceName(item)}${item.copied_ms ? ` · ${relativeTime(item.copied_ms, panel.openedAt)}` : ""}`);

  let image = $state<ImageView | null>(null);
  let imageFor = -1;
  $effect(() => {
    const id = kind === "image" ? item.id : -1;
    if (id === imageFor) return;
    imageFor = id;
    image = null;
    if (id < 0) return;
    void ipc.getImage(id, 320).then((result) => { if (imageFor === id) image = result; }).catch(() => {});
  });

  // ---------- 拖出去 ----------
  // 暂时关闭：牌拖出 960×540 窗口会被裁掉，方案（系统拖放 / 默认铺满工作区 / 拖动时临时扩窗）待定，
  // 见 internal 项目看板“待办候选”。关闭后单击照常粘贴，拖动不起作用。
  const DRAG_OUT = false;
  let drag = $state<{ x: number; y: number; out: boolean } | null>(null);
  let press: { x: number; y: number; id: number } | null = null;
  let dragged = false;
  function reset() {
    press = null; drag = null;
    panel.dragging = false;
  }
  function pointerDown(event: PointerEvent) {
    // 新的按下重新获得点击资格；上一次拖动即使没有补 click 也不会误吞这一次。
    dragged = false;
    if (event.button !== 0 || taken || panel.busy) return;
    event.preventDefault();
    if (!DRAG_OUT) return;
    press ={ x: event.clientX, y: event.clientY, id: event.pointerId };
    (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
  }
  function pointerMove(event: PointerEvent) {
    panel.pointerMoved();
    if (!press && !selected) panel.hoverSelect(index);
    if (!press || event.pointerId !== press.id) return;
    const x = event.clientX - press.x, y = event.clientY - press.y;
    if (!drag && Math.hypot(x, y) < 6) return;
    // 拖动开始：命中区域扩到整个窗口（Panel 负责上报），牌不会被窗口区域裁掉。
    if (!drag) panel.dragging = true;
    drag = { x, y, out: !inside(event.clientX, event.clientY) };
  }
  function pointerUp(event: PointerEvent) {
    if (!press || event.pointerId !== press.id) return;
    const wasDrag = !!drag;
    const out = wasDrag && !inside(event.clientX, event.clientY);
    reset();
    // 拖动过就吞掉随后的 click：同一次按下只取用一次。
    dragged = wasDrag;
    // 松在手牌 / 一列以外才粘贴；后端目前只能粘贴到原光标处。松回里面算取消，牌弹回原位。
    if (out) panel.chooseByMouse(index, event);
  }
  function click(event: MouseEvent) {
    if (dragged) { dragged = false; event.preventDefault(); return; }
    panel.chooseByMouse(index, event);
  }

  const style = $derived.by(() => {
    const x = box.x + (drag?.x ?? 0), y = box.y + (drag?.y ?? 0);
    return `translate:${x}px ${y}px;rotate:${drag ? 0 : box.r}deg;width:${box.w}px;height:${box.h}px;`
      + `--d:${duration}ms;--delay:${delay}ms;${clip && !drag ? `clip-path:${clip};` : ""}`;
  });
  // 命中区域上报用的目标矩形（含旋转后的外接框）：过渡期间与当前位置取并集。
  const target = $derived.by(() => {
    const angle = Math.abs(box.r) * Math.PI / 180;
    const w = box.w * Math.cos(angle) + box.h * Math.sin(angle), h = box.h * Math.cos(angle) + box.w * Math.sin(angle);
    const cx = box.x + box.w / 2, cy = box.y + box.h / 2;
    return `${cx - w / 2},${cy - h / 2},${cx + w / 2},${cy + h / 2}`;
  });
</script>

{#snippet marked(value: string)}
  {#each panel.highlights(value) as part}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
{/snippet}

<div
  class="card k-{kind}"
  class:flat class:on={selected} class:taken class:dragging={!!drag} class:favorite
  {style}
  data-hit
  data-hit-target={target}
  role="option"
  aria-selected={selected}
  aria-label={`${keycap || index + 1}，${item.title || firstLine(text) || KIND_NAME[kind]}${taken ? "，已选" : ""}`}
  tabindex="-1"
  onpointerenter={() => { if (!press) panel.hoverSelect(index); }}
  onpointermove={pointerMove}
  onpointerdown={pointerDown}
  onpointerup={pointerUp}
  onpointercancel={reset}
  onlostpointercapture={() => { if (press) { dragged = !!drag; reset(); } }}
  onclick={click}
  onkeydown={(event) => { if (event.key === "Enter" && !taken) { event.stopPropagation(); void panel.choose(index, "paste", event); } }}
  onauxclick={(event) => { if (event.button === 1 && !taken) { event.preventDefault(); panel.chooseByMouse(index, event); } }}
>
  <div class="face">
    <!-- 扇面牌面 -->
    <div class="fan">
      {#if kind === "image"}
        <div class="img">{#if image}<img src={image.url} alt="" draggable="false" />{/if}</div>
      {/if}
      <span class="kind">{#if item.title}{@render marked(item.title)}{:else}{KIND_NAME[kind]}{/if}</span>
      <div class="bd">
        {#if kind === "short"}
          <div class="short" class:big={[...text.trim()].length <= 10}>{@render marked(text.trim() || "（空文本）")}</div>
        {:else if kind === "long"}
          <div class="long">{@render marked(text.trim())}</div>
        {:else if kind === "code"}
          <pre>{@render marked(text)}</pre>
        {:else if kind === "link" && link}
          <div class="host">{@render marked(link.host)}</div>
          <div class="path">{@render marked(link.prefix)}<b>{@render marked(link.tail)}</b></div>
        {:else if kind === "color" && color}
          <div class="cv"><i class="chip" style={`background:${color.hex}`}></i>
            <div class="vals"><span class="hex">{@render marked(color.value)}</span><span class="sub">{color.format === "HEX" ? `RGB ${color.rgb.join(", ")}` : color.hex}</span>
              <span class="sub hsl">HSL {color.hsl[0]}°, {color.hsl[1]}%, {color.hsl[2]}%</span></div></div>
        {:else if kind === "file"}
          <div class="files">
            {#each item.file_names.slice(0, 5) as name}<div><i class="ext">{fileExt(name)}</i><span>{@render marked(name)}</span></div>{/each}
            {#if item.file_names.length > 5}<div class="dim">还有 {item.file_names.length - 5} 个</div>{/if}
          </div>
        {:else if kind === "mail" && mail}
          <div class="user">{@render marked(mail.user)}</div><div class="domain">@{@render marked(mail.domain)}</div>
        {:else if kind === "number"}
          <div class="number">{@render marked(text.trim())}</div>
        {/if}
      </div>
      <div class="foot">{item.title ? "常用" : meta}</div>
      {#if showSeal}<i class="seal" aria-label="常用">常</i>{/if}
    </div>

    <!-- 摊开的行面：单行；选中时展开 3–4 行 -->
    <div class="row">
      <span class="ks"></span>
      {#if kind === "image"}
        <span class="tm thumb">{#if image}<img src={image.url} alt="" draggable="false" />{/if}</span>
      {:else if kind === "color" && color}
        <span class="tm swatch" style={`background:${color.hex}`}></span>
      {:else if kind === "file"}
        <span class="tm t-file"><svg viewBox="0 0 36 26" aria-hidden="true"><path d="M1.5 1.5h27l6 6v17h-33z"/><path d="M28.5 1.5v6h6"/></svg><span>{fileExt(item.file_names[0] ?? "")}</span></span>
      {:else if kind === "long"}
        <span class="tm t-long"><svg width="20" height="16" viewBox="0 0 20 16" aria-hidden="true"><path d="M3 3.5h14M3 8h14M3 12.5h8"/></svg></span>
      {:else if kind === "link"}
        <span class="tm t-link"><svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 11.5l7-7M6 4.5h5.5V10"/></svg></span>
      {:else if kind === "mail"}
        <span class="tm t-mail"><svg width="22" height="15" viewBox="0 0 26 18" aria-hidden="true"><rect x="1" y="1" width="24" height="16" rx="2"/><path d="M2 2.5l11 8 11-8"/></svg></span>
      {:else}
        <span class="tm t-{kind}">{kind === "short" ? "Aa" : kind === "code" ? "{ }" : "#"}</span>
      {/if}
      <div class="rb">
        {#if !(selected && flat)}
          <div class="r1">
            {#if kind === "long"}<b>{@render marked(lines[0] ?? "")}</b>　<span class="dim">{@render marked(lines.slice(1).join("　"))}</span>
            {:else if kind === "link" && link}<b>{@render marked(link.host)}</b><span class="dim prefix">{@render marked(link.prefix)}</span><span class="tail">{@render marked(link.tail)}</span>
            {:else if kind === "code"}<span class="mono">{@render marked(firstLine(text).trim())}</span>
            {:else if kind === "image"}{item.title || "图片"}　<span class="dim">{imageSize(item)}</span>
            {:else if kind === "color" && color}<span class="mono">{@render marked(color.value)}</span>　<span class="dim">{color.format === "HEX" ? `RGB ${color.rgb.join(", ")}` : color.hex}</span>
            {:else if kind === "file"}{@render marked(item.file_names[0] ?? "文件")}{#if item.file_names.length > 1}　<span class="dim">等 {item.file_names.length} 个文件</span>{/if}
            {:else if kind === "mail" && mail}<b>{@render marked(mail.user)}</b><span class="dim">@{@render marked(mail.domain)}</span>
            {:else if kind === "number"}<span class="mono">{@render marked(text.trim())}</span>
            {:else}{@render marked(text.trim() || "（空文本）")}{/if}
          </div>
        {:else}
          <div class="r4">
            {#if kind === "long"}<div class="lines">{@render marked(lines.slice(0, 4).join("\n"))}</div>
            {:else if kind === "code"}<pre>{@render marked(text.split("\n").slice(0, 4).join("\n"))}</pre>
            {:else if kind === "link" && link}<div><b>{@render marked(link.host)}</b></div><div class="dim small">{@render marked(link.prefix)}<span class="tail">{@render marked(link.tail)}</span></div>
            {:else if kind === "image"}<div>{item.title || "图片"}　<span class="dim">{imageSize(item)}</span></div><div class="thumb4">{#if image}<img src={image.url} alt="" draggable="false" />{/if}</div>
            {:else if kind === "color" && color}<div><span class="mono">{@render marked(color.value)}</span></div>
              <div class="c4"><i style={`background:${color.hex}`}></i><span>HEX {color.hex}<br>RGB {color.rgb.join(", ")}<br>HSL {color.hsl[0]}°, {color.hsl[1]}%, {color.hsl[2]}%</span></div>
            {:else if kind === "file"}{#each item.file_names.slice(0, 4) as name}<div>{@render marked(name)}</div>{/each}
            {:else if kind === "mail" && mail}<b>{@render marked(mail.user)}</b><span class="dim">@{@render marked(mail.domain)}</span>
            {:else if kind === "number"}<span class="mono">{@render marked(text.trim())}</span>
            {:else}{@render marked(text.trim() || "（空文本）")}{/if}
          </div>
        {/if}
      </div>
      <span class="mt">{#if showSeal}<i class="sl">常</i>{/if}<span class="src">{meta}</span></span>
    </div>

    {#if holding}<i class="hold" style={`animation-duration:${holding.duration}ms`}></i>{/if}
  </div>
  <span class="kc" class:wide={keycap.startsWith("Ctrl")}>{#if keycap.startsWith("Ctrl")}<small>Ctrl</small>{keycap.slice(5)}{:else}{keycap}{/if}</span>
  {#if taken}<span class="tk">已选</span>{/if}
  {#if item.file_state?.state === "missing"}<span class="missing">原文件不在</span>{/if}
  {#if drag}<span class="drop">{drag.out ? "松手：粘贴到原光标处" : "松手：放回"}</span>{/if}
</div>

<style>
  .card { position: absolute; left: 0; top: 0; transform-origin: 50% 100%; cursor: pointer; user-select: none;
    transition: translate var(--d) cubic-bezier(.2,.8,.2,1) var(--delay), rotate var(--d) cubic-bezier(.2,.8,.2,1) var(--delay),
      width var(--d) cubic-bezier(.2,.8,.2,1) var(--delay), height var(--d) cubic-bezier(.2,.8,.2,1) var(--delay); }
  .card.dragging { transition: none; cursor: grabbing; }
  .face { position: absolute; inset: 0; overflow: hidden; border-radius: 10px; background: var(--hand-card); box-shadow: var(--hand-card-shadow); transition: border-radius var(--d), box-shadow 160ms; }
  .on .face, .dragging .face { box-shadow: var(--hand-selected-shadow); }
  .flat .face { border-radius: 8px; box-shadow: var(--hand-row-shadow); }
  .flat.on .face { box-shadow: var(--hand-pop); }
  .face::after { content: ""; position: absolute; inset: 0; z-index: 5; background: var(--hand-card-back); opacity: 0; transition: opacity 160ms; pointer-events: none; }
  .flat:not(.on):not(.taken):hover .face::after { opacity: .5; }
  /* 两层内容交叉：扇面层先淡出，行面层后 40% 淡入。 */
  .fan { position: absolute; inset: 0; opacity: 1; transition: opacity 110ms linear calc(var(--delay) + var(--d) * .5); }
  .row { position: absolute; inset: 0; z-index: 1; display: flex; align-items: flex-start; gap: 8px; padding: 7px 12px 0 8px; opacity: 0; transition: opacity 90ms linear var(--delay); }
  .flat .fan { opacity: 0; transition-delay: var(--delay); }
  .flat .row { opacity: 1; transition: opacity 100ms linear calc(var(--delay) + var(--d) * .6); }

  .kc { position: absolute; left: 10px; top: 10px; z-index: 6; min-width: 26px; height: 26px; padding: 0 6px; border-radius: 6px; background: var(--hand-key); color: var(--hand-ink);
    box-shadow: inset 0 0 0 1px var(--hand-key-line), inset 0 -2px 0 var(--hand-key-line); font: 700 15px/26px var(--font-ui); text-align: center; white-space: nowrap; font-variant-numeric: tabular-nums;
    transition: left var(--d) var(--delay), top var(--d) var(--delay); }
  .flat .kc { left: 8px; top: 7px; }
  .kc:empty { visibility: hidden; }
  .kc small { margin-right: 4px; font-size: 12px; font-weight: 500; }
  .on .kc { background: var(--hand-seal); color: var(--hand-on-seal); box-shadow: inset 0 -2px 0 rgba(0,0,0,.22); }
  .taken .kc { opacity: .5; }

  .kind { position: absolute; z-index: 2; left: 44px; right: 30px; top: 14px; overflow: hidden; color: var(--hand-ink-3); font-size: 12px; line-height: 18px; text-overflow: ellipsis; white-space: nowrap; }
  .favorite .kind { color: var(--hand-ink-2); font-weight: 600; }
  .bd { position: absolute; left: 12px; right: 12px; top: 46px; bottom: 32px; overflow: hidden; color: var(--hand-ink); font-size: 13px; line-height: 20px; overflow-wrap: anywhere; }
  .foot { position: absolute; z-index: 2; left: 12px; right: 12px; bottom: 8px; overflow: hidden; color: var(--hand-ink-3); font-size: 12px; line-height: 16px; text-overflow: ellipsis; white-space: nowrap; }
  .seal { position: absolute; z-index: 3; right: 10px; top: 10px; width: 18px; height: 18px; border-radius: 3px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 12px/18px var(--font-display); font-style: normal; text-align: center; }
  .short { height: 100%; display: flex; align-items: center; justify-content: center; text-align: center; font-size: 15px; line-height: 22px; }
  .short.big { font-size: 20px; font-weight: 500; line-height: 28px; }
  .long { white-space: pre-wrap; mask-image: linear-gradient(#000 70%, transparent); }
  .long::first-line { font-weight: 600; }
  .bd pre, .r4 pre { margin: 0; font: 12px/18px var(--font-mono); white-space: pre; }
  .host { font-size: 15px; font-weight: 700; line-height: 20px; }
  .path { margin-top: 4px; color: var(--hand-ink-3); font-size: 12px; line-height: 18px; }
  .path b, .tail { display: inline-block; color: var(--hand-ink); font-weight: 600; white-space: nowrap; }
  .cv { display: flex; flex-direction: column; gap: 6px; }
  .chip { width: 56px; height: 56px; display: block; flex: none; border-radius: 8px; box-shadow: inset 0 0 0 1px var(--hand-line-strong); }
  .vals { display: flex; flex-direction: column; min-width: 0; }
  .hex { font: 600 15px/20px var(--font-mono); white-space: nowrap; }
  .sub { color: var(--hand-ink-3); font-size: 12px; line-height: 16px; white-space: nowrap; }
  .hsl { display: none; }
  .on:not(.flat) .cv { flex-direction: row; align-items: center; gap: 12px; }
  .on:not(.flat) .chip { width: 64px; height: 64px; }
  .on:not(.flat) .hsl { display: block; }
  .img { position: absolute; inset: 0; background: var(--hand-card-back); }
  .img img, .thumb img, .thumb4 img { width: 100%; height: 100%; display: block; object-fit: cover; }
  .k-image .kind, .k-image .foot { color: #fff; text-shadow: 0 1px 2px rgba(0,0,0,.5); }
  .files > div { display: flex; align-items: center; gap: 6px; font-size: 12px; line-height: 22px; white-space: nowrap; }
  .files span { overflow: hidden; text-overflow: ellipsis; }
  .ext { flex: none; min-width: 32px; height: 18px; padding: 0 4px; border-radius: 3px; box-shadow: inset 0 0 0 1px var(--hand-line-strong); color: var(--hand-ink-2); font: 600 12px/18px var(--font-mono); font-style: normal; text-align: center; }
  .user { font-size: 15px; font-weight: 700; line-height: 22px; }
  .domain { color: var(--hand-ink-3); font-size: 13px; line-height: 20px; }
  .number { height: 100%; display: flex; align-items: center; font: 500 15px/24px var(--font-mono); overflow-wrap: normal; word-break: keep-all; }

  .ks { flex: none; width: 26px; height: 26px; }
  :global(.searching) .ks { width: 52px; }
  .kc.wide { min-width: 52px; }
  /* 行的类型标记：二级墨色，靠外形区分，不用朱砂。 */
  .tm { position: relative; flex: none; width: 36px; height: 26px; display: grid; place-items: center; overflow: hidden; border-radius: 5px; color: var(--hand-ink-2); font: 600 12px/1 var(--font-mono); }
  .tm svg { display: block; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  .t-short { background: var(--hand-card-back); box-shadow: inset 0 0 0 1px var(--hand-line); font: 600 13px/1 var(--font-ui); }
  .t-long { background: var(--hand-card-back); box-shadow: inset 0 0 0 1px var(--hand-line); }
  .t-link { border-radius: 13px; box-shadow: inset 0 0 0 1.5px var(--hand-ink-2); }
  .t-code { background: var(--hand-ink-2); color: var(--hand-card); }
  .t-file { overflow: visible; border-radius: 0; }
  .t-file svg { position: absolute; inset: 0; width: 36px; height: 26px; }
  .t-file span { position: relative; top: 1px; }
  .t-number { border-radius: 0; box-shadow: inset 0 -1.5px 0 var(--hand-ink-2); font-size: 13px; }
  .swatch, .thumb { box-shadow: inset 0 0 0 1px var(--hand-line-strong); }
  .rb { flex: 1; min-width: 0; color: var(--hand-ink); font-size: 13px; line-height: 26px; }
  .k-short .rb { font-size: 15px; }
  .r1 { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .k-link .r1 { display: flex; min-width: 0; }
  .k-link .r1 b { flex: none; }
  .prefix { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .r4 { line-height: 20px; padding-top: 3px; }
  .lines { max-height: 84px; overflow: hidden; white-space: pre-wrap; overflow-wrap: anywhere; }
  .lines::first-line { font-weight: 600; }
  .r4 pre { line-height: 20px; max-height: 84px; overflow: hidden; }
  .r4 > div { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .r4 .lines { white-space: pre-wrap; }
  .small { font-size: 12px; }
  .thumb4 { width: 144px; height: 81px; margin-top: 4px; overflow: hidden; border-radius: 4px; background: var(--hand-card-back); }
  .c4 { display: flex; align-items: center; gap: 12px; margin-top: 4px; }
  .c4 i { width: 56px; height: 56px; flex: none; border-radius: 6px; box-shadow: inset 0 0 0 1px var(--hand-line-strong); }
  .c4 span { color: var(--hand-ink-2); font: 12px/18px var(--font-mono); }
  .dim { color: var(--hand-ink-3); }
  .mono { font-family: var(--font-mono); }
  .k-code .r1 .mono { font-size: 12px; }
  .mt { flex: none; max-width: 160px; display: flex; align-items: center; gap: 6px; color: var(--hand-ink-3); font-size: 12px; line-height: 26px; white-space: nowrap; }
  .src { overflow: hidden; text-overflow: ellipsis; }
  .sl { width: 18px; height: 18px; flex: none; border-radius: 3px; background: var(--hand-seal); color: var(--hand-on-seal); font: 600 12px/18px var(--font-display); font-style: normal; text-align: center; }

  .taken { cursor: default; }
  .taken .face { background: var(--hand-card-back); box-shadow: none; outline: 1.5px dashed var(--hand-line-strong); outline-offset: -1.5px; }
  .taken .face > :not(.hold) { opacity: .18; }
  .taken .mt { visibility: hidden; }
  .tk { position: absolute; z-index: 7; left: 10px; top: 46px; height: 22px; padding: 0 8px; border-radius: 4px; background: var(--hand-paper); box-shadow: inset 0 0 0 1px var(--hand-line-strong); color: var(--hand-ink-2); font-size: 12px; line-height: 22px; }
  .flat .tk { left: auto; right: 12px; top: 9px; }
  .missing { position: absolute; z-index: 7; left: 10px; bottom: 28px; padding: 0 6px; border-radius: 3px; background: var(--state-critical-bg); color: var(--state-critical); font-size: 12px; line-height: 18px; }
  .flat .missing { left: auto; right: 170px; bottom: auto; top: 11px; }
  .drop { position: absolute; z-index: 8; left: 50%; bottom: -30px; translate: -50% 0; padding: 0 10px; border-radius: 12px; background: var(--hand-paper); box-shadow: var(--hand-pop); color: var(--hand-ink-2); font-size: 12px; line-height: 24px; white-space: nowrap; }
  .hold { position: absolute; z-index: 6; left: 0; right: 0; bottom: 0; height: 3px; background: var(--state-critical); transform-origin: left; animation: hold linear forwards; }
  @keyframes hold { from { transform: scaleX(0); } to { transform: scaleX(1); } }
  mark { padding: 0 1px; border-radius: 2px; background: var(--hand-mark); box-shadow: inset 0 -1.5px 0 var(--hand-line-strong); color: inherit; }
  /* 减少动画：位置尺寸直接到位，只留 100ms 淡入淡出。 */
  :global(html[data-motion="none"]) .card, :global(html[data-reduce-motion="true"]) .card { transition-duration: 0ms; transition-delay: 0ms; }
  :global(html[data-motion="none"]) .card .fan, :global(html[data-motion="none"]) .card .row,
  :global(html[data-reduce-motion="true"]) .card .fan, :global(html[data-reduce-motion="true"]) .card .row { transition: opacity 100ms linear; }
</style>
