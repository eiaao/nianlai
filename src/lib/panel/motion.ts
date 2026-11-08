import type { MotionPreference } from "../preferences";
import { COLLAPSE_AFTER_MS, shatter } from "./deleteEffect";

// v7 的临界阻尼弹簧：积分不超过 1/120s，每帧只写 transform / opacity。
export class SpringValue {
  private value: number;
  private target: number;
  private velocity = 0;
  private frame = 0;
  private last = 0;
  private ratio = 1;
  private readonly reduced = matchMedia("(prefers-reduced-motion: reduce)");
  private readonly onReduced = () => this.reset(this.target);

  constructor(
    initial: number,
    private readonly render: (value: number) => void,
    private readonly motion: () => MotionPreference,
    private readonly response = 0.3,
  ) {
    this.value = this.target = initial;
    this.reduced.addEventListener("change", this.onReduced);
    this.render(initial);
  }

  to(target: number, ratio = 1) {
    this.target = target;
    this.ratio = ratio;
    if (this.motion() === "none" || this.reduced.matches) {
      this.reset(target);
      return;
    }
    if (!this.frame) {
      this.last = performance.now();
      this.frame = requestAnimationFrame((time) => this.tick(time));
    }
  }

  nudge(value: number) {
    this.value = value;
    this.velocity = 0;
    this.render(value);
  }

  reset(value: number) {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
    this.value = this.target = value;
    this.velocity = 0;
    this.render(value);
  }

  cancel() {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
  }

  destroy() {
    this.cancel();
    this.reduced.removeEventListener("change", this.onReduced);
  }

  private tick(now: number) {
    this.frame = 0;
    let remaining = Math.min((now - this.last) / 1000, 0.05);
    this.last = now;
    const omega = 4.6 / this.response;
    while (remaining > 0) {
      const dt = Math.min(remaining, 1 / 120);
      remaining -= dt;
      this.velocity += (omega * omega * (this.target - this.value) - 2 * this.ratio * omega * this.velocity) * dt;
      this.value += this.velocity * dt;
    }
    this.render(this.value);
    if (Math.abs(this.target - this.value) < 0.001 && Math.abs(this.velocity) < 0.001) {
      this.reset(this.target);
      return;
    }
    this.frame = requestAnimationFrame((time) => this.tick(time));
  }
}

/**
 * 删除时的退场（B12）：只给刚删除的那一条（panel.leavingId）播放；搜索结果刷新等其他移除不动画。
 * 原位置立即隐藏，复制品在浮层上粉碎成粒子（deleteEffect.ts）；列表行等擦除扫过大半再收起高度，
 * 让下面的条目补位。卡片流不收起，卡片自己会移动补位。减少动画时只淡出。
 */
export function leave(node: HTMLElement, { leaving, motion, collapse }: { leaving: boolean; motion: MotionPreference; collapse: boolean }) {
  if (!leaving) return { duration: 0 };
  // 让选中竖条不再把这一行当成选中行；leaving 让列表先藏起紧跟在后面、由下一条新画出的分区标题，
  // 旧标题留在原处，收起结束后两者在同一位置交接，不跳动。
  node.removeAttribute("data-history-index");
  node.classList.add("leaving");
  if (motion === "none" || matchMedia("(prefers-reduced-motion: reduce)").matches) {
    return { duration: 120, css: (t: number) => `opacity:${t}` };
  }
  shatter(node, node.closest<HTMLElement>(".panel-shell") ?? document.body);
  const height = node.offsetHeight, total = COLLAPSE_AFTER_MS + (collapse ? 260 : 0);
  return {
    duration: total,
    css: (_t: number, u: number) => {
      if (!collapse) return "visibility:hidden;";
      const shrink = Math.max(0, (u * total - COLLAPSE_AFTER_MS) / 260);
      const eased = shrink * shrink * (3 - 2 * shrink);
      return `visibility:hidden;height:${(height * (1 - eased)).toFixed(2)}px;overflow:hidden;`;
    },
  };
}
