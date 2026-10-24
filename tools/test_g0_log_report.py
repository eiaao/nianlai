#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""test_g0_log_report.py — g0_log_report 的单元测试。

capture / capture_slow / capture_summary / 监听相关事件在真实样例日志里还没有出现，
这里按字段说明伪造测试数据，覆盖采集、监听、粘贴、空日志、字段缺失等情况。
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import g0_log_report as rep  # noqa: E402

BASE = 1790000000000


def ev(event, dt, **kw):
    """构造一条日志事件：必有 event 和 time_ms，其余字段按需。"""
    obj = {"event": event, "time_ms": BASE + dt}
    obj.update(kw)
    return obj


def make_full_log():
    """伪造一份覆盖所有要汇总事件的日志（按字段说明造的 capture 数据）。"""
    events = [
        ev("startup", 0),
        ev("capture_listener_started", 1, baseline_sequence=100, listener_attached=True),
        ev("system_event", 1000, kind="suspend"),
        # ---- capture：outcome / reason / 来源进程 / 格式 / 耗时全覆盖 ----
        ev("capture", 2000, sequence=101, trigger="notification", outcome="saved",
           reason=None, marker=None, source_process="chrome.exe",
           owner_process="chrome.exe", same_process=False,
           formats=["CF_UNICODETEXT", "CF_LOCALE"], format_count=2,
           saved_formats=[{"name": "CF_UNICODETEXT", "bytes": 84, "read_ms": 0.6,
                           "global_size_fallback": False}],
           missing_formats=[], ole_bitmap_skipped=False, open_retries=0,
           open_wait_ms=0.2, read_ms=1.2, capture_ms=3.4,
           notification_to_history_ms=10, record_id="r1", kind="text", copy_count=1),
        ev("capture", 3000, sequence=102, trigger="notification", outcome="saved",
           source_process="chrome.exe", formats=["CF_UNICODETEXT"], format_count=1,
           saved_formats=[{"name": "CF_UNICODETEXT", "bytes": 40, "read_ms": 0.4,
                           "global_size_fallback": False}],
           open_retries=0, notification_to_history_ms=20, record_id="r2",
           kind="text", copy_count=1),
        ev("capture", 4000, sequence=103, trigger="notification", outcome="saved",
           source_process="WINWORD.EXE", formats=["CF_UNICODETEXT",
                                                  "Rich Text Format", "HTML Format"],
           open_retries=1, notification_to_history_ms=30, record_id="r3",
           kind="rich", copy_count=1),
        ev("capture", 5000, sequence=104, trigger="notification", outcome="saved",
           source_process="mspaint.exe", formats=["CF_DIB", "CF_BITMAP",
                                                  "CF_UNICODETEXT"],
           missing_formats=[{"name": "CF_DIB", "reason": "too_large", "read_ms": 5.0},
                            {"name": "CF_BITMAP", "reason": "ole_bitmap",
                             "global_size_fallback": True}],
           ole_bitmap_skipped=True, open_retries=2, notification_to_history_ms=40,
           record_id="r4", kind="image", copy_count=1),
        ev("capture", 6000, sequence=105, trigger="notification", outcome="saved",
           # 故意不带 source_process，覆盖字段缺失
           formats=["Shell IDList Array", "FileNameW"], open_retries=0,
           notification_to_history_ms=100, record_id="r5", kind="files", copy_count=1),
        ev("capture", 7000, sequence=106, trigger="notification", outcome="duplicate",
           reason=None),
        ev("capture", 8000, sequence=107, trigger="notification", outcome="skipped",
           reason="own_write", marker=None),
        ev("capture", 9000, sequence=108, trigger="notification", outcome="skipped",
           reason="marker", marker="secret-project", source_process="chrome.exe"),
        ev("capture", 10000, sequence=109, trigger="notification", outcome="skipped",
           reason="empty"),
        ev("capture", 11000, sequence=110, trigger="notification", outcome="stale",
           reason="sequence_changed"),
        ev("capture", 12000, sequence=111, trigger="notification", outcome="skipped",
           reason="open_failed", open_retries=3),
        ev("capture", 13000, sequence=112, trigger="notification",
           outcome="merged_same_observation"),
        ev("capture", 14000, sequence=113, trigger="notification",
           outcome="merged_late_formats"),
        ev("capture", 14500, sequence=114, trigger="notification", outcome="skipped",
           reason="hash_failed"),
        ev("capture", 14600, sequence=115, trigger="notification", outcome="skipped",
           reason="capacity", marker="tmp-note"),
        ev("capture_slow", 16000, format="CF_DIB", read_ms=800.5),
        # ---- paste：结论 / 层2 / 层3 / 耗时 ----
        ev("paste", 20000, paste_result="copied", reason="no_target_window",
           process_name="notepad.exe", actual_key="ctrl+v",
           selected_to_send_ms=10, layer2=None, layer3=None, record_id="r1",
           written_formats=["CF_UNICODETEXT"]),
        ev("paste", 21000, paste_result="sent", process_name="chrome.exe",
           actual_key="ctrl+v", selected_to_send_ms=20, selected_to_result_ms=60,
           layer2={"status": "injected", "first_reader": "chrome.exe",
                   "relative_to_send_ms": 2.5},
           layer3={"interface": "sendinput", "conclusion": "observed"}),
        ev("paste", 22000, paste_result="confirmed", process_name="notepad.exe",
           actual_key="ctrl+v", selected_to_send_ms=30, selected_to_result_ms=90,
           layer2={"status": "injected", "first_reader": "notepad.exe",
                   "relative_to_send_ms": 3.0},
           layer3={"interface": "sendinput", "conclusion": "confirmed",
                   "confirmation_ms": 120}),
        ev("paste", 23000, paste_result="copied_only", reason="modifier_held",
           layer2={"status": "missed", "first_reader": None,
                   "relative_to_send_ms": None},
           layer3=None),
        # ---- 呼出 ----
        ev("shown", 30000, hotkey_to_show_ms=50),
        ev("shown", 30100, hotkey_to_show_ms=70),
        ev("shown", 30200, hotkey_to_show_ms=90),
        ev("painted", 30010, show_to_paint_ms=11),
        ev("painted", 30110, show_to_paint_ms=22),
        ev("painted", 30210, show_to_paint_ms=33),
        ev("hide_source", 31000, source="frontend"),
        ev("hide_source", 31100, source="frontend"),
        ev("hide_source", 31200, source="escape"),
        # ---- 监听 ----
        ev("system_event", 40000, kind="resume_automatic"),
        ev("capture_self_check_failed", 41000, sequence=150,
           since_system_event_ms=1200, since_own_write_ms=5000),
        ev("capture_listener_rearmed", 41100, success=True, rebuilt_window=True,
           elapsed_ms=30),
        ev("system_event", 50000, kind="session_lock"),
        ev("capture_self_check_failed", 52000, sequence=160,
           since_system_event_ms=2500, since_own_write_ms=None),
        ev("capture_listener_rearmed", 52100, success=False, rebuilt_window=True,
           elapsed_ms=50),
        ev("capture_listener_dropped_for_test", 60000, success=True),
        ev("capture_listener_failed", 61000, error="SetClipboardViewer failed"),
        ev("history_error", 62000, reason="disk_full"),
        ev("capture_reader_stopped", 70000),
        # ---- 退出汇总（两条，报告应取最后一条）----
        ev("capture_summary", 70500, notifications=7, reads=3, outcomes={},
           skip_reasons={}, self_check_failures=0, rearm_successes=0,
           rearm_failures=0, latency_p95_ms=None, reader_finished=True),
        ev("capture_summary", 71000, notifications=42, reads=15,
           outcomes={"saved": 5, "duplicate": 1, "skipped": 6, "stale": 1,
                     "merged_same_observation": 1, "merged_late_formats": 1},
           skip_reasons={"own_write": 1, "marker": 1, "empty": 1,
                         "sequence_changed": 1, "open_failed": 1, "hash_failed": 1,
                         "capacity": 1},
           self_check_failures=2, rearm_successes=1, rearm_failures=1,
           latency_p95_ms=100.0, reader_finished=True),
    ]
    return events


def indexed(events):
    return [(i + 1, obj) for i, obj in enumerate(events)]


def write_jsonl(path, events):
    with open(path, "w", encoding="utf-8") as f:
        for obj in events:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")


class PercentileTest(unittest.TestCase):
    def test_nearest_rank(self):
        self.assertEqual(rep.percentile_nearest_rank([1, 2, 3, 4], 50), 2)
        self.assertEqual(rep.percentile_nearest_rank([1, 2, 3, 4], 95), 4)
        self.assertEqual(rep.percentile_nearest_rank([10, 20, 30, 40, 100], 50), 30)
        self.assertEqual(rep.percentile_nearest_rank([10, 20, 30, 40, 100], 95), 100)
        self.assertEqual(rep.percentile_nearest_rank([5], 50), 5)
        self.assertEqual(rep.percentile_nearest_rank([5], 95), 5)

    def test_empty(self):
        self.assertIsNone(rep.percentile_nearest_rank([], 50))
        self.assertIsNone(rep.percentile_nearest_rank([], 95))


class CaptureSectionTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_full_log()))

    def test_headers(self):
        for title in ("# 剪贴板面板诊断日志汇总", "## 2. 采集"):
            self.assertIn(title, self.report)

    def test_outcome_counts(self):
        for row in ("| saved | 5 |", "| duplicate | 1 |", "| skipped | 6 |",
                    "| stale | 1 |", "| merged_same_observation | 1 |",
                    "| merged_late_formats | 1 |"):
            self.assertIn(row, self.report)

    def test_reason_counts(self):
        for row in ("| own_write | 1 |", "| marker | 1 |", "| empty | 1 |",
                    "| sequence_changed | 1 |", "| open_failed | 1 |",
                    "| hash_failed | 1 |", "| capacity | 1 |"):
            self.assertIn(row, self.report)

    def test_latency_stats(self):
        # notification_to_history_ms = [10, 20, 30, 40, 100] -> P50=30, P95=100, max=100
        self.assertIn("| notification_to_history_ms | 5 | 30 | 100 | 100 |",
                      self.report)

    def test_by_source_process(self):
        # 带 source_process 的还有一条 marker 跳过（未记录 kind）；
        # 其余无 source_process 的读取都归入 (未记录)
        self.assertIn("| chrome.exe | 3 | text 2 / %s 1 |" % rep.NOT_RECORDED,
                      self.report)
        self.assertIn("| WINWORD.EXE | 1 | rich 1 |", self.report)
        self.assertIn("| mspaint.exe | 1 | image 1 |", self.report)
        self.assertIn("| %s | 10 | %s 9 / files 1 |" % (rep.NOT_RECORDED,
                                                        rep.NOT_RECORDED),
                      self.report)

    def test_markers(self):
        self.assertIn("`secret-project`", self.report)
        self.assertIn("`tmp-note`", self.report)

    def test_slow_read(self):
        row = "| %s | CF_DIB | 800.5 |" % rep.fmt_time(BASE + 16000)
        self.assertIn(row, self.report)

    def test_open_retries(self):
        for row in ("| 0 | 3 |", "| 1 | 1 |", "| 2 | 1 |", "| 3 | 1 |"):
            self.assertIn(row, self.report)

    def test_sequence_changed_count(self):
        self.assertIn("sequence_changed（reason 命中）共 1 次", self.report)

    def test_missing_formats(self):
        self.assertIn("| too_large | 1 |", self.report)
        self.assertIn("| ole_bitmap | 1 |", self.report)

    def test_formats_per_process(self):
        self.assertIn("- chrome.exe（共 2 个）：CF_UNICODETEXT、CF_LOCALE", self.report)
        self.assertIn("- mspaint.exe（共 3 个）：CF_DIB、CF_BITMAP、CF_UNICODETEXT",
                      self.report)
        self.assertIn("Rich Text Format", self.report)
        self.assertIn("FileNameW", self.report)

    def test_formats_capped_at_20(self):
        many = [ev("capture", i, outcome="saved", source_process="noise.exe",
                   formats=["Format%02d" % k for k in range(25)]) for i in range(1)]
        report = rep.build_report(indexed(many))
        self.assertIn("共 25 个，仅列前 20 个", report)
        self.assertIn("Format19", report)
        self.assertNotIn("Format24", report)


class ListenerSectionTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_full_log()))

    def test_listener_started(self):
        row = "| %s | 100 |" % rep.fmt_time(BASE + 1)
        self.assertIn(row, self.report)

    def test_self_check_failures(self):
        ok_row = "| %s | 150 | 1200 | 5000 | 成功（30 ms，重建窗口） |" % rep.fmt_time(BASE + 41000)
        self.assertIn(ok_row, self.report)
        fail_row = "| %s | 160 | 2500 | %s | 失败（50 ms） |" % (
            rep.fmt_time(BASE + 52000), rep.NO_DATA)
        self.assertIn(fail_row, self.report)

    def test_rearm_totals(self):
        self.assertIn("重挂（capture_listener_rearmed）：共 2 次，成功 1 次，失败 1 次",
                      self.report)

    def test_misc_counters(self):
        self.assertIn("测试性摘除（capture_listener_dropped_for_test）：1 次，success=true 1 次",
                      self.report)
        self.assertIn("读线程停止（capture_reader_stopped）：1 次", self.report)

    def test_system_event_timeline(self):
        for kind in ("suspend", "resume_automatic", "session_lock"):
            self.assertIn("| %s |" % kind, self.report)


class PasteSectionTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_full_log()))

    def test_result_counts(self):
        for row in ("| copied | 1 |", "| sent | 1 |", "| confirmed | 1 |",
                    "| copied_only | 1 |"):
            self.assertIn(row, self.report)

    def test_reason_counts(self):
        self.assertIn("| no_target_window | 1 |", self.report)
        self.assertIn("| modifier_held | 1 |", self.report)

    def test_layer2_status(self):
        self.assertIn("| injected | 2 |", self.report)
        self.assertIn("| missed | 1 |", self.report)

    def test_layer3_conclusion(self):
        self.assertIn("| observed | 1 |", self.report)
        self.assertIn("| confirmed | 1 |", self.report)

    def test_timings(self):
        self.assertIn("| selected_to_send_ms | 3 | 20 | 30 | 30 |", self.report)
        self.assertIn("| selected_to_result_ms | 2 | 60 | 90 | 90 |", self.report)
        self.assertIn("| layer3.confirmation_ms | 1 | 120 | 120 | 120 |", self.report)


class ShowSectionTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_full_log()))

    def test_timings(self):
        self.assertIn("| hotkey_to_show_ms（shown，3 条） | 3 | 70 | 90 | 90 |",
                      self.report)
        self.assertIn("| show_to_paint_ms（painted，3 条） | 3 | 22 | 33 | 33 |",
                      self.report)

    def test_hide_source(self):
        self.assertIn("| frontend | 2 |", self.report)
        self.assertIn("| escape | 1 |", self.report)


class ErrorsAndSummaryTest(unittest.TestCase):
    def setUp(self):
        self.report = rep.build_report(indexed(make_full_log()))

    def test_error_events_listed(self):
        self.assertIn("## 6. 错误", self.report)
        self.assertIn("capture_listener_failed", self.report)
        self.assertIn("SetClipboardViewer failed", self.report)
        self.assertIn("history_error", self.report)
        # 事件名带 failed 的自检失效也应出现在错误一节
        self.assertIn("capture_self_check_failed", self.report)

    def test_summary_is_last_one_verbatim(self):
        self.assertIn('"notifications": 42', self.report)
        self.assertIn('"reader_finished": true', self.report)
        self.assertIn('"latency_p95_ms": 100.0', self.report)
        self.assertNotIn('"notifications": 7', self.report)


class EmptyLogTest(unittest.TestCase):
    def test_no_events(self):
        report = rep.build_report([])
        self.assertIn("## 1. 范围", report)
        self.assertIn("| 行数 | 0 |", report)
        self.assertIn("| 启动次数 | 0 |", report)
        # 每一类都没有事件时写「无」
        self.assertGreaterEqual(report.count(rep.NO_DATA), 5)

    def test_empty_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "empty.jsonl")
            with open(path, "w", encoding="utf-8"):
                pass
            report = rep.build_report_from_path(path, last_run=False)
        self.assertIn("| 行数 | 0 |", report)
        self.assertIn(rep.NO_DATA, report)


class MissingFieldsTest(unittest.TestCase):
    def test_minimal_events_do_not_crash(self):
        events = indexed([
            ev("capture", 0),
            ev("paste", 1),
            ev("shown", 2),
            ev("capture_self_check_failed", 3),
            ev("system_event", 4),
            ev("capture_summary", 5),
        ])
        report = rep.build_report(events)  # 不应抛异常
        self.assertIn("| %s | 1 |" % rep.NOT_RECORDED, report)  # outcome 缺失
        self.assertIn(rep.NO_DATA, report)

    def test_null_fields_do_not_crash(self):
        events = indexed([
            ev("capture", 0, outcome=None, reason=None, marker=None,
               source_process=None, kind=None, formats=None,
               notification_to_history_ms=None, open_retries=None,
               missing_formats=None),
            ev("paste", 1, paste_result=None, reason=None, layer2=None,
               layer3=None, selected_to_send_ms=None),
        ])
        report = rep.build_report(events)
        self.assertIn("## 2. 采集", report)
        self.assertIn("## 4. 粘贴", report)


class ScopeTest(unittest.TestCase):
    def setUp(self):
        self.events = [
            ev("startup", 0),
            ev("shown", 10, hotkey_to_show_ms=50),
            ev("shown", 20, hotkey_to_show_ms=70),
            ev("startup", 30),
            ev("shown", 40, hotkey_to_show_ms=90),
            ev("shown", 50, hotkey_to_show_ms=110),
        ]

    def write(self, tmp):
        path = os.path.join(tmp, "scope.jsonl")
        write_jsonl(path, self.events)
        return path

    def test_last_run_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = rep.build_report_from_path(self.write(tmp), last_run=True)
        self.assertIn("| 行数 | 3 |", report)
        self.assertIn("| 启动次数 | 1 |", report)
        self.assertIn("| hotkey_to_show_ms（shown，2 条） | 2 | 90 | 110 | 110 |",
                      report)

    def test_all(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = rep.build_report_from_path(self.write(tmp), last_run=False)
        self.assertIn("| 行数 | 6 |", report)
        self.assertIn("| 启动次数 | 2 |", report)

    def test_from_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.write(tmp)
            # 第 2 行起：最后一次 startup 在第 4 行 -> 只剩第 4~6 行
            report = rep.build_report_from_path(path, from_line=2, last_run=True)
            self.assertIn("| 行数 | 3 |", report)
            # 第 5 行起：没有 startup，整段都在范围内
            report = rep.build_report_from_path(path, from_line=5, last_run=True)
            self.assertIn("| 行数 | 2 |", report)
            self.assertIn("| 启动次数 | 0 |", report)

    def test_bad_lines_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "bad.jsonl")
            with open(path, "w", encoding="utf-8") as f:
                f.write(json.dumps(ev("shown", 0, hotkey_to_show_ms=10)) + "\n")
                f.write("\n")  # 空行
                f.write("{broken json\n")
                f.write(json.dumps(ev("shown", 1, hotkey_to_show_ms=20)) + "\n")
            report = rep.build_report_from_path(path, last_run=False)
        self.assertIn("| 行数 | 2 |", report)
        self.assertIn("解析失败/忽略的行", report)


class CliTest(unittest.TestCase):
    def run_cli(self, args, log_text=None):
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                              "g0_log_report.py")
        env = dict(os.environ, PYTHONIOENCODING="utf-8")
        if log_text is not None:
            tmp = tempfile.TemporaryDirectory()
            self.addCleanup(tmp.cleanup)
            path = os.path.join(tmp.name, "log.jsonl")
            with open(path, "w", encoding="utf-8") as f:
                f.write(log_text)
            args = args + ["--log", path]
        proc = subprocess.run([sys.executable, script] + args,
                              capture_output=True, text=True, encoding="utf-8",
                              env=env)
        return proc

    def test_all_runs(self):
        log_text = "\n".join(
            json.dumps(o, ensure_ascii=False) for o in make_full_log()) + "\n"
        proc = self.run_cli(["--all"], log_text)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("剪贴板面板诊断日志汇总", proc.stdout)
        self.assertIn("## 7. 退出汇总", proc.stdout)
        self.assertIn('"notifications": 42', proc.stdout)

    def test_missing_file_fails_cleanly(self):
        proc = self.run_cli(["--log", "Z:\\no\\such\\file.jsonl", "--all"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("读取日志失败", proc.stderr)

    def test_last_run_and_all_are_exclusive(self):
        proc = self.run_cli(["--last-run", "--all"])
        self.assertEqual(proc.returncode, 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
