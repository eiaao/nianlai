import type {
  GuideCardId,
  GuidePhase,
  GuidePasteResult,
  GuideSettings,
  HistorySnapshot,
  SettingsUpdate,
} from "./ipc";

export type GuideHealth = {
  store: "loading" | "ready" | "failed";
  capture: "starting" | "recording" | "failed" | "stopping";
};
export type GuideSettingsPage = "general" | "privacy" | "data";
export type GuideNotice = {
  kind: "info" | "paused" | "store_failed" | "capture_failed" | "paste_failed" | "save_failed" | "sample_missing";
  message: string;
  settingsPage?: GuideSettingsPage;
};
export type GuidePendingAction = "save" | "complete" | "skip";
export type GuidePending = {
  token: number;
  guide: GuideSettings;
  action: GuidePendingAction;
};
export type GuideState = {
  /** 最近一次已成功持久化的可见状态；终态不能在保存成功前写进这里。 */
  guide: GuideSettings;
  /** 本次面板会话和运行时信息不写入 settings.json。 */
  session: number | null;
  /** 已见 session 的单调高水位；收起/失败后拒绝任何不大于它的旧 shown。 */
  lastSession: number | null;
  targetConfirmed: boolean;
  baselineReady: boolean;
  health: GuideHealth;
  paused: boolean;
  pending: GuidePending | null;
  error: string | null;
  notice: GuideNotice | null;
  nextToken: number;
};
export type GuideHistory = Pick<HistorySnapshot, "items" | "total" | "last_recorded_ms">;
export type GuideEvent =
  | { type: "baseline"; snapshot: GuideHistory }
  | { type: "history_changed"; snapshot: GuideHistory }
  | { type: "restore"; snapshot: GuideHistory }
  | { type: "input_target_confirmed" }
  | { type: "panel_shown"; session: number }
  | { type: "panel_hidden"; session?: number }
  | { type: "paste_result"; session: number; id: number; result: GuidePasteResult["result"] }
  | { type: "skip_step" }
  | { type: "skip_guide" }
  | { type: "close_guide" }
  | { type: "persist_succeeded"; token: number }
  | { type: "persist_failed"; token: number; message?: string }
  | { type: "retry_persist" }
  | { type: "health_changed"; health: GuideHealth; paused: boolean }
  | { type: "copy_sample" }
  | { type: "open_settings"; page: GuideSettingsPage }
  | { type: "dismiss_card"; id: GuideCardId };
// persist_guide 必须由执行器按发出顺序串行 await setSettings(update)，再把结果连同 token 送回 reduce。
// 新事件可以取代尚未完成的候选；旧 token 的回包不会推进状态，也不会触发发牌或关闭。
export type GuideEffect =
  | { type: "persist_guide"; token: number; update: SettingsUpdate }
  | { type: "open_panel" }
  | { type: "copy_sample" }
  | { type: "open_settings"; page: GuideSettingsPage }
  | { type: "deal_cards" }
  | { type: "close_guide" };
export type GuideTransition = {
  state: GuideState;
  effects: GuideEffect[];
};

const phases: readonly GuidePhase[] = ["await_copy", "await_show", "await_paste", "completed", "skipped"];
const cardIds: readonly GuideCardId[] = ["t1", "t2", "t3", "t4"];
const actions: readonly GuidePendingAction[] = ["save", "complete", "skip"];
const noticeKinds: readonly GuideNotice["kind"][] = [
  "info", "paused", "store_failed", "capture_failed", "paste_failed", "save_failed", "sample_missing",
];
const MAX_SAFE = Number.MAX_SAFE_INTEGER;

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function safeInteger(value: unknown, positive = false): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= (positive ? 1 : 0);
}
function cloneGuide(guide: GuideSettings): GuideSettings {
  return { ...guide, dismissed: [...guide.dismissed] };
}
function normalizeGuide(value: unknown): GuideSettings {
  const input = record(value) ? value : {};
  const requestedPhase = phases.includes(input.phase as GuidePhase) ? input.phase as GuidePhase : "await_copy";
  const sampleId = safeInteger(input.sample_id, true) ? input.sample_id : null;
  const needsSample = requestedPhase === "await_show" || requestedPhase === "await_paste";
  const phase = needsSample && sampleId === null ? "await_copy" : requestedPhase;
  const terminal = phase === "completed" || phase === "skipped";
  const dismissed: GuideCardId[] = [];
  if (Array.isArray(input.dismissed)) {
    for (const id of input.dismissed) {
      if (cardIds.includes(id as GuideCardId) && !dismissed.includes(id as GuideCardId)) {
        dismissed.push(id as GuideCardId);
      }
    }
  }
  return {
    version: 1,
    phase,
    baseline_ms: !terminal && safeInteger(input.baseline_ms) ? input.baseline_ms : null,
    baseline_total: !terminal && safeInteger(input.baseline_total) ? input.baseline_total : 0,
    sample_id: terminal ? null : sampleId,
    cards_dealt: terminal && input.cards_dealt === true,
    dismissed,
  };
}
function normalizeNotice(value: unknown): GuideNotice | null {
  if (!record(value) || typeof value.message !== "string" || !noticeKinds.includes(value.kind as GuideNotice["kind"])) {
    return null;
  }
  const settingsPage = value.settingsPage === "general" || value.settingsPage === "privacy" || value.settingsPage === "data"
    ? value.settingsPage : undefined;
  return { kind: value.kind as GuideNotice["kind"], message: value.message, ...(settingsPage ? { settingsPage } : {}) };
}
function normalizeState(value: unknown): GuideState {
  const input = record(value) ? value : {};
  const guide = normalizeGuide(input.guide);
  let pending: GuidePending | null = null;
  if (record(input.pending) && safeInteger(input.pending.token, true)) {
    const action = actions.includes(input.pending.action as GuidePendingAction)
      ? input.pending.action as GuidePendingAction : null;
    if (action) {
      pending = { token: input.pending.token, guide: normalizeGuide(input.pending.guide), action };
    }
  }
  const requestedToken = safeInteger(input.nextToken, true) && input.nextToken < MAX_SAFE ? input.nextToken : 1;
  const nextToken = pending ? Math.max(requestedToken, pending.token >= MAX_SAFE - 1 ? 1 : pending.token + 1) : requestedToken;
  const healthInput = record(input.health) ? input.health : {};
  const health: GuideHealth = {
    store: healthInput.store === "ready" || healthInput.store === "failed" ? healthInput.store : "loading",
    capture: healthInput.capture === "recording" || healthInput.capture === "failed" || healthInput.capture === "stopping"
      ? healthInput.capture : "starting",
  };
  return {
    guide,
    session: safeInteger(input.session, true) ? input.session : null,
    lastSession: safeInteger(input.lastSession, true) ? input.lastSession : null,
    targetConfirmed: input.targetConfirmed === true,
    baselineReady: input.baselineReady === true,
    health,
    paused: input.paused === true,
    pending,
    error: typeof input.error === "string" ? input.error : null,
    notice: normalizeNotice(input.notice),
    nextToken,
  };
}
function same(state: GuideState, effects: GuideEffect[] = []): GuideTransition {
  return { state, effects };
}
function noticed(state: GuideState, notice: GuideNotice): GuideTransition {
  return same({ ...state, notice });
}
function save(
  state: GuideState,
  value: GuideSettings,
  action: GuidePendingAction = "save",
  notice: GuideNotice | null = null,
): GuideTransition {
  const guide = normalizeGuide(value);
  const token = state.nextToken;
  const pending = { token, guide: cloneGuide(guide), action };
  return same(
    { ...state, pending, nextToken: token >= MAX_SAFE - 1 ? 1 : token + 1, error: null, notice },
    [{ type: "persist_guide", token, update: { guide: cloneGuide(guide) } }],
  );
}
function currentGuide(state: GuideState): GuideSettings {
  return state.pending?.guide ?? state.guide;
}
function terminal(guide: GuideSettings): boolean {
  return guide.phase === "completed" || guide.phase === "skipped";
}
function completedGuide(state: GuideState): GuideSettings {
  return {
    ...currentGuide(state), phase: "completed", baseline_ms: null, baseline_total: 0,
    sample_id: null, cards_dealt: true,
  };
}
function healthNotice(state: GuideState): GuideNotice | null {
  if (state.health.store === "failed") {
    return { kind: "store_failed", message: "存东西出了问题，先到数据与备份看看。", settingsPage: "data" };
  }
  if (state.health.capture === "failed") {
    return { kind: "capture_failed", message: "记录出了问题，先到记录与隐私看看。", settingsPage: "privacy" };
  }
  if (state.paused) {
    return { kind: "paused", message: "已暂停，现在复制的不会记下；继续记录后再试。", settingsPage: "privacy" };
  }
  if (state.health.store === "loading" || state.health.capture === "starting") {
    return { kind: "info", message: "还在准备记录，稍后再试。" };
  }
  return null;
}
function snapshot(value: unknown) {
  if (!record(value)) return null;
  const last = value.last_recorded_ms === null ? null : safeInteger(value.last_recorded_ms) ? value.last_recorded_ms : null;
  const total = safeInteger(value.total) ? value.total : 0;
  const items = Array.isArray(value.items) ? value.items.flatMap((item) => {
    if (!record(item) || !safeInteger(item.id, true) || !safeInteger(item.copied_ms)) return [];
    return [{ id: item.id, copied_ms: item.copied_ms }];
  }) : [];
  return { last, total, items };
}
function baseline(state: GuideState, value: unknown): GuideTransition {
  const current = currentGuide(state);
  if (terminal(current)) return same(state);
  const observed = snapshot(value);
  if (!observed) return noticed(state, { kind: "info", message: "还没核对到复制记录，请再试一次。" });
  const next = {
    ...state, baselineReady: true, session: null, lastSession: null, targetConfirmed: false,
  };
  if ((current.phase === "await_show" || current.phase === "await_paste")
    && !observed.items.some((item) => item.id === current.sample_id)) {
    const notice = { kind: "sample_missing" as const, message: "刚才那条不在了，再复制一句" };
    return save({ ...next, notice }, {
      ...current, phase: "await_copy", baseline_ms: observed.last,
      baseline_total: observed.total, sample_id: null, cards_dealt: false,
    }, "save", notice);
  }
  return save(next, {
    ...current, baseline_ms: observed.last, baseline_total: observed.total,
    sample_id: current.phase === "await_copy" ? null : current.sample_id, cards_dealt: false,
  });
}

function reduce(value: GuideState, event: GuideEvent): GuideTransition {
  const state = normalizeState(value);
  if (!record(event) || typeof event.type !== "string") return same(state);
  const current = currentGuide(state);

  switch (event.type) {
    case "baseline":
      return baseline(state, event.snapshot);
    case "history_changed": {
      if (terminal(current)) return same(state);
      const blocked = healthNotice(state);
      if (blocked) return noticed(state, blocked);
      if (!state.baselineReady) return baseline(state, event.snapshot);
      if (current.phase !== "await_copy" && current.phase !== "await_show") return same(state);
      const observed = snapshot(event.snapshot);
      if (!observed) return noticed(state, { kind: "info", message: "还没核对到复制记录，请再试一次。" });
      const timeIncreased = observed.last !== null
        && (current.baseline_ms === null || observed.last > current.baseline_ms);
      if (!timeIncreased && observed.total <= current.baseline_total) return same(state);
      const sample = observed.items.find((item) => item.copied_ms === observed.last);
      if (!sample) return noticed(state, { kind: "info", message: "还没找到刚复制的那条，再复制一句。" });
      const next = { ...state, baselineReady: true, session: null, lastSession: null, targetConfirmed: false };
      return save(next, {
        ...current, phase: "await_show", baseline_ms: observed.last,
        baseline_total: observed.total, sample_id: sample.id, cards_dealt: false,
      });
    }
    case "restore": {
      // restore 只清理运行时会话；在途写已经可能落盘，必须保留 token 等成功/失败回包。
      const restored = current;
      const observed = snapshot(event.snapshot);
      const next = {
        ...state, session: null, lastSession: null, targetConfirmed: false, baselineReady: observed !== null,
      };
      if (terminal(restored)) return same(next);
      if ((restored.phase === "await_show" || restored.phase === "await_paste")
        && !observed?.items.some((item) => item.id === restored.sample_id)) {
        const notice = { kind: "sample_missing" as const, message: "刚才那条不在了，再复制一句" };
        // 普通 pending 可能已写入已删除样本；用新 token 的 await_copy 补偿写取代它。
        return save({ ...next, notice }, {
          ...restored, phase: "await_copy", baseline_ms: observed?.last ?? null,
          baseline_total: observed?.total ?? 0, sample_id: null, cards_dealt: false,
        }, "save", notice);
      }
      if (!observed) {
        return noticed(next, { kind: "info", message: "还没核对到复制记录，请再试一次。" });
      }
      if (restored.baseline_ms === observed.last && restored.baseline_total === observed.total) {
        return same(next);
      }
      // 恢复时快照就是可信基线，包括 last=null 的空库；缺字段或旧基线需持久化补齐。
      return save(next, { ...restored, baseline_ms: observed.last, baseline_total: observed.total });
    }
    case "input_target_confirmed": {
      if ((current.phase !== "await_show" && current.phase !== "await_paste") || current.sample_id === null) {
        return same(state);
      }
      return same({ ...state, session: null, targetConfirmed: true, notice: null }, [{ type: "open_panel" }]);
    }
    case "panel_shown": {
      if (!safeInteger(event.session, true)) return same(state);
      if ((current.phase !== "await_show" && current.phase !== "await_paste") || current.sample_id === null) {
        return same(state);
      }
      // 同一活动展示的重复通知不重写；session 是单调高水位，收起/失败后更不能回退。
      const highWater = Math.max(state.lastSession ?? 0, state.session ?? 0);
      if (state.session === event.session || event.session <= highWater) return same(state);
      if (!state.targetConfirmed) {
        return noticed(state, { kind: "info", message: "先点一下能打字的地方，确认后再呼出拈来。" });
      }
      const next = { ...state, session: event.session, lastSession: event.session, notice: null };
      if (current.phase === "await_paste") return same(next);
      return save(next, { ...current, phase: "await_paste" });
    }
    case "panel_hidden": {
      if (terminal(current)) return same(state);
      if (event.session === undefined || event.session !== state.session) return same(state);
      if (current.phase !== "await_paste") return same(state);
      // return_focus 会先收窗再发本次粘贴结论：保留 session 给随后的 result 精确匹配。
      // 用户重新确认呼出时才清掉它；若始终没贴，界面仍提示并允许用新 session 重试。
      return same({
        ...state, targetConfirmed: false,
        notice: { kind: "info", message: "面板已收起，选好能打字的地方后再呼出一次。" },
      });
    }
    case "paste_result": {
      if (current.phase !== "await_paste" || current.sample_id === null
        || !safeInteger(event.session, true) || !safeInteger(state.session, true)
        || event.session !== state.session || event.id !== current.sample_id) return same(state);
      if (event.result === "sent" || event.result === "confirmed") {
        return save({ ...state, notice: null }, completedGuide(state), "complete");
      }
      if (event.result === "copied" || event.result === "copied_only") {
        return noticed({ ...state, session: null, targetConfirmed: false }, {
          kind: "paste_failed",
          message: "这里可能不能输入文字，刚才只复制了内容；请先点一下记事本等能打字的地方再试。",
        });
      }
      if (event.result === "native_submission_unknown") {
        return noticed({ ...state, session: null, targetConfirmed: false }, { kind: "paste_failed", message: "没能确认是否贴出，请换一个能打字的地方再试。" });
      }
      return noticed({ ...state, session: null, targetConfirmed: false }, { kind: "paste_failed", message: "这次没能贴出，请换一个能打字的地方再试。" });
    }
    case "skip_step": {
      if (current.phase === "await_paste") return save(state, completedGuide(state), "complete");
      if (state.guide.phase === "await_paste" && state.pending?.action === "complete") {
        return reduce(state, { type: "retry_persist" });
      }
      return same(state);
    }
    case "skip_guide":
    case "close_guide": {
      // 以可见状态为准，可撤销尚未保存成功的完成候选；旧 token 回包随后会被忽略。
      if (terminal(state.guide)) return same(state);
      return save(state, {
        ...state.guide, phase: "skipped", baseline_ms: null, baseline_total: 0,
        sample_id: null, cards_dealt: false,
      }, "skip");
    }
    case "persist_succeeded": {
      if (!state.pending || event.token !== state.pending.token) return same(state);
      const pending = state.pending;
      const guide = cloneGuide(pending.guide);
      const next = {
        ...state, guide, pending: null, error: null,
        session: terminal(guide) ? null : state.session,
        lastSession: terminal(guide) ? null : state.lastSession,
        targetConfirmed: terminal(guide) ? false : state.targetConfirmed,
      };
      if (pending.action === "complete" && guide.phase === "completed" && guide.cards_dealt) {
        return same({ ...next, notice: null }, [{ type: "deal_cards" }, { type: "close_guide" }]);
      }
      if (pending.action === "skip" && guide.phase === "skipped") {
        return same({ ...next, notice: null }, [{ type: "close_guide" }]);
      }
      return same(next);
    }
    case "persist_failed": {
      if (!state.pending || event.token !== state.pending.token) return same(state);
      const error = typeof event.message === "string" && event.message.trim()
        ? event.message : "没保存上，还是原来的设置";
      return same({
        ...state, error,
        notice: { kind: "save_failed", message: `${error}；保存成功后才会继续，可以重试。` },
      });
    }
    case "retry_persist": {
      if (!state.pending) return same(state);
      return same(
        {
          ...state, error: null,
          notice: state.notice?.kind === "save_failed" ? null : state.notice,
        },
        [{
          type: "persist_guide", token: state.pending.token,
          update: { guide: cloneGuide(state.pending.guide) },
        }],
      );
    }
    case "health_changed": {
      const health: GuideHealth = {
        store: record(event.health) && (event.health.store === "ready" || event.health.store === "failed")
          ? event.health.store : "loading",
        capture: record(event.health) && (event.health.capture === "recording" || event.health.capture === "failed" || event.health.capture === "stopping")
          ? event.health.capture : "starting",
      };
      const next = { ...state, health, paused: event.paused === true };
      const healthProblem = healthNotice(next);
      const clearsHealthNotice = !healthProblem && state.notice
        && ["info", "paused", "store_failed", "capture_failed"].includes(state.notice.kind);
      return same({ ...next, notice: healthProblem ?? (clearsHealthNotice ? null : state.notice) });
    }
    case "copy_sample": {
      if (current.phase !== "await_copy") return same(state);
      const blocked = healthNotice(state);
      if (blocked) return noticed(state, blocked);
      return same(
        { ...state, notice: { kind: "info", message: "正在复制一句示例，真实记下后才会继续。" } },
        [{ type: "copy_sample" }],
      );
    }
    case "open_settings":
      return event.page === "general" || event.page === "privacy" || event.page === "data"
        ? same(state, [{ type: "open_settings", page: event.page }]) : same(state);
    case "dismiss_card": {
      if (terminal(current) && !terminal(state.guide)) return same(state);
      if (!cardIds.includes(event.id as GuideCardId) || current.dismissed.includes(event.id as GuideCardId)) {
        return same(state);
      }
      return save(state, { ...current, dismissed: [...current.dismissed, event.id as GuideCardId] });
    }
    default:
      return same(state);
  }
}

export const GuideFlow = {
  createState(guide?: unknown): GuideState {
    const normalized = normalizeGuide(guide);
    return normalizeState({
      guide: normalized,
      baselineReady: normalized.baseline_ms !== null || normalized.baseline_total > 0,
    });
  },
  reduce,
};
