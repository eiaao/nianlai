// 仓库根目录运行：node tools/test-guide.mjs
// Node 直接加载生产 src/lib/guide.ts；这里只组织事件，不复制状态转移表。
import assert from "node:assert/strict";
import { test } from "node:test";

const { GuideFlow } = await import("../src/lib/guide.ts");

const healthy = { store: "ready", capture: "recording" };
const oldSnapshot = {
  items: [{ id: 1, copied_ms: 100 }], total: 1, last_recorded_ms: 100,
  recording: true, store_status: null,
};
const copiedSnapshot = {
  items: [{ id: 2, copied_ms: 200 }, oldSnapshot.items[0]], total: 2, last_recorded_ms: 200,
  recording: true, store_status: null,
};

function initial(guide) {
  return GuideFlow.createState(guide);
}

function reduce(state, event) {
  const before = structuredClone(state);
  const transition = GuideFlow.reduce(state, event);
  assert.deepEqual(state, before, "reduce 不得修改输入状态");
  assert.ok(transition && transition.state && Array.isArray(transition.effects));
  return transition;
}

function request(state, event) {
  const transition = reduce(state, event);
  const save = transition.effects.find((effect) => effect.type === "persist_guide");
  assert.ok(save, `事件 ${event.type} 应产生保存副作用`);
  assert.equal(save.token, transition.state.pending?.token);
  assert.deepEqual(save.update, { guide: transition.state.pending?.guide });
  return transition;
}

function succeed(transition) {
  const token = transition.state.pending?.token;
  assert.ok(Number.isSafeInteger(token));
  return reduce(transition.state, { type: "persist_succeeded", token });
}

function advance(state, event) {
  return succeed(request(state, event)).state;
}

function readyAtCopy() {
  let state = initial();
  state = reduce(state, { type: "health_changed", health: healthy, paused: false }).state;
  state = advance(state, { type: "baseline", snapshot: oldSnapshot });
  return state;
}

function readyAtShow() {
  return advance(readyAtCopy(), { type: "history_changed", snapshot: copiedSnapshot });
}

function readyAtPaste(session = 11) {
  let state = readyAtShow();
  const opened = reduce(state, { type: "input_target_confirmed" });
  assert.ok(opened.effects.some((effect) => effect.type === "open_panel"));
  return advance(opened.state, { type: "panel_shown", session });
}

function effects(transition, type) {
  return transition.effects.filter((effect) => effect.type === type);
}

test("正常三拍只由真实记录和匹配 sent/confirmed 完成", () => {
  let state = readyAtCopy();
  assert.equal(state.guide.phase, "await_copy");
  assert.equal(state.guide.baseline_ms, 100);
  state = advance(state, { type: "history_changed", snapshot: copiedSnapshot });
  assert.equal(state.guide.phase, "await_show");
  assert.equal(state.guide.sample_id, 2);

  const noConfirmation = reduce(state, { type: "panel_shown", session: 11 });
  assert.equal(noConfirmation.state.guide.phase, "await_show");
  assert.equal(noConfirmation.state.pending, null);
  assert.equal(effects(noConfirmation, "open_panel").length, 0);

  const confirmed = reduce(state, { type: "input_target_confirmed" });
  assert.equal(confirmed.state.targetConfirmed, true);
  assert.deepEqual(effects(confirmed, "open_panel"), [{ type: "open_panel" }]);
  state = advance(confirmed.state, { type: "panel_shown", session: 11 });
  assert.equal(state.guide.phase, "await_paste");
  assert.equal(state.session, 11);

  const hidden = reduce(state, { type: "panel_hidden", session: 11 });
  assert.equal(hidden.state.session, 11, "生产先收窗时仍须保留本次会话以接收随后的粘贴结论");
  const finishing = request(hidden.state, { type: "paste_result", session: 11, id: 2, result: "sent" });
  assert.equal(finishing.state.guide.phase, "await_paste", "保存前不能可见完成");
  assert.equal(finishing.state.pending.guide.phase, "completed");
  assert.equal(finishing.state.pending.guide.cards_dealt, true);
  assert.equal(effects(finishing, "deal_cards").length, 0);
  assert.equal(effects(finishing, "close_guide").length, 0);
  const done = succeed(finishing);
  assert.equal(done.state.guide.phase, "completed");
  assert.equal(done.state.guide.cards_dealt, true);
  assert.equal(done.state.guide.sample_id, null);
  assert.deepEqual(effects(done, "deal_cards"), [{ type: "deal_cards" }]);
  assert.deepEqual(effects(done, "close_guide"), [{ type: "close_guide" }]);
  const late = reduce(done.state, { type: "paste_result", session: 11, id: 2, result: "sent" });
  assert.equal(late.state.pending, null);
  assert.equal(late.effects.length, 0);
});

test("重复复制跟踪最新样本，旧历史变化、错误会话和 ID 均不推进", () => {
  let state = readyAtShow();
  const newer = {
    ...copiedSnapshot,
    items: [{ id: 3, copied_ms: 300 }, ...copiedSnapshot.items],
    total: 3,
    last_recorded_ms: 300,
  };
  state = advance(state, { type: "history_changed", snapshot: newer });
  assert.equal(state.guide.sample_id, 3);
  const stale = reduce(state, { type: "history_changed", snapshot: copiedSnapshot });
  assert.equal(stale.state.pending, null);
  assert.equal(stale.state.guide.sample_id, 3);

  state = reduce(state, { type: "input_target_confirmed" }).state;
  state = advance(state, { type: "panel_shown", session: 20 });
  for (const event of [
    { type: "paste_result", session: 19, id: 3, result: "sent" },
    { type: "paste_result", session: 20, id: 2, result: "confirmed" },
    { type: "paste_result", session: 20, id: 3, result: "copied" },
  ]) {
    const rejected = reduce(state, event);
    assert.equal(rejected.state.guide.phase, "await_paste");
    assert.equal(rejected.state.pending, null);
  }
  const accepted = request(state, { type: "paste_result", session: 20, id: 3, result: "confirmed" });
  assert.equal(accepted.state.pending.guide.phase, "completed");
});

test("保存中的重复复制由新 token 接管，迟到成功不覆盖最新样本", () => {
  const state = readyAtCopy();
  const first = request(state, { type: "history_changed", snapshot: copiedSnapshot });
  const newer = {
    ...copiedSnapshot,
    items: [{ id: 3, copied_ms: 300 }, ...copiedSnapshot.items],
    total: 3,
    last_recorded_ms: 300,
  };
  const second = request(first.state, { type: "history_changed", snapshot: newer });
  assert.notEqual(first.state.pending.token, second.state.pending.token);
  assert.equal(second.state.guide.phase, "await_copy");
  assert.equal(second.state.pending.guide.sample_id, 3);
  const stale = reduce(second.state, { type: "persist_succeeded", token: first.state.pending.token });
  assert.equal(stale.state.guide.phase, "await_copy");
  assert.equal(stale.state.pending.token, second.state.pending.token);
  const latest = reduce(stale.state, { type: "persist_succeeded", token: second.state.pending.token });
  assert.equal(latest.state.guide.phase, "await_show");
  assert.equal(latest.state.guide.sample_id, 3);
});

test("copied/copied_only/native_submission_unknown 各有准确的可重试说明，前置 hidden 不吞结果", () => {
  for (const [result, expected, forbidden] of [
    ["copied", "刚才只复制了内容", "没能确认"],
    ["copied_only", "刚才只复制了内容", "没能确认"],
    ["native_submission_unknown", "没能确认是否贴出", "刚才只复制了内容"],
  ]) {
    const hidden = reduce(readyAtPaste(), { type: "panel_hidden", session: 11 });
    const transition = reduce(hidden.state, { type: "paste_result", session: 11, id: 2, result });
    assert.equal(transition.state.guide.phase, "await_paste");
    assert.equal(transition.state.pending, null);
    assert.match(transition.state.notice?.message ?? "", new RegExp(expected));
    assert.doesNotMatch(transition.state.notice?.message ?? "", new RegExp(forbidden));
    assert.equal(transition.effects.length, 0);
  }
});

test("第三拍跳过本步与整份跳过是不同终态，且都等待持久化", () => {
  const atPaste = readyAtPaste();
  const skippingStep = request(atPaste, { type: "skip_step" });
  assert.equal(skippingStep.state.guide.phase, "await_paste");
  assert.equal(skippingStep.state.pending.guide.phase, "completed");
  assert.equal(skippingStep.state.pending.guide.cards_dealt, true);
  const completed = succeed(skippingStep);
  assert.equal(effects(completed, "deal_cards").length, 1);
  assert.equal(effects(completed, "close_guide").length, 1);

  for (const type of ["skip_guide", "close_guide"]) {
    const skipping = request(readyAtShow(), { type });
    assert.equal(skipping.state.guide.phase, "await_show");
    assert.equal(skipping.state.pending.guide.phase, "skipped");
    assert.equal(skipping.state.pending.guide.cards_dealt, false);
    const skipped = succeed(skipping);
    assert.equal(skipped.state.guide.phase, "skipped");
    assert.equal(effects(skipped, "deal_cards").length, 0);
    assert.equal(effects(skipped, "close_guide").length, 1);
    const reopened = reduce(skipped.state, { type: "history_changed", snapshot: copiedSnapshot });
    assert.equal(reopened.state.guide.phase, "skipped");
    assert.equal(reopened.effects.length, 0);
  }
});

test("保存失败保留当前拍和候选状态，重试成功才完成并发牌", () => {
  const state = readyAtPaste();
  const finishing = request(state, { type: "paste_result", session: 11, id: 2, result: "sent" });
  const token = finishing.state.pending.token;
  const failed = reduce(finishing.state, { type: "persist_failed", token, message: "磁盘不让写" });
  assert.equal(failed.state.guide.phase, "await_paste");
  assert.equal(failed.state.pending.guide.phase, "completed");
  assert.match(failed.state.error ?? "", /磁盘不让写/);
  assert.match(failed.state.notice?.message ?? "", /重试/);
  assert.equal(failed.effects.length, 0);

  const stale = reduce(failed.state, { type: "persist_succeeded", token: token + 100 });
  assert.equal(stale.state.guide.phase, "await_paste");
  const explicitRetry = reduce(stale.state, { type: "retry_persist" });
  assert.equal(effects(explicitRetry, "persist_guide").length, 1);
  const retry = reduce(explicitRetry.state, { type: "skip_step" });
  assert.equal(effects(retry, "persist_guide").length, 1, "保存失败后第三拍跳过本步仍可重试");
  const done = reduce(retry.state, { type: "persist_succeeded", token });
  assert.equal(done.state.guide.phase, "completed");
  assert.equal(done.state.error, null);
  assert.equal(effects(done, "deal_cards").length, 1);
  assert.equal(effects(done, "close_guide").length, 1);
});

test("中途恢复保留当前拍；样本已删除则回第一拍并提示", () => {
  const atPaste = readyAtPaste(31);
  const restored = reduce(atPaste, { type: "restore", snapshot: copiedSnapshot });
  assert.equal(restored.state.guide.phase, "await_paste");
  assert.equal(restored.state.guide.sample_id, 2);
  assert.equal(restored.state.session, null);
  assert.equal(restored.state.targetConfirmed, false);

  const missing = request(atPaste, { type: "restore", snapshot: oldSnapshot });
  assert.equal(missing.state.pending.guide.phase, "await_copy");
  assert.equal(missing.state.pending.guide.sample_id, null);
  assert.equal(missing.state.notice?.message, "刚才那条不在了，再复制一句");
  const recovered = succeed(missing);
  assert.equal(recovered.state.guide.phase, "await_copy");
  assert.equal(recovered.state.notice?.message, "刚才那条不在了，再复制一句");
});

test("面板开了但没贴就收起可重试；生产 hidden→result 仍按本次会话完成", () => {
  let state = readyAtPaste(41);
  const hidden = reduce(state, { type: "panel_hidden", session: 41 });
  assert.equal(hidden.state.guide.phase, "await_paste");
  assert.equal(hidden.state.session, 41);
  assert.equal(hidden.state.targetConfirmed, false);
  assert.match(hidden.state.notice?.message ?? "", /面板已收起/);
  const finished = request(hidden.state, { type: "paste_result", session: 41, id: 2, result: "sent" });
  assert.equal(finished.state.pending.guide.phase, "completed");

  state = reduce(hidden.state, { type: "input_target_confirmed" }).state;
  assert.equal(state.session, null, "用户选择重新呼出后才作废旧会话");
  const reopened = reduce(state, { type: "panel_shown", session: 42 });
  assert.equal(reopened.state.guide.phase, "await_paste");
  assert.equal(reopened.state.session, 42);
  state = reopened.state;
  const late = reduce(state, { type: "paste_result", session: 41, id: 2, result: "sent" });
  assert.equal(late.state.pending, null);
  const currentHidden = reduce(state, { type: "panel_hidden", session: 42 });
  const current = request(currentHidden.state, { type: "paste_result", session: 42, id: 2, result: "confirmed" });
  assert.equal(current.state.pending.guide.phase, "completed");
});

test("hidden → 重新确认后旧 sent/confirmed 均在新 shown 前拒绝", () => {
  const hidden = reduce(readyAtPaste(41), { type: "panel_hidden", session: 41 });
  assert.equal(hidden.state.session, 41, "hidden 先保留本次会话等待粘贴结论");
  const retry = reduce(hidden.state, { type: "input_target_confirmed" });
  assert.equal(retry.state.targetConfirmed, true);
  assert.deepEqual(effects(retry, "open_panel"), [{ type: "open_panel" }]);

  const lateResults = ["sent", "confirmed"].map((result) => {
    const late = reduce(retry.state, { type: "paste_result", session: 41, id: 2, result });
    return {
      result,
      phase: late.state.guide.phase,
      session: late.state.session,
      pendingPhase: late.state.pending?.guide.phase ?? "",
      effects: late.effects.map((effect) => effect.type),
    };
  });
  assert.deepEqual(lateResults, ["sent", "confirmed"].map((result) => ({
    result, phase: "await_paste", session: null, pendingPhase: "", effects: [],
  })), "重新确认后，旧 sent/confirmed 都不得在新 shown 前保存、发牌或关闭");
  assert.equal(retry.state.session, null, "重新确认输入位置必须立即使旧会话失效");

  const reopened = reduce(retry.state, { type: "panel_shown", session: 42 });
  assert.equal(reopened.state.session, 42, "只有真实的新 session 可以接管");
  const currentHidden = reduce(reopened.state, { type: "panel_hidden", session: 42 });
  const current = request(currentHidden.state, { type: "paste_result", session: 42, id: 2, result: "sent" });
  assert.equal(current.state.pending.guide.phase, "completed");
});

test("session 严格单调：12 之后 shown=11/12 都不能重冻结，只有 13 能接管", () => {
  for (const interruption of ["panel_hidden", "paste_result"]) {
    let state = readyAtPaste(12);
    state = reduce(state, interruption === "panel_hidden"
      ? { type: "panel_hidden", session: 12 }
      : { type: "paste_result", session: 12, id: 2, result: "copied_only" }).state;
    state = reduce(state, { type: "input_target_confirmed" }).state;
    assert.equal(state.session, null);
    for (const session of [11, 12]) {
      const stale = reduce(state, { type: "panel_shown", session });
      assert.equal(stale.state.session, null, `${interruption}: shown=${session} 不得回退高水位`);
      assert.equal(stale.state.lastSession, 12);
      const paste = reduce(stale.state, { type: "paste_result", session, id: 2, result: "sent" });
      assert.equal(paste.state.pending, null);
      assert.equal(effects(paste, "deal_cards").length, 0);
    }
    const current = reduce(state, { type: "panel_shown", session: 13 });
    assert.equal(current.state.session, 13);
    assert.equal(current.state.lastSession, 13);
    const duplicate = reduce(current.state, { type: "panel_shown", session: 13 });
    assert.equal(duplicate.state.session, 13);
    assert.equal(duplicate.effects.length, 0);
    const wrongId = reduce(current.state, { type: "paste_result", session: 13, id: 1, result: "sent" });
    assert.equal(wrongId.state.pending, null);
    const hidden = reduce(current.state, { type: "panel_hidden", session: 13 });
    const done = request(hidden.state, { type: "paste_result", session: 13, id: 2, result: "confirmed" });
    assert.equal(done.state.pending.guide.phase, "completed");
  }
});

test("暂停、存储/记录故障不伪造复制，并提供对应设置入口", () => {
  for (const [health, paused, page] of [
    [healthy, true, "privacy"],
    [{ store: "failed", capture: "recording" }, false, "data"],
    [{ store: "ready", capture: "failed" }, false, "privacy"],
  ]) {
    let state = reduce(initial(), { type: "health_changed", health, paused }).state;
    assert.equal(state.notice?.settingsPage, page);
    const sample = reduce(state, { type: "copy_sample" });
    assert.equal(effects(sample, "copy_sample").length, 0);
    const copied = reduce(sample.state, { type: "history_changed", snapshot: copiedSnapshot });
    assert.equal(copied.state.guide.phase, "await_copy");
    assert.equal(copied.state.pending, null);
    const settings = reduce(copied.state, { type: "open_settings", page });
    assert.deepEqual(settings.effects, [{ type: "open_settings", page }]);
  }
  const ready = reduce(initial(), { type: "health_changed", health: healthy, paused: false });
  const sample = reduce(ready.state, { type: "copy_sample" });
  assert.deepEqual(effects(sample, "copy_sample"), [{ type: "copy_sample" }]);
  assert.equal(sample.state.guide.phase, "await_copy", "请求示例复制不等于已经记下");
});

test("dismissed 去重、保持顺序并忽略非法牌 ID", () => {
  let state = initial();
  state = advance(state, { type: "dismiss_card", id: "t2" });
  state = advance(state, { type: "dismiss_card", id: "t1" });
  const duplicate = reduce(state, { type: "dismiss_card", id: "t2" });
  assert.equal(duplicate.state.pending, null);
  assert.deepEqual(duplicate.state.guide.dismissed, ["t2", "t1"]);
  for (const id of ["t5", "", 1, null]) {
    const invalid = reduce(state, { type: "dismiss_card", id });
    assert.equal(invalid.state.pending, null);
    assert.deepEqual(invalid.state.guide.dismissed, ["t2", "t1"]);
  }
});

test("损坏状态、非法数值和未知事件安全降级，不抛异常", () => {
  const broken = {
    guide: { version: -1, phase: "await_paste", baseline_ms: -1, baseline_total: -9,
      sample_id: 0, cards_dealt: "yes", dismissed: ["t1", "t1", "bad"] },
    session: -4, targetConfirmed: "yes", nextToken: -1,
  };
  const state = initial(broken.guide);
  assert.equal(state.guide.phase, "await_copy");
  assert.equal(state.guide.sample_id, null);
  assert.deepEqual(state.guide.dismissed, ["t1"]);
  assert.doesNotThrow(() => GuideFlow.reduce(broken, { type: "paste_result", session: NaN, id: -1, result: "sent" }));
  assert.doesNotThrow(() => GuideFlow.reduce(null, null));
  const safe = reduce(state, { type: "not_a_real_event" });
  assert.equal(safe.state.guide.phase, "await_copy");
  assert.equal(safe.state.pending, null);
});

test("restore 保留在途完成候选，成功/失败回包都不会丢失或重复发牌关闭", () => {
  for (const mode of ["sent", "skip_step"]) {
    const event = mode === "sent"
      ? { type: "paste_result", session: 11, id: 2, result: "sent" }
      : { type: "skip_step" };
    const finishing = request(readyAtPaste(), event);
    const token = finishing.state.pending.token;
    const restored = reduce(finishing.state, { type: "restore", snapshot: copiedSnapshot });
    assert.equal(restored.state.guide.phase, "await_paste");
    assert.equal(restored.state.pending?.token, token, `${mode}: restore 不能丢完成 token`);
    assert.equal(restored.state.pending?.guide.phase, "completed");
    assert.equal(restored.state.session, null);
    assert.equal(restored.state.targetConfirmed, false);
    assert.equal(restored.effects.length, 0);

    const done = reduce(restored.state, { type: "persist_succeeded", token });
    assert.equal(done.state.guide.phase, "completed");
    assert.equal(done.state.pending, null);
    assert.equal(effects(done, "deal_cards").length, 1);
    assert.equal(effects(done, "close_guide").length, 1);
    const duplicate = reduce(done.state, { type: "persist_succeeded", token });
    assert.equal(duplicate.effects.length, 0);

    const failed = reduce(restored.state, { type: "persist_failed", token, message: `${mode} 保存失败` });
    assert.equal(failed.state.guide.phase, "await_paste");
    assert.equal(failed.state.pending?.token, token);
    assert.match(failed.state.error ?? "", new RegExp(`${mode} 保存失败`));
    assert.equal(effects(failed, "deal_cards").length, 0);
    assert.equal(effects(failed, "close_guide").length, 0);
    const retry = reduce(failed.state, { type: "retry_persist" });
    assert.equal(effects(retry, "persist_guide").length, 1);
    const retried = reduce(retry.state, { type: "persist_succeeded", token });
    assert.equal(effects(retried, "deal_cards").length, 1);
    assert.equal(effects(retried, "close_guide").length, 1);
    assert.equal(reduce(retried.state, { type: "persist_succeeded", token }).effects.length, 0);
  }
});

test("restore 保留在途整份跳过，失败可重试且始终不发牌", () => {
  const skipping = request(readyAtShow(), { type: "skip_guide" });
  const token = skipping.state.pending.token;
  const restored = reduce(skipping.state, { type: "restore", snapshot: copiedSnapshot });
  assert.equal(restored.state.guide.phase, "await_show");
  assert.equal(restored.state.pending?.token, token);
  assert.equal(restored.state.pending?.guide.phase, "skipped");

  const done = reduce(restored.state, { type: "persist_succeeded", token });
  assert.equal(done.state.guide.phase, "skipped");
  assert.equal(effects(done, "deal_cards").length, 0);
  assert.equal(effects(done, "close_guide").length, 1);
  assert.equal(reduce(done.state, { type: "persist_succeeded", token }).effects.length, 0);

  const failed = reduce(restored.state, { type: "persist_failed", token, message: "跳过保存失败" });
  assert.equal(failed.state.guide.phase, "await_show");
  assert.equal(failed.state.pending?.guide.phase, "skipped");
  assert.match(failed.state.error ?? "", /跳过保存失败/);
  assert.equal(failed.effects.length, 0);
  const retry = reduce(failed.state, { type: "retry_persist" });
  const retried = reduce(retry.state, { type: "persist_succeeded", token });
  assert.equal(effects(retried, "deal_cards").length, 0);
  assert.equal(effects(retried, "close_guide").length, 1);
});

test("restore 与普通 pending save 交错时保留有效 token，样本删除则用新候选补偿", () => {
  const pending = request(readyAtCopy(), { type: "history_changed", snapshot: copiedSnapshot });
  const token = pending.state.pending.token;
  const restored = reduce(pending.state, { type: "restore", snapshot: copiedSnapshot });
  assert.equal(restored.state.pending?.token, token);
  assert.equal(restored.state.pending?.guide.phase, "await_show");
  assert.equal(restored.state.guide.phase, "await_copy");

  const saved = reduce(restored.state, { type: "persist_succeeded", token });
  assert.equal(saved.state.guide.phase, "await_show");
  assert.equal(saved.state.guide.sample_id, 2);
  assert.equal(saved.effects.length, 0);
  const failed = reduce(restored.state, { type: "persist_failed", token, message: "普通保存失败" });
  assert.equal(failed.state.guide.phase, "await_copy");
  assert.equal(failed.state.pending?.guide.phase, "await_show");
  assert.match(failed.state.error ?? "", /普通保存失败/);
  const retry = reduce(failed.state, { type: "retry_persist" });
  assert.equal(effects(retry, "persist_guide").length, 1);

  const missing = request(pending.state, { type: "restore", snapshot: oldSnapshot });
  const replacement = missing.state.pending.token;
  assert.notEqual(replacement, token);
  assert.equal(missing.state.pending.guide.phase, "await_copy");
  assert.equal(missing.state.pending.guide.sample_id, null);
  assert.equal(missing.state.notice?.message, "刚才那条不在了，再复制一句");
  const stale = reduce(missing.state, { type: "persist_succeeded", token });
  assert.equal(stale.state.pending?.token, replacement);
  assert.equal(stale.state.guide.phase, "await_copy");
  const recovered = reduce(stale.state, { type: "persist_succeeded", token: replacement });
  assert.equal(recovered.state.guide.phase, "await_copy");
  assert.equal(recovered.state.guide.sample_id, null);
  assert.equal(recovered.effects.length, 0);
});

test("默认 await_copy 恢复会持久化可信基线，相同快照不算新复制", () => {
  let state = reduce(initial(), { type: "health_changed", health: healthy, paused: false }).state;
  const restored = request(state, { type: "restore", snapshot: oldSnapshot });
  assert.equal(restored.state.pending.guide.phase, "await_copy");
  assert.equal(restored.state.pending.guide.baseline_ms, 100);
  assert.equal(restored.state.pending.guide.baseline_total, 1);
  const token = restored.state.pending.token;
  const unchanged = reduce(restored.state, { type: "history_changed", snapshot: oldSnapshot });
  assert.equal(unchanged.state.pending?.token, token);
  assert.equal(unchanged.state.pending?.guide.phase, "await_copy");
  assert.equal(unchanged.state.pending?.guide.sample_id, null);
  assert.equal(unchanged.effects.length, 0);

  state = reduce(unchanged.state, { type: "persist_succeeded", token }).state;
  assert.equal(state.guide.baseline_ms, 100);
  const stillUnchanged = reduce(state, { type: "history_changed", snapshot: oldSnapshot });
  assert.equal(stillUnchanged.state.pending, null);
  const updated = request(state, { type: "history_changed", snapshot: copiedSnapshot });
  assert.equal(updated.state.pending.guide.phase, "await_show");
  assert.equal(updated.state.pending.guide.sample_id, 2);
});

test("缺基线 await_show 恢复不换掉仍在的样本，真正新记录才推进", () => {
  let state = initial({ phase: "await_show", sample_id: 1, baseline_ms: null, baseline_total: 0 });
  state = reduce(state, { type: "health_changed", health: healthy, paused: false }).state;
  const restored = request(state, { type: "restore", snapshot: copiedSnapshot });
  const token = restored.state.pending.token;
  assert.equal(restored.state.pending.guide.phase, "await_show");
  assert.equal(restored.state.pending.guide.sample_id, 1);
  assert.equal(restored.state.pending.guide.baseline_ms, 200);
  assert.equal(restored.state.pending.guide.baseline_total, 2);
  const unchanged = reduce(restored.state, { type: "history_changed", snapshot: copiedSnapshot });
  assert.equal(unchanged.state.pending?.token, token);
  assert.equal(unchanged.state.pending?.guide.sample_id, 1);
  assert.equal(unchanged.effects.length, 0);

  state = reduce(unchanged.state, { type: "persist_succeeded", token }).state;
  const newer = {
    ...copiedSnapshot,
    items: [{ id: 3, copied_ms: 300 }, ...copiedSnapshot.items],
    total: 3,
    last_recorded_ms: 300,
  };
  const updated = request(state, { type: "history_changed", snapshot: newer });
  assert.equal(updated.state.pending.guide.sample_id, 3);
});

test("paste_result 拒绝 null 和其他非法 session，hidden 后合法会话仍可完成", () => {
  const hidden = reduce(readyAtPaste(), { type: "panel_hidden", session: 11 });
  assert.equal(hidden.state.session, 11);
  for (const session of [null, undefined, 0, -1, 1.5, NaN, Infinity, "11"]) {
    const invalid = reduce(hidden.state, { type: "paste_result", session, id: 2, result: "sent" });
    assert.equal(invalid.state.guide.phase, "await_paste");
    assert.equal(invalid.state.pending, null, `非法 session 应忽略：${String(session)}`);
    assert.equal(effects(invalid, "persist_guide").length, 0);
  }
  const legal = request(hidden.state, { type: "paste_result", session: 11, id: 2, result: "confirmed" });
  assert.equal(legal.state.pending.guide.phase, "completed");
});

for (const phase of ["await_show", "await_paste"]) {
  test(`无基线 ${phase} 首观察发现样本删除时回 await_copy`, () => {
    let state = initial({ phase, sample_id: 2, baseline_ms: null, baseline_total: 0 });
    state = reduce(state, { type: "health_changed", health: healthy, paused: false }).state;
    assert.equal(state.baselineReady, false, "必须走 history_changed → baseline 首观察路径");
    const recovering = request(state, { type: "history_changed", snapshot: oldSnapshot });
    assert.equal(recovering.state.pending.guide.phase, "await_copy");
    assert.equal(recovering.state.pending.guide.sample_id, null);
    assert.equal(recovering.state.pending.guide.baseline_ms, 100);
    assert.equal(recovering.state.pending.guide.baseline_total, 1);
    assert.equal(recovering.state.pending.guide.cards_dealt, false);
    assert.equal(recovering.state.notice?.kind, "sample_missing");
    assert.equal(recovering.state.notice?.message, "刚才那条不在了，再复制一句");

    const saved = succeed(recovering);
    assert.equal(saved.state.guide.phase, "await_copy");
    assert.equal(saved.state.guide.sample_id, null);
    assert.equal(saved.state.notice?.message, "刚才那条不在了，再复制一句");
    const opened = reduce(saved.state, { type: "input_target_confirmed" });
    assert.equal(opened.state.targetConfirmed, false);
    assert.equal(effects(opened, "open_panel").length, 0, "已删除样本不能再呼出面板");
    assert.equal(opened.effects.length, 0);
  });
}

test("About 显式重发可保留 skipped+cards_dealt=true，进行中和普通跳过仍只能为 false", () => {
  const skipped = initial({ phase: "skipped", cards_dealt: true, dismissed: [] });
  assert.equal(skipped.guide.phase, "skipped");
  assert.equal(skipped.guide.cards_dealt, true);
  const completed = initial({ phase: "completed", cards_dealt: true });
  assert.equal(completed.guide.cards_dealt, true);
  for (const phase of ["await_copy", "await_show", "await_paste"]) {
    assert.equal(initial({ phase, sample_id: phase === "await_copy" ? null : 1, cards_dealt: true }).guide.cards_dealt, false);
  }
  const skippedAgain = advance(readyAtShow(), { type: "skip_guide" });
  assert.equal(skippedAgain.guide.phase, "skipped");
  assert.equal(skippedAgain.guide.cards_dealt, false);
});

// 真实 Svelte 组件、生产 IPC 执行器和三态截图；仍只使用内存 Tauri 替身。
await import("./test-guide-component.mjs");
