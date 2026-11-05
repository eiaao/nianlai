import type { MotionPreference } from "../preferences";

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
