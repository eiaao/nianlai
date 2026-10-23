#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""g0_log_report.py — 汇总剪贴板面板样机的 JSONL 诊断日志，输出中文 Markdown 报告。

只依赖 Python 3 标准库。日志里只有元数据（进程名、格式名、字节数、耗时、计数），
本脚本只做统计汇总，不输出日志内容以外的任何信息。
"""

import argparse
import json
import math
import os
import sys
import time
from collections import Counter

DEFAULT_LOG = os.path.join(
    os.environ.get("LOCALAPPDATA") or "", "Nianlai", "g0", "panel-log.jsonl"
)

NO_DATA = "无"
NOT_RECORDED = "(未记录)"


# ---------------------------------------------------------------------------
# 基础工具
# ---------------------------------------------------------------------------

def _is_num(v):
    """数值判断；bool 是 int 的子类，但不算耗时/字节数。"""
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def fmt_num(v):
    """把数字格式化成尽量短的文本；None 显示为「无」。"""
    if v is None:
        return NO_DATA
    if isinstance(v, bool):
        return "是" if v else "否"
    if isinstance(v, float):
        s = ("%.3f" % v).rstrip("0").rstrip(".")
        return s or "0"
    return str(v)


def fmt_time(ms):
    """Unix 毫秒 -> 本地时间字符串。"""
    if ms is None:
        return NO_DATA
    try:
        ms_i = int(ms)
    except (TypeError, ValueError):
        return str(ms)
    try:
        t = time.localtime(ms_i / 1000)
    except (OverflowError, OSError, ValueError):
        return str(ms)
    return "%s.%03d" % (time.strftime("%Y-%m-%d %H:%M:%S", t), ms_i % 1000)


def percentile_nearest_rank(values, p):
    """最近秩法（nearest-rank）百分位数：排序后取第 ceil(p/100*N) 个（1 起）。

    空列表返回 None。
    """
    if not values:
        return None
    ordered = sorted(values)
    k = int(math.ceil(p / 100.0 * len(ordered)))
    if k < 1:
        k = 1
    elif k > len(ordered):
        k = len(ordered)
    return ordered[k - 1]


def table(headers, rows):
    """生成一个 Markdown 表格。"""
    lines = ["| " + " | ".join(headers) + " |"]
    lines.append("| " + " | ".join("---" for _ in headers) + " |")
    for row in rows:
        lines.append("| " + " | ".join(str(cell) for cell in row) + " |")
    return lines


def counter_rows(counter):
    """Counter -> 表格行，键为 None 时显示 (未记录)。"""
    return [
        [NOT_RECORDED if key is None else str(key), str(n)]
        for key, n in counter.most_common()
    ]


def collect_numbers(objects, key):
    """从一组对象里取某个数值字段（过滤 null / 非数值）。"""
    return [o[key] for o in objects if _is_num(o.get(key))]


def stats_rows(items):
    """items: [(指标名, 数值列表)] -> 统计表（条数 / P50 / P95 / 最大）。"""
    headers = ["指标", "条数", "P50 (ms)", "P95 (ms)", "最大 (ms)"]
    rows = []
    for label, values in items:
        if values:
            rows.append([
                label,
                str(len(values)),
                fmt_num(percentile_nearest_rank(values, 50)),
                fmt_num(percentile_nearest_rank(values, 95)),
                fmt_num(max(values)),
            ])
        else:
            rows.append([label, "0", NO_DATA, NO_DATA, NO_DATA])
    return headers, rows


_BRIEF_KEYS = (
    "reason", "error", "message", "exception", "format", "kind",
    "success", "source_process", "source", "sequence",
)


def brief(obj):
    """错误事件的一行摘要，挑几个常见字段。"""
    parts = []
    for key in _BRIEF_KEYS:
        v = obj.get(key)
        if v is None:
            continue
        parts.append("%s=%s" % (key, fmt_num(v) if not isinstance(v, str) else v))
    return "，".join(parts)


def is_error_event(name):
    return isinstance(name, str) and ("error" in name.lower() or "failed" in name.lower())


# ---------------------------------------------------------------------------
# 读取与裁剪
# ---------------------------------------------------------------------------

def load_events(path, from_line=None):
    """读取 JSONL 日志，返回 (events, bad_lines)。

    events 是 (行号, 对象) 列表；空行跳过，解析失败或不是对象/缺 event 的行计入 bad_lines。
    """
    events = []
    bad = 0
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line_no, raw in enumerate(f, 1):
            if from_line is not None and line_no < from_line:
                continue
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


def scope_last_run(events):
    """截取最后一次 startup（含）之后的记录；没有 startup 时原样返回。"""
    start = 0
    for i, (_line_no, obj) in enumerate(events):
        if obj.get("event") == "startup":
            start = i
    return events[start:]


# ---------------------------------------------------------------------------
# 各分节
# ---------------------------------------------------------------------------

def section_scope(events, bad_lines=0):
    lines = ["## 1. 范围", ""]
    startups = sum(1 for _ln, o in events if o.get("event") == "startup")
    if events:
        first_ln, first = events[0]
        last_ln, last = events[-1]
        rows = [
            ["起始时间", "%s（第 %d 行）" % (fmt_time(first.get("time_ms")), first_ln)],
            ["结束时间", "%s（第 %d 行）" % (fmt_time(last.get("time_ms")), last_ln)],
            ["行数", str(len(events))],
            ["启动次数", str(startups)],
        ]
    else:
        rows = [
            ["起始时间", NO_DATA],
            ["结束时间", NO_DATA],
            ["行数", "0"],
            ["启动次数", "0"],
        ]
    if bad_lines:
        rows.append(["解析失败/忽略的行", str(bad_lines)])
    lines.extend(table(["项目", "值"], rows))
    lines.append("")
    return lines


def section_capture(events):
    caps = [(ln, o) for ln, o in events if o.get("event") == "capture"]
    slows = [(ln, o) for ln, o in events if o.get("event") == "capture_slow"]

    lines = ["## 2. 采集", ""]

    if not caps:
        lines.append("读取（capture）：" + NO_DATA)
        lines.append("")
    else:
        # outcome / reason 计数
        lines.append("### 读取（capture）共 %d 条" % len(caps))
        lines.append("")
        lines.append("outcome 计数：")
        lines.append("")
        lines.extend(table(["outcome", "条数"],
                           counter_rows(Counter(o.get("outcome") for _ln, o in caps))))
        lines.append("")
        reasons = Counter(o.get("reason") for _ln, o in caps if o.get("reason") is not None)
        lines.append("reason 计数（非 null）：")
        lines.append("")
        if reasons:
            lines.extend(table(["reason", "条数"], counter_rows(reasons)))
        else:
            lines.append(NO_DATA)
        lines.append("")

        # notification_to_history_ms 统计
        n2h = collect_numbers([o for _ln, o in caps], "notification_to_history_ms")
        headers, rows = stats_rows([("notification_to_history_ms", n2h)])
        lines.extend(table(headers, rows))
        lines.append("")

        # 按来源进程分组
        proc_counts = Counter()
        proc_kinds = {}
        proc_formats = {}
        for _ln, o in caps:
            src = o.get("source_process")
            key = src if src else NOT_RECORDED
            proc_counts[key] += 1
            kinds = proc_kinds.setdefault(key, Counter())
            kind = o.get("kind")
            kinds[kind if kind else NOT_RECORDED] += 1
            names = proc_formats.setdefault(key, [])
            for name in o.get("formats") or []:
                if isinstance(name, str) and name not in names:
                    names.append(name)
        lines.append("### 按来源进程分组")
        lines.append("")
        proc_rows = []
        for proc, n in proc_counts.most_common():
            dist = " / ".join("%s %d" % (k, m) for k, m in proc_kinds[proc].most_common())
            proc_rows.append([proc, str(n), dist])
        lines.extend(table(["来源进程", "条数", "类型（kind）分布"], proc_rows))
        lines.append("")

        # “不要记录”标记
        markers = []
        for _ln, o in caps:
            m = o.get("marker")
            if m and m not in markers:
                markers.append(m)
        lines.append("### 见过的「不要记录」标记")
        lines.append("")
        lines.append("、".join("`%s`" % m for m in markers) if markers else NO_DATA)
        lines.append("")

        # 打开剪贴板重试分布
        retries = Counter(o.get("open_retries") for _ln, o in caps
                          if o.get("open_retries") is not None)
        lines.append("### 打开剪贴板的重试分布（open_retries）")
        lines.append("")
        lines.extend(table(["open_retries", "条数"], counter_rows(retries))
                     if retries else [NO_DATA])
        lines.append("")
        seq_changed = sum(1 for _ln, o in caps if o.get("reason") == "sequence_changed")
        lines.append("- sequence_changed（reason 命中）共 %d 次" % seq_changed)
        lines.append("")

        # missing_formats 按原因
        mf = Counter()
        for _ln, o in caps:
            for item in o.get("missing_formats") or []:
                if isinstance(item, dict):
                    mf[item.get("reason") or NOT_RECORDED] += 1
                else:
                    mf[NOT_RECORDED] += 1
        lines.append("### missing_formats 按原因")
        lines.append("")
        lines.extend(table(["reason", "条数"], counter_rows(mf)) if mf else [NO_DATA])
        lines.append("")

        # 每个来源进程见过的格式名
        lines.append("### 各来源进程出现过的格式名（每个进程最多列 20 个）")
        lines.append("")
        listed = False
        for proc in proc_counts:
            names = proc_formats.get(proc) or []
            if not names:
                continue
            listed = True
            if len(names) > 20:
                lines.append("- %s（共 %d 个，仅列前 20 个）：%s"
                             % (proc, len(names), "、".join(names[:20])))
            else:
                lines.append("- %s（共 %d 个）：%s" % (proc, len(names), "、".join(names)))
        if not listed:
            lines.append(NO_DATA)
        lines.append("")

    # 慢读取（capture_slow 独立于 capture 存在）
    lines.append("### 慢读取（capture_slow）")
    lines.append("")
    if slows:
        rows = [[fmt_time(o.get("time_ms")), str(o.get("format") or NOT_RECORDED),
                 fmt_num(o.get("read_ms"))] for _ln, o in slows]
        lines.extend(table(["时间", "format", "read_ms"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_listen(events):
    lines = ["## 3. 监听", ""]

    started = [(ln, o) for ln, o in events if o.get("event") == "capture_listener_started"]
    lines.append("### 监听器启动（capture_listener_started）")
    lines.append("")
    if started:
        rows = [[fmt_time(o.get("time_ms")), fmt_num(o.get("baseline_sequence"))]
                for _ln, o in started]
        lines.extend(table(["时间", "baseline_sequence"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")

    # 自检失效 -> 找其后第一条 capture_listener_rearmed（截止到下一次自检失效）
    lines.append("### 自检判定失效（capture_self_check_failed）")
    lines.append("")
    check_idx = [i for i, (_ln, o) in enumerate(events)
                 if o.get("event") == "capture_self_check_failed"]
    if check_idx:
        rearm_of = {}
        pending = None
        for i, (_ln, o) in enumerate(events):
            name = o.get("event")
            if name == "capture_self_check_failed":
                pending = i
            elif name == "capture_listener_rearmed" and pending is not None:
                rearm_of[pending] = o
                pending = None
        rows = []
        for i in check_idx:
            ln, o = events[i]
            rearm = rearm_of.get(i)
            if rearm is None:
                result = "未见重挂"
            elif rearm.get("success"):
                result = "成功（%s ms%s）" % (
                    fmt_num(rearm.get("elapsed_ms")),
                    "，重建窗口" if rearm.get("rebuilt_window") else "",
                )
            else:
                result = "失败（%s ms）" % fmt_num(rearm.get("elapsed_ms"))
            rows.append([
                fmt_time(o.get("time_ms")),
                fmt_num(o.get("sequence")),
                fmt_num(o.get("since_system_event_ms")),
                fmt_num(o.get("since_own_write_ms")),
                result,
            ])
        lines.extend(table(
            ["时间", "sequence", "距最近系统事件 (ms)", "距最近自己写剪贴板 (ms)", "重挂结果"],
            rows))
        lines.append("")

    rearms = [o for _ln, o in events if o.get("event") == "capture_listener_rearmed"]
    ok = sum(1 for o in rearms if o.get("success"))
    dropped = [o for _ln, o in events if o.get("event") == "capture_listener_dropped_for_test"]
    dropped_ok = sum(1 for o in dropped if o.get("success"))
    stopped = [o for _ln, o in events if o.get("event") == "capture_reader_stopped"]
    lines.append("- 重挂（capture_listener_rearmed）：共 %d 次，成功 %d 次，失败 %d 次"
                 % (len(rearms), ok, len(rearms) - ok))
    lines.append("- 测试性摘除（capture_listener_dropped_for_test）：%d 次，success=true %d 次"
                 % (len(dropped), dropped_ok))
    lines.append("- 读线程停止（capture_reader_stopped）：%d 次" % len(stopped))
    if not check_idx:
        lines.append("- 自检判定失效（capture_self_check_failed）：" + NO_DATA)
    lines.append("")

    sys_events = [(ln, o) for ln, o in events if o.get("event") == "system_event"]
    lines.append("### 系统事件时间线（system_event）")
    lines.append("")
    if sys_events:
        rows = [[fmt_time(o.get("time_ms")), str(o.get("kind") or NOT_RECORDED)]
                for _ln, o in sys_events]
        lines.extend(table(["时间", "kind"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def _sub_dict(obj, key):
    """取子对象，容错：不存在 / null / 不是字典都返回 {}。"""
    sub = obj.get(key)
    return sub if isinstance(sub, dict) else {}


def section_paste(events):
    lines = ["## 4. 粘贴", ""]
    pastes = [(ln, o) for ln, o in events if o.get("event") == "paste"]
    if not pastes:
        lines.append(NO_DATA)
        lines.append("")
        return lines

    results = Counter(o.get("paste_result") or NOT_RECORDED for _ln, o in pastes)
    lines.append("### 结论（paste_result）共 %d 条" % len(pastes))
    lines.append("")
    lines.extend(table(["paste_result", "条数"], counter_rows(results)))
    lines.append("")

    reasons = Counter(o.get("reason") for _ln, o in pastes if o.get("reason") is not None)
    lines.append("### 原因（reason，非 null）")
    lines.append("")
    if reasons:
        lines.extend(table(["reason", "条数"], counter_rows(reasons)))
    else:
        lines.append(NO_DATA)
    lines.append("")

    layer2 = Counter()
    for _ln, o in pastes:
        status = _sub_dict(o, "layer2").get("status")
        layer2[status if status else NOT_RECORDED] += 1
    has_layer2 = any(isinstance(o.get("layer2"), dict) for _ln, o in pastes)
    lines.append("### 第 2 层状态（layer2.status）")
    lines.append("")
    if has_layer2:
        lines.extend(table(["status", "条数"], counter_rows(layer2)))
    else:
        lines.append(NO_DATA)
    lines.append("")

    layer3 = Counter()
    confirmations = []
    for _ln, o in pastes:
        sub = _sub_dict(o, "layer3")
        conclusion = sub.get("conclusion")
        if sub:
            layer3[conclusion if conclusion else NOT_RECORDED] += 1
            if _is_num(sub.get("confirmation_ms")):
                confirmations.append(sub["confirmation_ms"])
    lines.append("### 第 3 层结论（layer3.conclusion）")
    lines.append("")
    if layer3:
        lines.extend(table(["conclusion", "条数"], counter_rows(layer3)))
    else:
        lines.append(NO_DATA)
    lines.append("")

    headers, rows = stats_rows([
        ("selected_to_send_ms", collect_numbers([o for _ln, o in pastes], "selected_to_send_ms")),
        ("selected_to_result_ms", collect_numbers([o for _ln, o in pastes], "selected_to_result_ms")),
        ("layer3.confirmation_ms", confirmations),
    ])
    lines.extend(table(headers, rows))
    lines.append("")
    return lines


def section_show(events):
    lines = ["## 5. 呼出", ""]
    shown = [o for _ln, o in events if o.get("event") == "shown"]
    painted = [o for _ln, o in events if o.get("event") == "painted"]
    headers, rows = stats_rows([
        ("hotkey_to_show_ms（shown，%d 条）" % len(shown),
         collect_numbers(shown, "hotkey_to_show_ms")),
        ("show_to_paint_ms（painted，%d 条）" % len(painted),
         collect_numbers(painted, "show_to_paint_ms")),
    ])
    lines.extend(table(headers, rows))
    lines.append("")

    hide = Counter(o.get("source") for _ln, o in events if o.get("event") == "hide_source")
    lines.append("### 隐藏来源（hide_source）")
    lines.append("")
    if hide:
        lines.extend(table(["source", "条数"], counter_rows(hide)))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_errors(events):
    lines = ["## 6. 错误", ""]
    errs = [(ln, o) for ln, o in events if is_error_event(o.get("event"))]
    if errs:
        rows = [[str(ln), fmt_time(o.get("time_ms")), str(o.get("event")), brief(o)]
                for ln, o in errs]
        lines.extend(table(["行号", "时间", "事件", "详情"], rows))
    else:
        lines.append(NO_DATA)
    lines.append("")
    return lines


def section_summary(events):
    lines = ["## 7. 退出汇总", ""]
    summaries = [o for _ln, o in events if o.get("event") == "capture_summary"]
    if not summaries:
        lines.append(NO_DATA)
        lines.append("")
        return lines
    lines.append("最后一条 capture_summary（共 %d 条，原样）：" % len(summaries))
    lines.append("")
    lines.append("```json")
    lines.append(json.dumps(summaries[-1], ensure_ascii=False, indent=2))
    lines.append("```")
    lines.append("")
    return lines


# ---------------------------------------------------------------------------
# 组装
# ---------------------------------------------------------------------------

def build_report(events, bad_lines=0, log_path=None, from_line=None, all_runs=False):
    """events 为 (行号, 对象) 列表，返回完整 Markdown 报告文本。"""
    out = []
    out.append("# 剪贴板面板诊断日志汇总")
    out.append("")
    if log_path:
        out.append("- 日志文件：`%s`" % log_path)
    scope_bits = []
    if from_line is not None:
        scope_bits.append("自第 %d 行起" % from_line)
    scope_bits.append("全部记录" if all_runs else "最后一次启动以来")
    out.append("- 统计范围：%s" % "，".join(scope_bits))
    out.append("")

    out.extend(section_scope(events, bad_lines))
    out.extend(section_capture(events))
    out.extend(section_listen(events))
    out.extend(section_paste(events))
    out.extend(section_show(events))
    out.extend(section_errors(events))
    out.extend(section_summary(events))
    return "\n".join(out).rstrip("\n") + "\n"


def build_report_from_path(path, from_line=None, last_run=True):
    events, bad = load_events(path, from_line)
    if last_run:
        events = scope_last_run(events)
    return build_report(events, bad_lines=bad, log_path=path,
                        from_line=from_line, all_runs=not last_run)


def _force_utf8_stdout():
    """重定向到文件时也输出 UTF-8，保证报告编码稳定。"""
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError, OSError):
        pass


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="汇总剪贴板面板样机的 JSONL 诊断日志（%s）" % DEFAULT_LOG)
    parser.add_argument("--log", default=DEFAULT_LOG,
                        help="日志路径（默认 %s）" % DEFAULT_LOG)
    parser.add_argument("--from-line", type=int, default=None, metavar="N",
                        help="从第 N 行（1 起）开始")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--last-run", action="store_true",
                       help="只看最后一次启动以来的记录（默认行为）")
    group.add_argument("--all", action="store_true", help="看全部记录")
    args = parser.parse_args(argv)

    try:
        report = build_report_from_path(args.log, from_line=args.from_line,
                                        last_run=not args.all)
    except OSError as exc:
        print("读取日志失败：%s" % exc, file=sys.stderr)
        return 2
    _force_utf8_stdout()
    print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
