#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""test_g0_matrix_report.py — g0_matrix_report 的单元测试。

按 panel.rs、paste.rs 写出的字段构造小日志，覆盖：分组和计数、两种模式的判定、降级原因、
可疑项、没粘贴就收起、paste 晚到、多次启动、行号范围、字段缺失，以及不输出不在白名单里的字段。
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import g0_matrix_report as rep  # noqa: E402

BASE = 1790000000000
# 放进不该输出的字段（标题、内容、UIA 类名、错误信息），报告里不能出现。
SECRET = "SECRET-不该出现-7f3a"
RESTORED = {"foreground": {"path": "direct", "success": True},
            "focus_restored": True, "focus_unknown": False}


def ev(event, dt, **kw):
    """构造一条日志事件：必有 event 和 time_ms，其余字段按需。"""
    obj = {"event": event, "time_ms": BASE + dt}
    obj.update(kw)
    return obj


def shown(session, dt, process, focus_class="Edit"):
    """一次呼出：hotkey 和 shown 两条。"""
    return [
        ev("hotkey", dt, session=session, process_name=process, focus_class=focus_class),
        ev("shown", dt + 1, session=session, process_name=process, focus_class=focus_class,
           auto=False, foreground_ok=True, focus_unknown=False, hotkey_to_show_ms=12.5,
           window_title=SECRET),
    ]


def paste(session, dt, process, result, reason=None, sent=True, layer2="target_read",
          layer3="confirmed", restore=None, failed=None, as_text=None, send_ms=40.0,
          result_ms=120.0, key="Ctrl+V"):
    """按 paste.rs 的 execute() 造一条 paste 事件。"""
    obj = {
        "session": session, "paste_result": result, "reason": reason,
        "clipboard_written": True, "record_id": 7, "written_formats": ["CF_UNICODETEXT"],
        "process_name": process, "window_class": "TargetClass", "focus_class": "Edit",
        "focus_unknown": False,
        "key_rule": {"source": "builtin", "index": 8, "key": key, "as_text": None},
        "as_text": as_text, "actual_key": key if sent else None,
        "preflight": None, "integrity": None,
        "layer2": ({"status": layer2, "first_reader": process,
                    "timed_out": layer2 == "timeout"} if sent else None),
        "layer3": {"interface": "value", "class_name": SECRET,
                   "conclusion": layer3 if sent else "unavailable"},
        "selected_to_send_ms": send_ms if sent else None,
        "selected_to_result_ms": result_ms,
        "text": SECRET,
    }
    if sent or failed is not None:
        obj["preflight"] = {"failed_checks": list(failed or []), "timed_out": bool(failed)}
    if restore is not None:
        obj["restore"] = restore
    return ev("paste", dt, **obj)


def focus(session, dt, ok=True, restored=True, unknown=False, switch=False):
    return ev("focus_restored", dt, session=session, foreground_ok=ok, focus_restored=restored,
              focus_unknown=unknown, external_switch=switch, foreground_path="unchanged")


def make_log():
    """13 次呼出，覆盖要汇总的各种情况。"""
    events = [ev("startup", 0, platform="windows"),
              ev("store_opened", 1, location=SECRET),
              ev("frontend_ready", 2, session=0)]
    # S1 微信，快速取用，确认已粘贴
    events += shown(1, 1000, "Weixin.exe")
    events += [ev("hide_source", 1010, session=1, source="paste", process_name="Weixin.exe"),
               paste(1, 1100, "Weixin.exe", "confirmed", send_ms=40.0, result_ms=120.0)]
    # S2 微信，搜索模式，已发送，补发 4 个键
    events += shown(2, 2000, "Weixin.exe")
    events += [ev("search_foreground", 2010, session=2, foreground_ok=True),
               ev("search_replay", 2100, session=2, replayed_keys=4, dropped_keys=0,
                  wait_ms=150.0, search_box_ready=True, reason="ready"),
               ev("search_ready", 2101, session=2, search_box_ready=True, foreground_ok=True),
               ev("search_first_input", 2120, session=2, input_type="insertText",
                  composing=False, after_ready_ms=3.0),
               ev("search", 2130, words=1, characters=4, mode="fts"),  # 历史搜索，没有 session
               ev("hide_source", 2200, session=2, source="paste"),
               paste(2, 2300, "Weixin.exe", "sent", layer3="timeout", restore=RESTORED,
                     send_ms=110.0, result_ms=700.0)]
    # S3 微信，快速取用，按 Esc 收起；前端随后又报一次 frontend
    events += shown(3, 3000, "Weixin.exe")
    events += [ev("hide_source", 3010, session=3, source="escape_key"),
               focus(3, 3011),
               ev("hide_source", 3012, session=3, source="frontend")]
    # S4 飞书，快速取用，粘贴前前台被输入法窗口抢走
    events += shown(4, 4000, "Feishu.exe", "Chrome_RenderWidgetHostHWND")
    events += [ev("paste_switch", 4010, session=4, detected_by="foreground_event",
                  switched_class="IME", switched_process="ChsIME.exe"),
               ev("hide_source", 4011, session=4, source="paste"),
               paste(4, 4100, "Feishu.exe", "copied_only", reason="external_switch",
                     sent=False, result_ms=30.0)]
    # S5 以管理员身份运行的 Notepad++
    events += shown(5, 5000, "notepad++.exe", "Scintilla")
    events += [ev("hide_source", 5010, session=5, source="paste"),
               paste(5, 5100, "notepad++.exe", "copied_only",
                     reason="target_may_be_elevated", sent=False, result_ms=25.0)]
    # S6 WPS，快速取用：paste 事件晚于下一次呼出才写出
    events += shown(6, 6000, "wps.exe", "WpsEditClass")
    events += [ev("hide_source", 6010, session=6, source="paste")]
    # S7 WPS，快速取用，点面板外收起，焦点没回来（钩子线程写的 hide_source 不带进程名）
    events += shown(7, 6500, "wps.exe", "WpsEditClass")
    events += [ev("hide_source", 6510, session=7, source="outside_click", process_name=None),
               focus(7, 6511, ok=False, restored=False),
               paste(6, 6600, "wps.exe", "sent", layer2="timeout", layer3="unavailable",
                     send_ms=50.0, result_ms=1100.0)]
    # S8 WPS 表格，搜索模式：搜索框超时，3 个键被丢掉，按 Esc 收起
    events += shown(8, 8000, "et.exe", "EtCell")
    events += [ev("search_foreground", 8010, session=8, foreground_ok=True),
               ev("search_replay", 8600, session=8, replayed_keys=0, dropped_keys=3,
                  wait_ms=510.0, search_box_ready=False, reason="timeout"),
               ev("search_ready", 8601, session=8, search_box_ready=False, foreground_ok=True),
               ev("hide_source", 8700, session=8, source="frontend"),
               focus(8, 8701, unknown=True)]
    # S9 远程桌面，快速取用：收起来源是粘贴，但没有 paste 事件
    events += shown(9, 9000, "mstsc.exe", "IHWindowClass")
    events += [ev("hide_source", 9010, session=9, source="paste")]
    # S10 远程桌面，搜索模式：面板没拿到前台，自动收起，还报了错（没有 search_ready）
    events += shown(10, 10000, "mstsc.exe", "IHWindowClass")
    events += [ev("search_foreground", 10010, session=10, foreground_ok=False),
               ev("search_foreground_wait", 10500, session=10, foreground_ok=False),
               ev("hide_source", 10501, session=10, source="search_activation_failed"),
               focus(10, 10502),
               ev("panel_error", 10503, session=10, error=SECRET)]
    # S11 微信，快速取用，只复制
    events += shown(11, 11000, "Weixin.exe")
    events += [ev("hide_source", 11010, session=11, source="paste"),
               paste(11, 11100, "Weixin.exe", "copied", reason="user_copy", sent=False,
                     result_ms=20.0)]
    # S12 微信，快速取用，发键前确认没过；之后按呼出键被跳过（记的是旧 session 号）
    events += shown(12, 12000, "Weixin.exe")
    events += [ev("hide_source", 12010, session=12, source="paste"),
               paste(12, 12100, "Weixin.exe", "copied_only", reason="preflight_timeout",
                     sent=False, failed=["focus"], result_ms=310.0),
               ev("show_skipped", 12200, session=12, ready=True, pasting=True),
               ev("capture", 12300, outcome="saved", source_process="Weixin.exe",
                  formats=[SECRET])]
    # S13 微信，快速取用，再按呼出键收起时前台已经换成别的窗口
    events += shown(13, 13000, "Weixin.exe")
    events += [ev("hide_source", 13010, session=13, source="hotkey"),
               focus(13, 13011, ok=False, restored=False, switch=True)]
    return events


def indexed(events):
    return [(i + 1, obj) for i, obj in enumerate(events)]


def line_of(events, event, session):
    """某个 session 的某条事件在日志里的行号（1 起）。"""
    for i, obj in enumerate(events):
        if obj.get("event") == event and obj.get("session") == session:
            return i + 1
    raise AssertionError("找不到 %s / session %s" % (event, session))


def write_jsonl(path, events, tail=""):
    with open(path, "w", encoding="utf-8") as f:
        for obj in events:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")
        f.write(tail)


def section(report, title):
    """取出某一节（到下一个 ## 为止）。"""
    start = report.index(title)
    end = report.find("\n## ", start + len(title))
    return report[start:] if end < 0 else report[start:end]


def table_rows(text):
    """Markdown 表格的数据行（去掉表头和分隔行），按单元格拆开。"""
    rows = [line for line in text.splitlines() if line.startswith("| ")]
    return [[c.strip() for c in row.strip("|").split(" | ")] for row in rows[2:]]


class GroupTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_log()))

    def test_overview(self):
        self.assertIn("- 启动 1 次；呼出 13 次：粘贴 8 次，没粘贴就收起 5 次；可疑 9 次。",
                      self.report)
        self.assertIn("show_skipped）1 次", self.report)

    def test_group_rows(self):
        for row in (
            "| Weixin.exe | 快速取用 | 5 | 3 | 1 | 0 | 1 | 1 | 0 | 2 | 2 |",
            "| Weixin.exe | 搜索模式 | 1 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |",
            "| Feishu.exe | 快速取用 | 1 | 1 | 0 | 0 | 0 | 1 | 0 | 0 | 1 |",
            "| notepad++.exe | 快速取用 | 1 | 1 | 0 | 0 | 0 | 1 | 0 | 0 | 1 |",
            "| wps.exe | 快速取用 | 2 | 1 | 0 | 1 | 0 | 0 | 0 | 1 | 2 |",
            "| et.exe | 搜索模式 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 |",
            "| mstsc.exe | 快速取用 | 1 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | 1 |",
            "| mstsc.exe | 搜索模式 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 |",
        ):
            self.assertIn(row, self.report)

    def test_group_order_follows_first_appearance(self):
        rows = table_rows(section(self.report, "## 1. 按目标和模式"))
        self.assertEqual([r[0] + "/" + r[1] for r in rows][:3],
                         ["Weixin.exe/快速取用", "Weixin.exe/搜索模式", "Feishu.exe/快速取用"])

    def test_search_foreground_alone_counts_as_search(self):
        # S10 只有 search_foreground，没有 search_ready，也算搜索模式。
        trials, _ = rep.build_trials(indexed(make_log()))
        by_session = {t.session: t for t in trials}
        self.assertEqual(by_session[10].mode, rep.SEARCH)
        self.assertEqual(by_session[9].mode, rep.QUICK)
        self.assertEqual(by_session[2].mode, rep.SEARCH)

    def test_late_paste_goes_to_its_own_session(self):
        trials, _ = rep.build_trials(indexed(make_log()))
        by_session = {t.session: t for t in trials}
        self.assertEqual(by_session[6].result, "sent")
        self.assertIsNone(by_session[7].paste)
        self.assertFalse(by_session[7].pasted)

    def test_commit_note(self):
        self.assertIn("判断不了“误提交”", self.report)
        self.assertIn("要按记录表人工记", self.report)


class ReasonAndHiddenTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_log()))

    def test_reasons(self):
        for row in (
            "| Weixin.exe | 快速取用 | copied | user_copy | 1 |",
            "| Weixin.exe | 快速取用 | copied_only | preflight_timeout | 1 |",
            "| Feishu.exe | 快速取用 | copied_only | external_switch | 1 |",
            "| notepad++.exe | 快速取用 | copied_only | target_may_be_elevated | 1 |",
        ):
            self.assertIn(row, self.report)
        self.assertIn("目标以管理员身份运行", self.report)

    def test_hidden_without_paste_uses_first_source(self):
        text = section(self.report, "## 4. 没粘贴就收起")
        for row in (
            "| Weixin.exe | 快速取用 | escape_key | 1 |",
            "| Weixin.exe | 快速取用 | hotkey | 1 |",
            "| wps.exe | 快速取用 | outside_click | 1 |",
            "| et.exe | 搜索模式 | frontend | 1 |",
            "| mstsc.exe | 搜索模式 | search_activation_failed | 1 |",
        ):
            self.assertIn(row, text)
        # S3 按 Esc 后前端又报的 frontend 不算第二个来源
        self.assertNotIn("| Weixin.exe | 快速取用 | frontend |", text)
        self.assertEqual(len(table_rows(text)), 5)


class SuspiciousTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_log()))
        self.text = section(self.report, "## 3. 可疑试验")
        self.rows = table_rows(self.text)

    def flags_of(self, session):
        for row in self.rows:
            if row[4] == str(session):
                return row[5]
        return None

    def test_only_suspicious_sessions_listed(self):
        self.assertEqual(sorted(int(r[4]) for r in self.rows),
                         [4, 5, 6, 7, 8, 9, 10, 12, 13])

    def test_flag_details(self):
        self.assertEqual(self.flags_of(4),
                         "copied_only：external_switch；"
                         "paste_switch：ChsIME.exe（IME，foreground_event）")
        self.assertEqual(self.flags_of(5), "copied_only：target_may_be_elevated")
        self.assertEqual(self.flags_of(6), "第 2 层超时：1 秒内没有程序读剪贴板，多半没粘进去")
        self.assertEqual(self.flags_of(7),
                         "焦点没还原：还原失败（收起来源 outside_click，点到别的窗口时属正常）")
        self.assertEqual(self.flags_of(8),
                         "补发丢键：丢了 3 个键（timeout）；搜索框没就绪：search_ready 超时")
        self.assertIn("无结论：没有 paste 事件", self.flags_of(9))
        self.assertEqual(self.flags_of(10), "搜索框没就绪：没有 search_ready；样机报错：panel_error")
        self.assertEqual(self.flags_of(12), "copied_only：preflight_timeout（没过：focus）")
        self.assertEqual(self.flags_of(13),
                         "external_switch：收起时前台已是别的窗口（收起来源 hotkey）")

    def test_ordinal_counts_pastes_in_group(self):
        # 微信快速取用的粘贴依次是 S1、S11、S12，S12 是第 3 次；没粘贴的 S13 不编号。
        for row in self.rows:
            if row[4] == "12":
                self.assertEqual(row[2], "3")
            if row[4] == "13":
                self.assertEqual(row[2], rep.DASH)


class DetailTest(unittest.TestCase):
    def setUp(self):
        self.events = make_log()
        self.report = rep.build_report(indexed(self.events))

    def test_pasted_rows(self):
        s1 = line_of(self.events, "hotkey", 1)
        self.assertIn("| 1 | %d | 1 | TargetClass | Edit | confirmed | — | Ctrl+V（内置 8） | — "
                      "| target_read | confirmed | 发键前确认通过 | — | 40.0 | 120.0 |" % s1,
                      self.report)
        s2 = line_of(self.events, "hotkey", 2)
        self.assertIn("| 1 | %d | 2 | TargetClass | Edit | sent | — | Ctrl+V（内置 8） | — "
                      "| target_read | timeout | 还原成功 | — | 110.0 | 700.0 |" % s2,
                      self.report)
        s6 = line_of(self.events, "hotkey", 6)
        self.assertIn("| 1 | %d | 6 | TargetClass | WpsEditClass | sent | — | Ctrl+V（内置 8） "
                      "| — | timeout | unavailable | 发键前确认通过 | — | 50.0 | 1100.0 |" % s6,
                      self.report)

    def test_not_sent_rows_leave_layers_empty(self):
        s4 = line_of(self.events, "hotkey", 4)
        self.assertIn("| 1 | %d | 4 | TargetClass | Chrome_RenderWidgetHostHWND | copied_only "
                      "| external_switch | Ctrl+V（内置 8） | — | — | — | — | ChsIME.exe（IME） "
                      "| — | 30.0 |" % s4, self.report)
        s12 = line_of(self.events, "hotkey", 12)
        self.assertIn("发键前确认没过：focus", self.report)
        self.assertIn("| 3 | %d | 12 |" % s12, self.report)

    def test_hidden_rows(self):
        s3 = line_of(self.events, "hotkey", 3)
        self.assertIn("| — | %d | 3 | — | Edit | 未粘贴 | escape_key | — | — | — | — | 还原成功 "
                      "| — | — | — |" % s3, self.report)
        s8 = line_of(self.events, "hotkey", 8)
        self.assertIn("| — | %d | 8 | — | EtCell | 未粘贴 | frontend | — | — | — | — "
                      "| 还原成功（控件未知） | — | — | — |" % s8, self.report)
        s13 = line_of(self.events, "hotkey", 13)
        self.assertIn("| — | %d | 13 | — | Edit | 未粘贴 | hotkey | — | — | — | — "
                      "| 没还原：前台已是别的窗口 | — | — | — |" % s13, self.report)

    def test_group_headings(self):
        for heading in ("### Weixin.exe · 快速取用", "### Weixin.exe · 搜索模式",
                        "### mstsc.exe · 搜索模式"):
            self.assertIn(heading, self.report)

    def test_as_text_and_override_key(self):
        events = [ev("startup", 0)] + shown(1, 10, "notepad++.exe") + [
            ev("hide_source", 20, session=1, source="paste"),
            paste(1, 30, "notepad++.exe", "sent", layer3="unavailable",
                  as_text={"trigger": "rule", "conversion": "paths", "style": "lines",
                           "files": 2, "elapsed_ms": 0.01}),
        ] + shown(2, 40, "notepad++.exe") + [
            ev("hide_source", 50, session=2, source="paste"),
            paste(2, 60, "notepad++.exe", "sent",
                  as_text={"trigger": "manual", "conversion": None, "reason": "already_text"}),
        ] + shown(3, 70, "notepad++.exe") + [
            ev("hide_source", 80, session=3, source="paste"),
            paste(3, 90, "notepad++.exe", "sent",
                  as_text={"trigger": "rule", "conversion": "image_path", "error": "no_export_directory"}),
        ]
        events[-1]["key_rule"] = {"source": "override", "index": 1, "key": "Shift+Insert"}
        report = rep.build_report(indexed(events))
        self.assertIn("| 路径（键表） |", report)
        self.assertIn("| 无需转换（Ctrl+Enter） |", report)
        self.assertIn("| 失败：no_export_directory（键表） |", report)
        self.assertIn("| Shift+Insert（覆盖表 1） |", report)


class TimingAndSearchTest(unittest.TestCase):
    def setUp(self):
        self.events = make_log()
        self.report = rep.build_report(indexed(self.events))

    def test_timing(self):
        # 微信快速取用：只有 S1 发了键；选中到结论是 [120, 20, 310]
        self.assertIn("| Weixin.exe | 快速取用 | selected_to_send_ms | 1 | 40.0 | 40.0 | 40.0 |",
                      self.report)
        self.assertIn("| Weixin.exe | 快速取用 | selected_to_result_ms | 3 | 120.0 | 310.0 "
                      "| 310.0 |", self.report)

    def test_search_first_key(self):
        text = section(self.report, "## 6. 搜索模式的首键")
        s2 = line_of(self.events, "hotkey", 2)
        self.assertIn("| Weixin.exe | 1 | %d | 2 | 4 | 0 | 150.0 | ready | 是 | insertText |" % s2,
                      text)
        s8 = line_of(self.events, "hotkey", 8)
        self.assertIn("| et.exe | — | %d | 8 | 0 | 3 | 510.0 | timeout | 超时 | — |" % s8, text)
        s10 = line_of(self.events, "hotkey", 10)
        self.assertIn("| mstsc.exe | — | %d | 10 | 0 | 0 | — | — | 没有 | — |" % s10, text)
        self.assertEqual(len(table_rows(text)), 3)


class PrivacyTest(unittest.TestCase):
    def test_no_fields_outside_whitelist(self):
        report = rep.build_report(indexed(make_log()))
        self.assertNotIn(SECRET, report)
        self.assertNotIn("SECRET", report)

    def test_pipes_and_newlines_do_not_break_tables(self):
        events = [ev("startup", 0)] + shown(1, 10, "a|b.exe", "x\ny") + [
            ev("hide_source", 20, session=1, source="escape_key")]
        report = rep.build_report(indexed(events))
        self.assertIn("| a\\|b.exe | 快速取用 | 1 | 0 |", report)
        self.assertIn("x y", report)


class ScopeTest(unittest.TestCase):
    def two_runs(self):
        return ([ev("startup", 0)] + shown(1, 10, "a.exe")
                + [ev("hide_source", 20, session=1, source="paste"), paste(1, 30, "a.exe", "sent")]
                + [ev("startup", 100)] + shown(1, 110, "a.exe")
                + [ev("hide_source", 120, session=1, source="escape_key"), focus(1, 121)])

    def test_same_session_number_in_two_runs(self):
        report = rep.build_report(indexed(self.two_runs()), all_runs=True)
        self.assertIn("| a.exe | 快速取用 | 2 | 1 | 0 | 1 | 0 | 0 | 0 | 1 | 0 |", report)

    def test_last_run_by_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "log.jsonl")
            write_jsonl(path, self.two_runs())
            report = rep.build_report_from_path(path)
            self.assertIn("| a.exe | 快速取用 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 |", report)
            self.assertIn("最后一次启动以来", report)
            report = rep.build_report_from_path(path, last_run=False)
            self.assertIn("| a.exe | 快速取用 | 2 | 1 |", report)

    def test_from_line_drops_sessions_started_before(self):
        events = make_log()
        start = line_of(events, "shown", 1) + 1  # S1 只剩 hide_source 和 paste
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "log.jsonl")
            write_jsonl(path, events)
            report = rep.build_report_from_path(path, from_line=start)
        self.assertIn("有 1 个 session 的呼出不在范围内", report)
        self.assertIn("| Weixin.exe | 快速取用 | 4 | 2 | 0 | 0 | 1 | 1 |", report)
        self.assertIn("自第 %d 行起" % start, report)
        self.assertIn("- 启动 0 次；", report)

    def test_to_line(self):
        events = make_log()
        end = line_of(events, "paste", 1)
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "log.jsonl")
            write_jsonl(path, events, tail="{broken json\n")
            report = rep.build_report_from_path(path, to_line=end)
            self.assertIn("呼出 1 次：粘贴 1 次", report)
            self.assertIn("到第 %d 行为止" % end, report)
            self.assertNotIn("解析失败", report)
            report = rep.build_report_from_path(path)
            self.assertIn("解析失败、忽略的行：1", report)


class RobustnessTest(unittest.TestCase):
    def test_empty(self):
        report = rep.build_report([])
        self.assertIn("无（范围内没有呼出）", report)
        self.assertIn("判断不了“误提交”", report)
        self.assertIn("- 启动 0 次；呼出 0 次", report)

    def test_empty_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "empty.jsonl")
            write_jsonl(path, [])
            report = rep.build_report_from_path(path)
        self.assertIn("无（范围内没有呼出）", report)

    def test_wrong_types_do_not_crash(self):
        events = [ev("startup", 0)] + shown(1, 1, "x.exe") + [
            ev("hide_source", 2, session=1, source=5),
            ev("paste", 3, session=1, paste_result=7, reason=["a"], key_rule="Ctrl+V",
               as_text=[1], layer2="timeout", layer3=None, restore=5,
               preflight={"failed_checks": "focus"}, selected_to_send_ms="40", actual_key=1),
            ev("search_replay", 4, session=1, replayed_keys="3", dropped_keys=None),
            ev("paste_switch", 5, session=1, switched_process=None),
            ev("focus_restored", 6, session="1"),
            ev("hotkey", 7, session=True, process_name=None),
            ev("shown", 8, session=2, process_name=None, focus_class=3),
        ]
        report = rep.build_report(indexed(events))
        self.assertIn("无结论：paste_result 为空", report)
        self.assertIn("| %s | 快速取用 | 1 | 0 |" % rep.UNKNOWN, report)

    def test_auto_cycle_sessions_are_not_trials(self):
        events = [ev("startup", 0),
                  ev("shown", 10, session=1, auto=True, process_name=""),
                  ev("hidden", 20, session=1, auto=True)]
        report = rep.build_report(indexed(events))
        self.assertIn("自动显示 1 次", report)
        self.assertIn("呼出 0 次", report)


class CliTest(unittest.TestCase):
    def run_cli(self, args, log_text=None):
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "g0_matrix_report.py")
        env = dict(os.environ, PYTHONIOENCODING="utf-8")
        env.pop("NIANLAI_G0_DATA_DIR", None)
        if log_text is not None:
            tmp = tempfile.TemporaryDirectory()
            self.addCleanup(tmp.cleanup)
            path = os.path.join(tmp.name, "log.jsonl")
            with open(path, "w", encoding="utf-8") as f:
                f.write(log_text)
            args = args + ["--log", path]
        return subprocess.run([sys.executable, script] + args, capture_output=True, text=True,
                              encoding="utf-8", env=env)

    def log_text(self):
        return "\n".join(json.dumps(o, ensure_ascii=False) for o in make_log()) + "\n"

    def test_stdout(self):
        proc = self.run_cli(["--all"], self.log_text())
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("# 焦点矩阵手测汇总", proc.stdout)
        self.assertIn("| Weixin.exe | 快速取用 | 5 | 3 |", proc.stdout)

    def test_out_file_is_utf8(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "report.md")
            proc = self.run_cli(["--out", out], self.log_text())
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(proc.stdout, "")
            with open(out, "r", encoding="utf-8") as f:
                self.assertIn("## 3. 可疑试验", f.read())

    def test_missing_file_fails_cleanly(self):
        proc = self.run_cli(["--log", "Z:\\no\\such\\file.jsonl"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("读取日志失败", proc.stderr)

    def test_bad_arguments(self):
        self.assertEqual(self.run_cli(["--last-run", "--all"]).returncode, 2)
        self.assertEqual(self.run_cli(["--from-line", "5", "--to-line", "3"]).returncode, 2)
        self.assertEqual(self.run_cli(["--from-line", "0"]).returncode, 2)

    def test_default_log_follows_data_dir(self):
        with mock.patch.dict(os.environ, {"NIANLAI_G0_DATA_DIR": "D:\\g0-data"}):
            self.assertEqual(rep.default_log(), os.path.join("D:\\g0-data", "panel-log.jsonl"))
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("NIANLAI_G0_DATA_DIR", None)
            self.assertEqual(rep.default_log(), rep.base.DEFAULT_LOG)


if __name__ == "__main__":
    unittest.main(verbosity=2)
