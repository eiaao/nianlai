/**
 * 删除动画：粉碎成粒子（B12）。
 *
 * 删除那一刻把条目复制一份放到面板上的浮层里播放，列表本身只负责收起补位，选中、撤销等逻辑不受影响。
 * 复制品先变灰，再被一道带噪点的参差边缘从左往右“吃掉”；每个被吃掉的点化成 1～2px 的微粒，
 * 向右上打着旋飘走、变淡。微粒用界面的次要灰（--faint），半透明；日间中灰，夜间自动跟着变。
 *
 * 备选方案“便利贴撕下碎裂”的设计和演示保留在内部文档 design/delete-effects/。
 */

/** 擦除扫过整条所需的时间；列表等扫过大半再收起，否则下一条会从被吃掉的缺口里透出来。 */
export const SWEEP_MS = 620;
export const COLLAPSE_AFTER_MS = 560;
/** 微粒最长寿命，加上扫过时间就是整段动画的上限；到时无论如何都清掉浮层。 */
const LIFE_MS = 1050;

// 复制品：去掉 id 避免重复；停掉所有 CSS 动画，保持删除那一刻的样子（例如按住时铺满的进度）。
function snapshot(source: HTMLElement, rect: DOMRect) {
  const copy = source.cloneNode(true) as HTMLElement;
  for (const element of [copy, ...copy.querySelectorAll<HTMLElement>("*")]) {
    element.removeAttribute("id");
    element.style.animation = "none";
    element.style.transition = "none";
  }
  copy.setAttribute("aria-hidden", "true");
  Object.assign(copy.style, {
    position: "fixed", left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`,
    margin: "0", transform: "none", opacity: "1", visibility: "visible", pointerEvents: "none",
  });
  return copy;
}

export function shatter(source: HTMLElement, host: HTMLElement) {
  const rect = source.getBoundingClientRect();
  if (rect.width < 1 || rect.height < 1) return;
  const layer = document.createElement("div");
  layer.style.cssText = "position:fixed;inset:0;z-index:50;pointer-events:none;overflow:visible";
  host.append(layer);
  const sheet = snapshot(source, rect);
  layer.append(sheet);
  sheet.animate([{ filter: "none" }, { filter: "grayscale(1) contrast(1.05)" }], { duration: 160, fill: "forwards" });

  // 擦除遮罩：每 2px 一格，按“横向位置 + 成块的噪点 + 细噪点”排出先后，边缘参差不齐。
  const grid = 2, mw = Math.ceil(rect.width / grid), mh = Math.ceil(rect.height / grid);
  const mask = document.createElement("canvas");
  mask.width = mw; mask.height = mh;
  const maskContext = mask.getContext("2d");
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!maskContext || !context) { layer.remove(); return; }
  const image = maskContext.createImageData(mw, mh);
  image.data.fill(255);
  const blocksX = Math.ceil(mw / 6), blocks = Array.from({ length: blocksX * Math.ceil(mh / 6) }, Math.random);
  const order = new Float32Array(mw * mh);
  for (let y = 0; y < mh; y++) {
    for (let x = 0; x < mw; x++) {
      order[y * mw + x] = x / mw * .72 + blocks[Math.floor(y / 6) * blocksX + Math.floor(x / 6)] * .18 + Math.random() * .1;
    }
  }
  const gone = new Uint8Array(mw * mh);

  // 微粒画布比条目大，给向右上飘的微粒留空间。
  const ratio = devicePixelRatio || 1, padX = 140, padY = 110, left = 20;
  const width = rect.width + padX + left, height = rect.height + padY + 20;
  canvas.width = Math.ceil(width * ratio); canvas.height = Math.ceil(height * ratio);
  Object.assign(canvas.style, { position: "fixed", left: `${rect.left - left}px`, top: `${rect.top - padY}px`, width: `${width}px`, height: `${height}px` });
  layer.append(canvas);
  context.scale(ratio, ratio);
  context.fillStyle = getComputedStyle(host).getPropertyValue("--faint").trim() || "#64736c";

  type Particle = { x: number; y: number; vx: number; vy: number; size: number; alpha: number; age: number; life: number; wobble: number };
  const particles: Particle[] = [];
  const start = performance.now();
  let last = start;
  const finish = () => { layer.remove(); };
  // 面板收起后 requestAnimationFrame 会停；到上限时间就直接清掉。
  const guard = setTimeout(finish, SWEEP_MS + LIFE_MS + 300);
  const frame = (now: number) => {
    if (!layer.isConnected) return;
    const dt = Math.min(48, now - last) / 1000;
    last = now;
    const progress = (now - start) / SWEEP_MS * 1.04;
    let changed = false;
    for (let i = 0; i < order.length; i++) {
      if (gone[i] || order[i] > progress) continue;
      gone[i] = 1;
      image.data[i * 4 + 3] = 0;
      changed = true;
      if (Math.random() < .7) {
        particles.push({
          x: (i % mw) * grid + Math.random() * grid + left, y: Math.floor(i / mw) * grid + Math.random() * grid + padY,
          vx: 14 + Math.random() * 70, vy: -8 - Math.random() * 46, size: .7 + Math.random() * 1.5,
          alpha: .22 + Math.random() * .4, age: 0, life: .45 + Math.random() * .6, wobble: Math.random() * 6.28,
        });
      }
    }
    if (changed) {
      maskContext.putImageData(image, 0, 0);
      const url = `url(${mask.toDataURL()})`;
      sheet.style.setProperty("mask-image", url);
      sheet.style.setProperty("-webkit-mask-image", url);
      sheet.style.setProperty("mask-size", "100% 100%");
      sheet.style.setProperty("-webkit-mask-size", "100% 100%");
    }
    context.clearRect(0, 0, width, height);
    for (let k = particles.length - 1; k >= 0; k--) {
      const p = particles[k];
      p.age += dt;
      if (p.age >= p.life) { particles[k] = particles[particles.length - 1]; particles.pop(); continue; }
      p.vx += Math.sin(p.wobble + p.age * 9) * 30 * dt;
      p.vy -= 20 * dt;
      p.x += p.vx * dt;
      p.y += p.vy * dt;
      context.globalAlpha = p.alpha * (1 - p.age / p.life);
      context.fillRect(p.x, p.y, p.size, p.size);
    }
    if (progress >= 1.04 && particles.length === 0) { clearTimeout(guard); finish(); }
    else requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}
