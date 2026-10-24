#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""g0_matrix_report.py — 焦点矩阵手测的日志汇总：把样机的 panel-log.jsonl 按“一次呼出一次试验”整理，
输出中文 Markdown。

给 PRD 6.4 焦点验收矩阵的手工部分用（第 4–7 行）。只依赖 Python 3 标准库；截取最后一次启动、
百分位数、表格和错误事件判断复用 g0_log_report.py。

- 一次呼出（一个 session）算一次试验。session 号每次启动都从 1 数起，所以按（第几次启动，session）区分。
  paste 事件可能晚于下一次呼出才写出，按 session 号归位，不按先后。
- 模式：出现过 search_ready 或 search_foreground（已经进了搜索）就算搜索模式，否则是快速取用。
  只看 search_ready 的话，进搜索失败、自动收起的那次会被算成快速取用。
- 按（目标进程，模式）分组。

只输出这些字段：进程名、窗口类名、焦点控件类名、结论和原因这类枚举值、计数和耗时。日志本身不含
剪贴板内容和窗口标题，脚本也不输出任何没列出的字段。

日志判断不了“误提交”，也看不出粘进去的内容对不对，这两项要人工记。
"""

import argparse
import json
import os
import sys
from collections import Counter, OrderedDict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import g0_log_report as base  # noqa: E402

QUICK = "快速取用"
SEARCH = "搜索模式"
DASH = "—"
NO_DATA = "无"
UNKNOWN = "(未知)"
NOT_RECORDED = "(未记录)"
RESULTS = ("confirmed", "sent", "copied", "copied_only")

COMMIT_NOTE = (
    "> 日志只记样机自己做了什么，判断不了“误提交”（回车漏给目标、消息被发出去、单元格被确认），"
    "也看不出粘进去的内容对不对、搜索框有没有丢字。这三项要按记录表人工记。"
    "sent 只说明粘贴键发出去了，不代表内容进去了。"
)

REASON_TEXT = {
    "user_copy": "按了只复制（Shift+Enter）",
    "external_switch": "选好时前台已不是目标窗口，没发粘贴键",
    "invalid_target": "目标窗口已经关了或换了",
    "target_may_be_elevated": "目标以管理员身份运行（或查不到它的权限），只复制",
    "preflight_timeout": "发键前 300 ms 内确认没过（前台、焦点、修饰键、面板隐藏）",
    "conditions_changed": "写剪贴板前后条件变了，没发粘贴键",
    "send_failed": "粘贴键没有全部发出去",
    "record_missing": "这条记录已不在历史里",
    "store_read_failed": "读取历史失败，没改剪贴板",
    "request_cancelled": "样机正在退出，粘贴取消",
    "clipboard_busy": "剪贴板被别的程序占着",
}

HIDE_TEXT = {
    "escape_key": "快速取用里按 Esc",
    "frontend": "面板发起的收起，多为搜索模式里按 Esc",
    "outside_click": "点了面板外面",
    "hotkey": "再按一次呼出键",
    "search_activation_failed": "进搜索模式时面板没拿到前台，自动收起",
    None: "日志里没有收起来源（面板可能还开着，或样机退出时收起）",
}

KEY_SOURCE = {"builtin": "内置", "override": "覆盖表"}
AS_TEXT_CONVERSION = {"paths": "路径", "image_path": "PNG 路径", "plain_text": "去格式"}
AS_TEXT_TRIGGER = {"manual": "Ctrl+Enter", "rule": "键表"}


# ---------------------------------------------------------------------------
# 基础工具
# ---------------------------------------------------------------------------

def default_log():
    """和样机一致：设了 NIANLAI_G0_DATA_DIR 就用其中的日志，否则用 g0_log_report 的默认位置。"""
    data_dir = os.environ.get("NIANLAI_G0_DATA_DIR")
    if data_dir:
        return os.path.join(data_dir, "panel-log.jsonl")
    return base.DEFAULT_LOG


def _is_num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def _text(v):
    """只认非空字符串，其余返回 None。"""
    return v if isinstance(v, str) and v else None


def _sub(obj, key):
    """取子对象；不存在、为 null 或不是字典都返回 {}。"""
    value = obj.get(key) if isinstance(obj, dict) else None
    return value if isinstance(value, dict) else {}


def _texts(value):
    """字符串列表，非字符串的项丢掉。"""
    if not isinstance(value, list):
        return []
    return [v for v in value if isinstance(v, str) and v]


def _unique(items):
    seen = []
    for item in items:
        if item not in seen:
            seen.append(item)
    return seen


def cell(value):
    """表格单元格：空值显示为 —，去掉会破坏表格的 | 和换行。"""
    if value is None or value == "":
        return DASH
    text = str(value)
    return text.replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def fmt_ms(value):
    return "%.1f" % value if _is_num(value) else DASH


# ---------------------------------------------------------------------------
# 读取
# ---------------------------------------------------------------------------

def load_events(path, from_line=None, to_line=None):
    """读取 JSONL，返回 (events, bad_lines)。规则同 g0_log_report.load_events，多一个结束行。"""
    events = []
    bad = 0
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line_no, raw in enumerate(f, 1):
            if from_line is not None and line_no < from_line:
                continue
            if to_line is not None and line_no > to_line:
                break
            text = raw.strip()
            if not text:
                continue
            try:
                obj = json.loads(text)
            except ValueError:
                bad += 1
                continue
            if isinstance(obj, dict) and "event" in obj:
                events.append((line_no, obj))
            else:
                bad += 1
    return events, bad


# ---------------------------------------------------------------------------
# 一次呼出 = 一次试验
# ---------------------------------------------------------------------------

class Trial(object):
    """一次呼出。只从日志里取白名单字段。"""

    def __init__(self, run, session, line):
        self.run = run
        self.session = session
        self.line = line              # 范围内这个 session 的第一行
        self.started = False          # 范围内见到了 hotkey 或 shown
        self.auto = False             # 自动显示（NIANLAI_G0_AUTOCYCLE），不算试验
        self.process = None
        self.focus_class = None
        self.window_class = None
        self.search_entered = False
        self.ready = []               # 每条 search_ready 的 search_box_ready
        self.replays = []             # search_replay
        self.first_input = None       # search_first_input
        self.hide_sources = []
        self.focus = None             # 最后一条 focus_restored
        self.switches = []            # paste_switch
        self.paste = None
        self.errors = []
        self.ordinal = None           # 组内第几次粘贴，分组时填
        self._flags = None

    def _target(self, obj):
        name = obj.get("process_name")
        if self.process is None and isinstance(name, str):
            self.process = name
        focus_class = obj.get("focus_class")
        if self.focus_class is None and isinstance(focus_class, str):
            self.focus_class = focus_class

    def absorb(self, name, obj):
        if name in ("hotkey", "shown"):
            self.started = True
            if name == "shown" and obj.get("auto") is True:
                self.auto = True
            self._target(obj)
        elif name in ("search_foreground", "search_foreground_wait"):
            self.search_entered = True
        elif name == "search_ready":
            self.search_entered = True
            self.ready.append(obj.get("search_box_ready") is True)
        elif name == "search_replay":
            self.replays.append({
                "replayed": obj.get("replayed_keys") if _is_int(obj.get("replayed_keys")) else 0,
                "dropped": obj.get("dropped_keys") if _is_int(obj.get("dropped_keys")) else 0,
                "wait_ms": obj.get("wait_ms"),
                "reason": _text(obj.get("reason")),
            })
        elif name == "search_first_input":
            if self.first_input is None:
                self.first_input = {
                    "input_type": _text(obj.get("input_type")),
                    "composing": obj.get("composing") is True,
                }
        elif name == "hide_source":
            self.hide_sources.append(_text(obj.get("source")))
        elif name == "focus_restored":
            self.focus = {
                "foreground_ok": obj.get("foreground_ok") is True,
                "focus_restored": obj.get("focus_restored") is True,
                "focus_unknown": obj.get("focus_unknown") is True,
                "external_switch": obj.get("external_switch") is True,
            }
        elif name == "paste_switch":
            self.switches.append({
                "detected_by": _text(obj.get("detected_by")),
                "switched_class": _text(obj.get("switched_class")),
                "switched_process": _text(obj.get("switched_process")),
            })
        elif name == "paste":
            if self.paste is None:
                self.paste = obj
                self._target(obj)
                self.window_class = _text(obj.get("window_class"))
        if base.is_error_event(name):
            self.errors.append(name)

    # ---- 派生字段 ----

    @property
    def mode(self):
        return SEARCH if self.search_entered else QUICK

    @property
    def hide_source(self):
        """第一条收起来源。快速取用里按 Esc 后前端还会再报一次 frontend，以第一条为准。"""
        for source in self.hide_sources:
            if source:
                return source
        return None

    @property
    def pasted(self):
        return self.paste is not None or "paste" in self.hide_sources

    @property
    def result(self):
        return _text(self.paste.get("paste_result")) if self.paste is not None else None

    @property
    def reason(self):
        return _text(self.paste.get("reason")) if self.paste is not None else None

    @property
    def sent(self):
        """粘贴键发出去了（actual_key 只在发键后才写）。"""
        return self.paste is not None and _text(self.paste.get("actual_key")) is not None

    def failed_checks(self):
        return _texts(_sub(self.paste, "preflight").get("failed_checks"))

    def focus_state(self):
        """(显示文本, 是否算“焦点没还原”)。

        搜索模式粘贴和收起时看还原的结果；快速取用粘贴时面板不拿焦点，看发键前的确认。
        发键前确认没过会降级为 copied_only，原因里已经写了，这里不重复算。
        """
        if self.pasted:
            if self.paste is None:
                return DASH, False
            restore = _sub(self.paste, "restore")
            if restore:
                return _restored(_sub(restore, "foreground").get("success") is True,
                                 restore.get("focus_restored") is True,
                                 restore.get("focus_unknown") is True)
            preflight = _sub(self.paste, "preflight")
            if preflight:
                failed = [c for c in self.failed_checks() if c in ("foreground", "focus")]
                if failed:
                    return "发键前确认没过：" + "、".join(failed), False
                return "发键前确认通过", False
            return DASH, False
        if self.focus:
            if self.focus["external_switch"]:
                return "没还原：前台已是别的窗口", False
            return _restored(self.focus["foreground_ok"], self.focus["focus_restored"],
                             self.focus["focus_unknown"])
        return DASH, False

    def flags(self):
        """可疑项 [(名称, 详情)]。可疑不等于失败，要对照手工记录看。"""
        if self._flags is not None:
            return self._flags
        out = []
        result, reason = self.result, self.reason
        if self.pasted and result is None:
            if self.paste is None:
                out.append(("无结论", "没有 paste 事件（可能在范围之外，或样机在粘贴途中退出）"))
            else:
                out.append(("无结论", reason or "paste_result 为空"))
        if result == "copied_only":
            detail = reason or NOT_RECORDED
            if reason == "preflight_timeout" and self.failed_checks():
                detail += "（没过：%s）" % "、".join(self.failed_checks())
            out.append(("copied_only", detail))
        if not self.pasted and self.focus and self.focus["external_switch"]:
            out.append(("external_switch",
                        "收起时前台已是别的窗口（收起来源 %s）" % (self.hide_source or NOT_RECORDED)))
        if self.switches:
            out.append(("paste_switch", "；".join(_switch_text(s, True) for s in self.switches)))
        if self.paste is not None and _sub(self.paste, "layer2").get("status") == "timeout":
            out.append(("第 2 层超时", "1 秒内没有程序读剪贴板，多半没粘进去"))
        focus_text, lost = self.focus_state()
        if lost:
            detail = focus_text
            if not self.pasted and self.hide_source:
                detail += "（收起来源 %s" % self.hide_source
                if self.hide_source == "outside_click":
                    detail += "，点到别的窗口时属正常"
                detail += "）"
            out.append(("焦点没还原", detail))
        dropped = sum(r["dropped"] for r in self.replays)
        if dropped:
            reasons = _unique(r["reason"] or NOT_RECORDED for r in self.replays if r["dropped"])
            out.append(("补发丢键", "丢了 %d 个键（%s）" % (dropped, "、".join(reasons))))
        if self.search_entered and not any(self.ready):
            out.append(("搜索框没就绪", "search_ready 超时" if self.ready else "没有 search_ready"))
        if self.errors:
            out.append(("样机报错", "、".join(_unique(self.errors))))
        self._flags = out
        return out


def _restored(foreground_ok, focus_restored, focus_unknown):
    if foreground_ok and focus_restored:
        return ("还原成功（控件未知）" if focus_unknown else "还原成功"), False
    if foreground_ok:
        return "窗口回来了，控件没回来", True
    return "还原失败", True


def _switch_text(switch, with_detector=False):
    text = "%s（%s" % (switch["switched_process"] or UNKNOWN, switch["switched_class"] or UNKNOWN)
    if with_detector and switch["detected_by"]:
        text += "，" + switch["detected_by"]
    return text + "）"


def build_trials(events):
    """按（第几次启动，session）整理。session 为 0 或空的事件不属于任何一次呼出。"""
    trials = OrderedDict()
    run = 0
    skipped_shows = 0
    for line_no, obj in events:
        name = obj.get("event")
        if name == "startup":
            run += 1
            continue
        if name == "show_skipped":
            # 记的是上一次的 session 号，不归到那次呼出里。
            skipped_shows += 1
            continue
        session = obj.get("session")
        if not _is_int(session) or session < 1:
            continue
        key = (run, session)
        trial = trials.get(key)
        if trial is None:
            trial = trials[key] = Trial(run, session, line_no)
        trial.absorb(name, obj)
    return list(trials.values()), skipped_shows


class Group(object):
    def __init__(self, process, mode):
        self.process = process
        self.mode = mode
        self.trials = []

    @property
    def label(self):
        return "%s · %s" % (cell(self.process), self.mode)


def group_trials(trials):
    """按（目标进程，模式）分组，组的顺序按第一次出现；组内给粘贴编号 1..N。"""
    groups = OrderedDict()
    for trial in trials:
        name = trial.process if trial.process else UNKNOWN
        key = (name.lower(), trial.mode)
        group = groups.get(key)
        if group is None:
            group = groups[key] = Group(name, trial.mode)
        group.trials.append(trial)
    for group in groups.values():
        n = 0
        for trial in group.trials:
            if trial.pasted:
                n += 1
                trial.ordinal = n
    return list(groups.values())


# ---------------------------------------------------------------------------
# 单元格文本
# ---------------------------------------------------------------------------

def key_text(paste):
    rule = _sub(paste, "key_rule")
    key = _text(rule.get("key"))
    if key is None:
        return DASH
    source = KEY_SOURCE.get(rule.get("source"))
    if source and _is_int(rule.get("index")):
        return "%s（%s %d）" % (key, source, rule["index"])
    return key


def as_text_text(paste):
    data = paste.get("as_text") if isinstance(paste, dict) else None
    if not isinstance(data, dict):
        return DASH
    error = _text(data.get("error"))
    conversion = _text(data.get("conversion"))
    if error:
        text = "失败：" + error
    elif conversion:
        text = AS_TEXT_CONVERSION.get(conversion, conversion)
    elif _text(data.get("reason")) == "already_text":
        text = "无需转换"
    else:
        text = DASH
    trigger = AS_TEXT_TRIGGER.get(data.get("trigger"))
    return "%s（%s）" % (text, trigger) if trigger else text


def layer_text(trial, key, field):
    if not trial.sent:
        return DASH
    return cell(_text(_sub(trial.paste, key).get(field)))


def detail_row(trial):
    focus_text, _ = trial.focus_state()
    if not trial.pasted:
        return [DASH, trial.line, trial.session, DASH, cell(trial.focus_class), "未粘贴",
                cell(trial.hide_source or NOT_RECORDED), DASH, DASH, DASH, DASH, focus_text,
                DASH, DASH, DASH]
    paste = trial.paste or {}
    switches = "；".join(_switch_text(s) for s in trial.switches) if trial.switches else DASH
    return [
        trial.ordinal, trial.line, trial.session, cell(trial.window_class),
        cell(trial.focus_class), cell(trial.result or "无结论"), cell(trial.reason),
        key_text(paste), as_text_text(paste), layer_text(trial, "layer2", "status"),
        layer_text(trial, "layer3", "conclusion"), focus_text, cell(switches),
        fmt_ms(paste.get("selected_to_send_ms")), fmt_ms(paste.get("selected_to_result_ms")),
    ]


# ---------------------------------------------------------------------------
# 各分节
# ---------------------------------------------------------------------------

def section_groups(groups):
    lines = ["## 1. 按目标和模式", ""]
    if not groups:
        return lines + ["无（范围内没有呼出）", ""]
    headers = ["目标进程", "模式", "呼出", "粘贴", "confirmed", "sent", "copied",
               "copied_only", "无结论", "没粘贴就收起", "可疑"]
    rows = []
    for group in groups:
        pasted = [t for t in group.trials if t.pasted]
        results = Counter(t.result for t in pasted)
        rows.append(
            [cell(group.process), group.mode, len(group.trials), len(pasted)]
            + [results.get(r, 0) for r in RESULTS]
            + [sum(1 for t in pasted if t.result not in RESULTS),
               len(group.trials) - len(pasted),
               sum(1 for t in group.trials if t.flags())])
    lines.extend(base.table(headers, rows))
    lines.append("")
    return lines


def section_reasons(groups):
    lines = ["## 2. 降级和其他原因", ""]
    rows = []
    for group in groups:
        reasons = Counter((t.result, t.reason) for t in group.trials if t.pasted and t.reason)
        for (result, reason), n in sorted(reasons.items(), key=lambda kv: (-kv[1], kv[0][1])):
            rows.append([cell(group.process), group.mode, cell(result or "无结论"), cell(reason),
                         n, REASON_TEXT.get(reason, "见日志")])
    if rows:
        lines.extend(base.table(["目标进程", "模式", "结论", "原因", "次数", "说明"], rows))
    else:
        lines.append("无（粘贴都没有带原因）")
    lines.append("")
    return lines


def section_suspicious(groups):
    lines = ["## 3. 可疑试验", ""]
    rows = []
    for group in groups:
        for trial in group.trials:
            flags = trial.flags()
            if flags:
                rows.append([cell(group.process), group.mode, cell(trial.ordinal), trial.line,
                             trial.session,
                             cell("；".join("%s：%s" % (name, detail) for name, detail in flags))])
    if rows:
        lines.append("可疑不等于失败，要对照手工记录看。“次”是这一组里第几次粘贴，没粘贴的呼出不编号。")
        lines.append("")
        lines.extend(base.table(["目标进程", "模式", "次", "行号", "session", "可疑项"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_hidden(groups):
    lines = ["## 4. 没粘贴就收起", ""]
    rows = []
    for group in groups:
        sources = Counter(t.hide_source for t in group.trials if not t.pasted)
        for source, n in sources.most_common():
            rows.append([cell(group.process), group.mode, cell(source or NOT_RECORDED), n,
                         HIDE_TEXT.get(source, "见日志")])
    if rows:
        lines.extend(base.table(["目标进程", "模式", "收起来源", "次数", "说明"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_timing(groups):
    lines = ["## 5. 耗时", ""]
    rows = []
    for group in groups:
        for key in ("selected_to_send_ms", "selected_to_result_ms"):
            values = [t.paste[key] for t in group.trials
                      if t.paste is not None and _is_num(t.paste.get(key))]
            if values:
                rows.append([cell(group.process), group.mode, key, len(values),
                             fmt_ms(base.percentile_nearest_rank(values, 50)),
                             fmt_ms(base.percentile_nearest_rank(values, 95)),
                             fmt_ms(max(values))])
    if rows:
        lines.extend(base.table(
            ["目标进程", "模式", "指标", "条数", "P50 (ms)", "P95 (ms)", "最大 (ms)"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_search(groups):
    lines = ["## 6. 搜索模式的首键", ""]
    rows = []
    for group in groups:
        if group.mode != SEARCH:
            continue
        for trial in group.trials:
            replays = trial.replays
            wait = replays[0]["wait_ms"] if replays else None
            reasons = "、".join(_unique(r["reason"] or NOT_RECORDED for r in replays))
            if any(trial.ready):
                ready = "是"
            elif trial.ready:
                ready = "超时"
            else:
                ready = "没有"
            first = trial.first_input
            if first and first["input_type"]:
                first_text = first["input_type"] + ("，组合中" if first["composing"] else "")
            else:
                first_text = DASH
            rows.append([cell(group.process), cell(trial.ordinal), trial.line, trial.session,
                         sum(r["replayed"] for r in replays), sum(r["dropped"] for r in replays),
                         fmt_ms(wait), cell(reasons), ready, cell(first_text)])
    if not rows:
        return lines + ["无（范围内没有搜索模式的呼出）", ""]
    lines.extend(base.table(["目标进程", "次", "行号", "session", "补发", "丢弃", "等待 (ms)",
                             "补发原因", "搜索框就绪", "首个输入"], rows))
    lines.append("")
    lines.append("- 补发：进搜索时钩子先收下、搜索框就绪后补发进去的键数。丢弃不为 0，搜索框里就可能少字。")
    lines.append("- 首个输入：insertCompositionText 表示首键进了输入法；insertText 表示直接上屏。"
                 "中文状态下首键直接上屏，就可能出现 `n爱好` 这种结果；英文状态下 insertText 是正常的。")
    lines.append("")
    return lines


def section_details(groups):
    lines = ["## 7. 逐次明细", ""]
    if not groups:
        return lines + [NO_DATA, ""]
    headers = ["次", "行号", "session", "窗口类", "焦点控件类", "结论", "原因", "键", "转文字",
               "第 2 层", "第 3 层", "焦点", "切走", "选中到发键 (ms)", "选中到结论 (ms)"]
    for group in groups:
        lines.append("### " + group.label)
        lines.append("")
        lines.extend(base.table(headers, [detail_row(t) for t in group.trials]))
        lines.append("")
    return lines


def section_legend():
    return [
        "## 附：字段含义",
        "",
        "- 结论：confirmed 确认已粘贴（第 3 层在输入框里读到了这段内容）；sent 粘贴键已发出，确认不了；"
        "copied 按了只复制；copied_only 降级为只复制，没发粘贴键。",
        "- 第 2 层：target_read 目标先读了剪贴板；other_process_first 别的程序先读"
        "（这台机器上常见 msrdc.exe、GameViewer.exe），确认不了目标；unknown_reader_first 查不到谁先读；"
        "before_send 发键前就被读了；timeout 1 秒内没人读。",
        "- 第 3 层：confirmed 读到了内容；timeout 600 ms 内没看到变化；unavailable 目标没有能读的文本接口。"
        "没发粘贴键时两层都写 —。",
        "- 焦点：搜索模式粘贴和收起时看还原的结果；快速取用粘贴时面板不拿焦点，看发键前的确认。",
        "- 切走：粘贴前前台被哪个窗口抢走（paste_switch），只记进程名和窗口类。",
        "",
    ]


# ---------------------------------------------------------------------------
# 组装
# ---------------------------------------------------------------------------

def build_report(events, bad_lines=0, log_path=None, from_line=None, to_line=None,
                 all_runs=False):
    """events 为 (行号, 对象) 列表，返回完整 Markdown 报告。"""
    all_trials, skipped_shows = build_trials(events)
    partial = [t for t in all_trials if not t.started]
    auto = [t for t in all_trials if t.started and t.auto]
    trials = [t for t in all_trials if t.started and not t.auto]
    groups = group_trials(trials)

    pasted = sum(1 for t in trials if t.pasted)
    flagged = sum(1 for t in trials if t.flags())
    startups = sum(1 for _ln, o in events if o.get("event") == "startup")

    scope = []
    if from_line is not None:
        scope.append("自第 %d 行起" % from_line)
    if to_line is not None:
        scope.append("到第 %d 行为止" % to_line)
    scope.append("范围内全部记录" if all_runs else "最后一次启动以来")
    if events:
        scope.append("实际是第 %d–%d 行" % (events[0][0], events[-1][0]))

    out = ["# 焦点矩阵手测汇总", ""]
    if log_path:
        out.append("- 日志文件：`%s`" % log_path)
    out.append("- 统计范围：%s" % "，".join(scope))
    out.append("- 启动 %d 次；呼出 %d 次：粘贴 %d 次，没粘贴就收起 %d 次；可疑 %d 次。"
               % (startups, len(trials), pasted, len(trials) - pasted, flagged))
    if partial:
        out.append("- 有 %d 个 session 的呼出不在范围内，只剩后半段，没算进去。" % len(partial))
    if auto:
        out.append("- 自动显示 %d 次（NIANLAI_G0_AUTOCYCLE），不算试验。" % len(auto))
    if skipped_shows:
        out.append("- 按了呼出键但面板没出来（show_skipped）%d 次：上一次粘贴还没发完，或前端没就绪。"
                   % skipped_shows)
    if bad_lines:
        out.append("- 解析失败、忽略的行：%d" % bad_lines)
    out.append("")
    out.append(COMMIT_NOTE)
    out.append("")

    out.extend(section_groups(groups))
    out.extend(section_reasons(groups))
    out.extend(section_suspicious(groups))
    out.extend(section_hidden(groups))
    out.extend(section_timing(groups))
    out.extend(section_search(groups))
    out.extend(section_details(groups))
    out.extend(section_legend())
    return "\n".join(out).rstrip("\n") + "\n"


def build_report_from_path(path, from_line=None, to_line=None, last_run=True):
    events, bad = load_events(path, from_line, to_line)
    if last_run:
        events = base.scope_last_run(events)
    return build_report(events, bad_lines=bad, log_path=path, from_line=from_line,
                        to_line=to_line, all_runs=not last_run)


def _force_utf8_stdout():
    """重定向到文件时也输出 UTF-8。"""
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError, OSError):
        pass


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="焦点矩阵手测的日志汇总：按（目标进程，模式）统计每次呼出的粘贴结论。")
    parser.add_argument("--log", default=None,
                        help="日志路径（默认：设了 NIANLAI_G0_DATA_DIR 就用其中的 panel-log.jsonl，"
                             "否则 %s）" % base.DEFAULT_LOG)
    parser.add_argument("--from-line", type=int, default=None, metavar="N",
                        help="从第 N 行（1 起）开始")
    parser.add_argument("--to-line", type=int, default=None, metavar="M",
                        help="到第 M 行为止（含）")
    scope = parser.add_mutually_exclusive_group()
    scope.add_argument("--last-run", action="store_true",
                       help="只看范围内最后一次启动以来的记录（默认）")
    scope.add_argument("--all", action="store_true", help="看范围内的全部记录")
    parser.add_argument("--out", default=None, metavar="FILE",
                        help="把报告写成 UTF-8 文件，不打印。PowerShell 里用 > 重定向会乱码，用这个")
    args = parser.parse_args(argv)
    if args.from_line is not None and args.from_line < 1:
        parser.error("--from-line 从 1 开始")
    if args.to_line is not None and args.to_line < (args.from_line or 1):
        parser.error("--to-line 不能小于 --from-line")

    path = args.log or default_log()
    try:
        report = build_report_from_path(path, from_line=args.from_line, to_line=args.to_line,
                                        last_run=not args.all)
    except OSError as exc:
        print("读取日志失败：%s" % exc, file=sys.stderr)
        return 2
    if args.out:
        try:
            with open(args.out, "w", encoding="utf-8", newline="\n") as f:
                f.write(report)
        except OSError as exc:
            print("写报告失败：%s" % exc, file=sys.stderr)
            return 2
        print("报告已写到 %s" % args.out, file=sys.stderr)
        return 0
    _force_utf8_stdout()
    sys.stdout.write(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
