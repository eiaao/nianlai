using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;
using System.Windows.Forms;

namespace Nianlai.NativeEditProbe
{
    internal static class Program
    {
        internal const string Fixture = "NIANLAI_DIRECT_EDIT_V1 | English 中文验收 😀 | <&> {\\} END";
        private static readonly object OutputLock = new object();

        [STAThread]
        private static int Main(string[] args)
        {
            try
            {
                if (args.Length == 1 && args[0] == "--self-test") return SelfTest();
                if (args.Length == 3 && args[0] == "--run" && args[1] == "--output") return Run(args[2]);
                if (args.Length == 4 && args[0] == "--child") return Child(args[1], args[2], Int32.Parse(args[3], CultureInfo.InvariantCulture));
                Console.Error.WriteLine("Use --self-test or --run --output ABSOLUTE_NEW_DIRECTORY. No default live action.");
                return 2;
            }
            catch (Exception error)
            {
                Console.Error.WriteLine(Json(Obj("ok", false, "error_type", error.GetType().Name, "error", error.Message)));
                return 2;
            }
        }

        internal static Dictionary<string, object> Obj(params object[] pairs)
        {
            var value = new Dictionary<string, object>();
            for (int index = 0; index < pairs.Length; index += 2) value.Add((string)pairs[index], pairs[index + 1]);
            return value;
        }
        internal static string Json(object value) { return new JavaScriptSerializer().Serialize(value); }
        internal static void Check(bool value, string code) { if (!value) throw new InvalidOperationException(code); }
        internal static string Hash(string value)
        {
            using (var hash = SHA256.Create()) return BitConverter.ToString(hash.ComputeHash(Encoding.UTF8.GetBytes(value))).Replace("-", "").ToLowerInvariant();
        }
        internal static int Number(Dictionary<string, object> value, string key) { return Convert.ToInt32(value[key], CultureInfo.InvariantCulture); }
        internal static long LongNumber(Dictionary<string, object> value, string key) { return Convert.ToInt64(value[key], CultureInfo.InvariantCulture); }
        internal static Dictionary<string, object> ObjectAt(Dictionary<string, object> value, string key) { return (Dictionary<string, object>)value[key]; }
        internal static void WriteFrame(string nonce, string kind, object state)
        {
            lock (OutputLock)
            {
                Console.WriteLine(Json(Obj("nonce", nonce, "kind", kind, "state", state)));
                Console.Out.Flush();
            }
        }
        private static void SaveNew(string path, object value)
        {
            using (var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.Read))
            using (var writer = new StreamWriter(stream, new UTF8Encoding(false))) writer.Write(Json(value));
        }

        private static CaseSpec[] Cases()
        {
            return new[] {
                new CaseSpec("raw-caret", "raw", "LR", 1, 1, Fixture, 512, false, false, false, "normal", null, true),
                new CaseSpec("forms-selection-crlf", "forms", "L_old_R", 2, 5, Fixture + "\r\nLine2", 512, true, false, false, "normal", null, true),
                new CaseSpec("raw-limit-exact", "raw", "ABCD", 1, 3, "中😀", 5, false, false, false, "normal", null, true),
                new CaseSpec("forms-limit-refused", "forms", "ABCD", 1, 3, "中文😀", 5, false, false, false, "normal", "capacity_exceeded", false),
                new CaseSpec("forms-readonly-refused", "forms", "ABCD", 1, 3, Fixture, 512, false, true, false, "normal", "readonly", false),
                new CaseSpec("raw-password-refused", "raw", "ABCD", 1, 3, Fixture, 512, false, false, true, "normal", "password", false),
                new CaseSpec("raw-timeout-after-entry", "raw", "LR", 1, 1, Fixture, 512, false, false, false, "timeout", null, false),
                new CaseSpec("forms-early-reply", "forms", "L_old_R", 2, 5, Fixture, 512, false, false, false, "early_reply", null, false)
            };
        }
        private static CaseSpec FindCase(string name)
        {
            foreach (var value in Cases()) if (value.Name == name) return value;
            throw new InvalidOperationException("unknown_fixed_case");
        }

        // Pure path: no Process, Control, event, Marshal allocation or P/Invoke is reached.
        private static int SelfTest()
        {
            int checks = 0;
            Action<bool, string> test = delegate(bool ok, string name) { Check(ok, name); checks++; };
            Payload payload; string reason;
            test(Payload.TryCreate(Encoding.Unicode.GetBytes(Fixture), false, out payload, out reason), "unicode_payload");
            test(payload.Units == Fixture.Length && payload.Bytes.Length == 2 * (Fixture.Length + 1), "payload_units");
            test(payload.Bytes[payload.Bytes.Length - 1] == 0 && payload.Bytes[payload.Bytes.Length - 2] == 0, "payload_nul");
            test(!Payload.TryCreate(new byte[0], false, out payload, out reason) && reason == "empty", "empty_not_delete");
            test(!Payload.TryCreate(new byte[] { 1 }, false, out payload, out reason) && reason == "odd_utf16", "odd_rejected");
            test(!Payload.TryCreate(new byte[] { 0, 216 }, false, out payload, out reason) && reason == "invalid_utf16", "surrogate_rejected");
            test(!Payload.TryCreate(Encoding.Unicode.GetBytes("A\0B"), false, out payload, out reason) && reason == "embedded_nul", "nul_rejected");
            test(!Payload.TryCreate(Encoding.Unicode.GetBytes("A\r\nB"), false, out payload, out reason), "single_line_break_rejected");
            test(Payload.TryCreate(Encoding.Unicode.GetBytes("A\r\nB"), true, out payload, out reason), "multiline_crlf");
            test(!Payload.TryCreate(Encoding.Unicode.GetBytes("A\nB"), true, out payload, out reason), "bare_lf_rejected");
            test(!Payload.TryCreate(Encoding.Unicode.GetBytes("A\rB"), true, out payload, out reason), "bare_cr_rejected");
            test(!Payload.TryCreate(Encoding.Unicode.GetBytes(new string('a', 32768)), true, out payload, out reason), "payload_bound");
            test(Payload.TryCreate(Encoding.Unicode.GetBytes("中😀"), false, out payload, out reason) && payload.Units == 3, "emoji_units");
            var metadata = new Metadata { ClassName = "Edit", Unicode = true, Length = 4, Start = 1, End = 3, Limit = 5 };
            test(Rules.Refusal(metadata, payload) == null, "exact_capacity");
            metadata.End = 2; test(Rules.Refusal(metadata, payload) == "capacity_exceeded", "capacity_refused");
            metadata.End = 3; metadata.Style = Native.ES_READONLY; test(Rules.Refusal(metadata, payload) == "readonly", "readonly_refused");
            metadata.Style = Native.ES_PASSWORD; test(Rules.Refusal(metadata, payload) == "password", "password_style_refused");
            metadata.Style = 0; metadata.PasswordCharacter = 42; test(Rules.Refusal(metadata, payload) == "password", "password_character_refused");
            metadata.PasswordCharacter = 0; metadata.ClassName = "RichEdit20W"; test(Rules.Refusal(metadata, payload) == "unsupported_class", "richedit_refused");
            metadata.ClassName = "WindowsForms10.EDIT.app.test"; test(Rules.Refusal(metadata, payload) == null, "forms_qualified");
            metadata.Unicode = false; test(Rules.Refusal(metadata, payload) == "ansi_control", "ansi_refused");
            metadata.Unicode = true; metadata.Style = Native.ES_NUMBER; test(Rules.Refusal(metadata, payload) == "transforming_style", "style_refused");
            metadata.Style = 0; metadata.Length = 65536; test(Rules.Refusal(metadata, payload) == "range_too_large", "packed_selection_bound");
            metadata.Length = 4; metadata.End = 6; test(Rules.Refusal(metadata, payload) == "invalid_selection", "selection_bounds");
            metadata.End = 3; metadata.Limit = 0; test(Rules.Refusal(metadata, payload) == "capacity_exceeded", "zero_limit_not_unlimited");
            test(Delivery.Status(false, false) == "not_submitted", "not_called");
            test(Delivery.Status(true, false) == "unknown", "timeout_unknown");
            test(Delivery.Status(true, true) == "sent_unconfirmed", "ack_not_content");
            var names = new HashSet<string>();
            foreach (var spec in Cases()) { test(names.Add(spec.Name), "unique_case"); test(spec.ExpectedText.Length < 512, "short_fixture"); }
            Console.WriteLine(Json(Obj("ok", true, "pure_checks_passed", checks, "native_api_calls", 0, "controls_created", 0,
                "child_processes_started", 0, "clipboard_accesses", 0, "input_events", 0, "live_modes_executed", 0)));
            return 0;
        }

        private static int Run(string output)
        {
            Check(Path.IsPathRooted(output) && Path.GetFullPath(output) == output, "absolute_output_required");
            Check(!Directory.Exists(output) && !File.Exists(output), "new_output_required");
            Directory.CreateDirectory(output);
            var results = new List<object>();
            bool passed = true;
            foreach (var spec in Cases())
            {
                var result = RunCase(spec);
                SaveNew(Path.Combine(output, spec.Name + ".json"), result);
                results.Add(result);
                if (!(bool)result["passed"]) { passed = false; break; }
            }
            var summary = Obj("schema", 1, "ok", passed, "executed_at", DateTimeOffset.UtcNow.ToString("o"),
                "cases", results, "clipboard_accesses", 0, "input_events", 0, "activation_requests", 0,
                "scope", "owned_hidden_cross_process_controls_only");
            SaveNew(Path.Combine(output, "summary.json"), summary);
            Console.WriteLine(Json(summary));
            return passed ? 0 : 1;
        }

        private static Dictionary<string, object> RunCase(CaseSpec spec)
        {
            string nonce = Guid.NewGuid().ToString("N");
            var result = Obj("case", spec.Name, "control_kind", spec.Kind, "scenario", spec.Scenario,
                "passed", false, "mutation_attempts", 0, "expected_length", spec.ExpectedText.Length,
                "expected_sha256_utf8", Hash(spec.ExpectedText), "child_exit_observed", false, "child_exit_code", null);
            Process process = null; bool started = false;
            var stderr = new StringBuilder();
            using (var entered = NewEvent(nonce, "entered"))
            using (var release = NewEvent(nonce, "release"))
            using (var completed = NewEvent(nonce, "completed"))
            {
                try
                {
                    var start = new ProcessStartInfo(Assembly.GetExecutingAssembly().Location,
                        "--child " + spec.Name + " " + nonce + " " + Process.GetCurrentProcess().Id.ToString(CultureInfo.InvariantCulture));
                    start.UseShellExecute = false;
                    start.CreateNoWindow = true;
                    start.WindowStyle = ProcessWindowStyle.Hidden;
                    start.RedirectStandardInput = true; start.RedirectStandardOutput = true; start.RedirectStandardError = true;
                    start.StandardOutputEncoding = Encoding.UTF8; start.StandardErrorEncoding = Encoding.UTF8;
                    process = new Process(); process.StartInfo = start;
                    process.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs args)
                    {
                        if (args.Data != null) lock (stderr) { if (stderr.Length < 4096) stderr.AppendLine(args.Data); }
                    };
                    Check(process.Start(), "child_start_failed"); started = true;
                    IntPtr originalHandle = process.Handle; Check(originalHandle != IntPtr.Zero, "child_handle_missing");
                    result["child_pid"] = process.Id;
                    result["child_started_at"] = process.StartTime.ToUniversalTime().ToString("o");
                    process.BeginErrorReadLine();
                    var ready = ReadFrame(process, nonce, "ready");
                    var identity = ObjectAt(ready, "identity");
                    Check(Number(identity, "pid") == process.Id, "ready_pid_mismatch");
                    IntPtr target = new IntPtr(LongNumber(identity, "target"));
                    IntPtr root = new IntPtr(LongNumber(identity, "root"));
                    uint thread = Convert.ToUInt32(identity["thread"], CultureInfo.InvariantCulture);
                    var initial = ObjectAt(ready, "snapshot");
                    CheckSnapshot(initial, spec.Before, 0, 0, "initial");
                    Check(Number(initial, "selection_start") == spec.Start && Number(initial, "selection_end") == spec.End, "initial_selection");
                    result["initial"] = initial;
                    Metadata metadata = ReadMetadata(target, root, thread, process.Id);
                    result["metadata"] = metadata.Report();
                    Payload payload; string payloadReason;
                    Check(Payload.TryCreate(Encoding.Unicode.GetBytes(spec.Payload), metadata.Multiline, out payload, out payloadReason), "fixed_payload_" + payloadReason);
                    string refusal = Rules.Refusal(metadata, payload);
                    if (spec.ExpectedRefusal != null)
                    {
                        Check(refusal == spec.ExpectedRefusal, "expected_refusal_mismatch");
                        result["transport_status"] = Delivery.Status(false, false); result["refusal"] = refusal;
                        SendCommand(process, nonce, "snapshot");
                        var unchanged = ReadFrame(process, nonce, "snapshot");
                        CheckSnapshot(unchanged, spec.Before, 0, 0, "refused");
                        result["after"] = unchanged;
                    }
                    else
                    {
                        Check(refusal == null, "unexpected_refusal_" + refusal);
                        VerifyOwnedHidden(target, root, thread, process.Id);
                        // The helper clears/frees the sender allocation before returning to release this gate.
                        Delivery delivery = SendReplacement(target, payload, spec.Scenario == "timeout" ? 100U : 1000U,
                            delegate { result["mutation_attempts"] = 1; });
                        result["transport_status"] = delivery.TransportStatus;
                        result["acknowledged"] = delivery.Acknowledged;
                        result["error_code"] = delivery.ErrorCode;
                        result["sender_buffer_cleared_and_freed"] = true;
                        bool wasEntered = entered.WaitOne(0);
                        bool completedBeforeRelease = completed.WaitOne(0);
                        result["entered_before_release"] = wasEntered;
                        result["completed_before_release"] = completedBeforeRelease;
                        if (spec.Scenario == "timeout") Check(!delivery.Acknowledged && wasEntered && !completedBeforeRelease, "timeout_lifecycle_not_observed");
                        else if (spec.Scenario == "early_reply") Check(delivery.Acknowledged && wasEntered && !completedBeforeRelease, "early_reply_lifecycle_not_observed");
                        else Check(delivery.Acknowledged, "normal_call_not_acknowledged");
                        release.Set();
                        Check(completed.WaitOne(5000), "target_did_not_complete");
                        SendCommand(process, nonce, "snapshot");
                        var after = ReadFrame(process, nonce, "snapshot");
                        CheckSnapshot(after, spec.ExpectedText, 1, 1, "inserted");
                        Check(Number(after, "before_length") == spec.Before.Length && (string)after["before_sha256_utf8"] == Hash(spec.Before), "before_observation");
                        Check(Number(after, "after_length") == spec.ExpectedText.Length && (string)after["after_sha256_utf8"] == Hash(spec.ExpectedText), "after_observation");
                        int caret = spec.Start + spec.Payload.Length;
                        Check(Number(after, "selection_start") == caret && Number(after, "selection_end") == caret, "result_selection");
                        if (spec.Scenario == "early_reply") Check((bool)after["reply_message_succeeded"], "reply_message_failed");
                        result["after"] = after;
                        if (spec.Undo)
                        {
                            Check((bool)after["can_undo"], "undo_unavailable");
                            SendCommand(process, nonce, "undo");
                            var undone = ReadFrame(process, nonce, "undo");
                            CheckSnapshot(undone, spec.Before, 1, 1, "undo");
                            result["undo"] = undone;
                        }
                    }
                    result["passed"] = true;
                }
                catch (Exception error)
                {
                    result["error_type"] = error.GetType().Name; result["error"] = error.Message;
                }
                finally
                {
                    release.Set();
                    if (started)
                    {
                        try { SendCommand(process, nonce, "exit"); } catch (Exception error) { result["close_error_type"] = error.GetType().Name; }
                        bool exited = process.WaitForExit(15000);
                        result["child_exit_observed"] = exited;
                        if (exited)
                        {
                            process.WaitForExit(); // Already exited; finish the asynchronous stderr reader.
                            result["child_exit_code"] = process.ExitCode;
                            if (process.ExitCode != 0) result["passed"] = false;
                        }
                        else result["passed"] = false;
                    }
                    else result["passed"] = false;
                    if (process != null) process.Dispose();
                    lock (stderr) { if (stderr.Length != 0) result["child_stderr"] = stderr.ToString(); }
                }
            }
            return result;
        }

        private static EventWaitHandle NewEvent(string nonce, string suffix)
        {
            bool created;
            var value = new EventWaitHandle(false, EventResetMode.ManualReset, EventName(nonce, suffix), out created);
            if (!created) { value.Dispose(); throw new InvalidOperationException("new_gate_required"); }
            return value;
        }
        internal static string EventName(string nonce, string suffix) { return "Local\\NianlaiNativeEditProbe_" + nonce + "_" + suffix; }
        private static void SendCommand(Process child, string nonce, string command) { child.StandardInput.WriteLine(nonce + " " + command); child.StandardInput.Flush(); }
        private static Dictionary<string, object> ReadFrame(Process child, string nonce, string kind)
        {
            Task<string> task = child.StandardOutput.ReadLineAsync();
            Check(task.Wait(5000), "child_protocol_timeout");
            Check(task.Result != null, "child_protocol_eof");
            var frame = new JavaScriptSerializer().Deserialize<Dictionary<string, object>>(task.Result);
            Check((string)frame["nonce"] == nonce && (string)frame["kind"] == kind, "child_protocol_identity");
            return ObjectAt(frame, "state");
        }
        private static void CheckSnapshot(Dictionary<string, object> state, string expected, int entered, int completed, string stage)
        {
            Check(Number(state, "length") == expected.Length && (string)state["sha256_utf8"] == Hash(expected), stage + "_text_mismatch");
            Check(Number(state, "mutation_entered") == entered && Number(state, "mutation_completed") == completed, stage + "_mutation_count");
            Check(state["failure"] == null, stage + "_receiver_failure");
        }
        private static void VerifyOwnedHidden(IntPtr target, IntPtr root, uint thread, int childPid)
        {
            uint process;
            Check(childPid != Process.GetCurrentProcess().Id && thread != Native.GetCurrentThreadId(), "cross_process_required");
            Check(Native.IsWindow(target) && Native.IsWindow(root), "owned_window_missing");
            Check(Native.GetWindowThreadProcessId(target, out process) == thread && process == (uint)childPid, "target_identity_changed");
            Check(Native.GetWindowThreadProcessId(root, out process) == thread && process == (uint)childPid, "root_identity_changed");
            Check(Native.GetAncestor(target, 2) == root, "root_changed");
            Check(!Native.IsWindowVisible(root) && !Native.IsWindowVisible(target), "fixture_must_remain_hidden");
            Check(Native.IsWindowEnabled(root) && Native.IsWindowEnabled(target), "owned_control_disabled");
        }
        private static Metadata ReadMetadata(IntPtr target, IntPtr root, uint thread, int childPid)
        {
            VerifyOwnedHidden(target, root, thread, childPid);
            var name = new StringBuilder(256);
            Check(Native.GetClassNameW(target, name, name.Capacity) > 0, "class_query_failed");
            Native.SetLastError(0);
            long style = Native.GetWindowLongPtrW(target, -16).ToInt64();
            int error = Marshal.GetLastWin32Error(); Check(style != 0 || error == 0, "style_query_failed");
            var budget = Stopwatch.StartNew();
            ulong length = Query(target, Native.WM_GETTEXTLENGTH, budget);
            // Packed EM_GETSEL is exact only for these deliberately short fixtures.
            Check(length <= 32767, "range_too_large");
            ulong selection = Query(target, Native.EM_GETSEL, budget);
            Check(selection <= UInt32.MaxValue && selection != UInt32.MaxValue, "packed_selection_unavailable");
            ulong limit = Query(target, Native.EM_GETLIMITTEXT, budget);
            ulong password = Query(target, Native.EM_GETPASSWORDCHAR, budget);
            VerifyOwnedHidden(target, root, thread, childPid);
            Check(limit <= Int32.MaxValue && password <= UInt16.MaxValue, "metadata_out_of_range");
            return new Metadata { ClassName = name.ToString(), Unicode = Native.IsWindowUnicode(target), Style = style,
                Length = (int)length, Start = (int)(selection & 65535), End = (int)((selection >> 16) & 65535),
                Limit = (int)limit, PasswordCharacter = (int)password };
        }
        private static ulong Query(IntPtr target, uint message, Stopwatch budget)
        {
            long remaining = 200 - budget.ElapsedMilliseconds;
            Check(remaining > 0, "query_budget_exhausted");
            UIntPtr value;
            Native.SetLastError(0);
            IntPtr result = Native.SendMessageTimeoutW(target, message, UIntPtr.Zero, IntPtr.Zero, Native.SendFlags, (uint)remaining, out value);
            int error = Marshal.GetLastWin32Error();
            Check(result != IntPtr.Zero, "metadata_query_failed_" + error.ToString(CultureInfo.InvariantCulture));
            return value.ToUInt64();
        }
        private static Delivery SendReplacement(IntPtr target, Payload payload, uint timeout, Action markAttempt)
        {
            IntPtr buffer = Marshal.AllocHGlobal(payload.Bytes.Length);
            try
            {
                Marshal.Copy(payload.Bytes, 0, buffer, payload.Bytes.Length);
                UIntPtr ignored;
                Native.SetLastError(0);
                markAttempt();
                IntPtr result = Native.SendMessageTimeoutW(target, Native.EM_REPLACESEL, new UIntPtr(1), buffer, Native.SendFlags, timeout, out ignored);
                int error = Marshal.GetLastWin32Error();
                return new Delivery { Acknowledged = result != IntPtr.Zero, ErrorCode = result != IntPtr.Zero ? (int?)null : error };
            }
            finally
            {
                // Deliberately erase and free BEFORE the driver releases a delayed receiver.
                Marshal.Copy(new byte[payload.Bytes.Length], 0, buffer, payload.Bytes.Length);
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static int Child(string caseName, string nonce, int parentPid)
        {
            Check(nonce.Length == 32 && Guid.ParseExact(nonce, "N").ToString("N") == nonce, "invalid_child_nonce");
            using (var parent = Process.GetProcessById(parentPid)) Check(!parent.HasExited && parentPid != Process.GetCurrentProcess().Id, "parent_not_alive");
            Console.OutputEncoding = new UTF8Encoding(false);
            var spec = FindCase(caseName);
            using (var entered = EventWaitHandle.OpenExisting(EventName(nonce, "entered")))
            using (var release = EventWaitHandle.OpenExisting(EventName(nonce, "release")))
            using (var completed = EventWaitHandle.OpenExisting(EventName(nonce, "completed")))
            using (var watchdog = new System.Threading.Timer(delegate { Console.Error.WriteLine("child_watchdog_timeout"); Environment.Exit(124); }, null, 12000, Timeout.Infinite))
            using (var host = new ProbeHost(spec, entered, release, completed))
            {
                var context = new ApplicationContext();
                int exitCode = 0;
                Application.ThreadException += delegate(object sender, ThreadExceptionEventArgs args)
                {
                    exitCode = 1; Console.Error.WriteLine("child_ui_error_" + args.Exception.GetType().Name); context.ExitThread();
                };
                var input = new Thread(delegate()
                {
                    try
                    {
                        for (;;)
                        {
                            string line = Console.ReadLine();
                            if (line == null) { host.Queue(delegate { context.ExitThread(); }); return; }
                            Check(line.StartsWith(nonce + " ", StringComparison.Ordinal), "child_command_nonce");
                            string command = line.Substring(nonce.Length + 1);
                            if (command == "exit") { host.Queue(delegate { context.ExitThread(); }); return; }
                            if (command == "snapshot") host.Queue(delegate { WriteFrame(nonce, "snapshot", host.Snapshot()); });
                            else if (command == "undo") host.Queue(delegate { host.UndoOnce(); WriteFrame(nonce, "undo", host.Snapshot()); });
                            else throw new InvalidOperationException("unknown_child_command");
                        }
                    }
                    catch (Exception error)
                    {
                        Console.Error.WriteLine("child_protocol_error_" + error.GetType().Name);
                        Environment.Exit(1);
                    }
                });
                input.IsBackground = true;
                // Ready means the UI loop has actually dispatched this first callback.
                host.Queue(delegate
                {
                    input.Start();
                    WriteFrame(nonce, "ready", Obj("identity", Obj("pid", Process.GetCurrentProcess().Id,
                        "thread", Native.GetCurrentThreadId(), "root", host.Root.ToInt64(), "target", host.Target.ToInt64()), "snapshot", host.Snapshot()));
                });
                // No MainForm argument: Application.Run(Form) would show a window.
                Application.Run(context);
                return host.Failure == null ? exitCode : 1;
            }
        }
    }

    internal sealed class CaseSpec
    {
        internal readonly string Name, Kind, Before, Payload, Scenario, ExpectedRefusal;
        internal readonly int Start, End, Limit;
        internal readonly bool Multiline, ReadOnly, Password, Undo;
        internal CaseSpec(string name, string kind, string before, int start, int end, string payload, int limit,
            bool multiline, bool readOnly, bool password, string scenario, string refusal, bool undo)
        { Name = name; Kind = kind; Before = before; Start = start; End = end; Payload = payload; Limit = limit;
            Multiline = multiline; ReadOnly = readOnly; Password = password; Scenario = scenario; ExpectedRefusal = refusal; Undo = undo; }
        internal string ExpectedText { get { return ExpectedRefusal != null ? Before : Before.Substring(0, Start) + Payload + Before.Substring(End); } }
    }
    internal sealed class Payload
    {
        internal byte[] Bytes; internal int Units;
        internal static bool TryCreate(byte[] bytes, bool multiline, out Payload payload, out string reason)
        {
            payload = null; reason = null;
            if (bytes.Length == 0) reason = "empty";
            else if ((bytes.Length & 1) != 0) reason = "odd_utf16";
            else if (bytes.Length / 2 > 32767) reason = "payload_too_large";
            if (reason != null) return false;
            string text;
            try { text = new UnicodeEncoding(false, false, true).GetString(bytes); }
            catch (DecoderFallbackException) { reason = "invalid_utf16"; return false; }
            if (text.IndexOf('\0') >= 0) { reason = "embedded_nul"; return false; }
            for (int index = 0; index < text.Length; index++)
            {
                if (text[index] == '\r' && multiline && index + 1 < text.Length && text[index + 1] == '\n') { index++; continue; }
                if (text[index] == '\r' || text[index] == '\n') { reason = "unsupported_line_break"; return false; }
            }
            var terminated = new byte[bytes.Length + 2]; Buffer.BlockCopy(bytes, 0, terminated, 0, bytes.Length);
            payload = new Payload { Bytes = terminated, Units = text.Length }; return true;
        }
    }
    internal sealed class Metadata
    {
        internal string ClassName; internal bool Unicode; internal long Style;
        internal int Length, Start, End, Limit, PasswordCharacter;
        internal bool Multiline { get { return (Style & Native.ES_MULTILINE) != 0; } }
        internal object Report() { return Program.Obj("class", ClassName, "unicode", Unicode, "multiline", Multiline,
            "auto_hscroll", (Style & Native.ES_AUTOHSCROLL) != 0,
            "readonly", (Style & Native.ES_READONLY) != 0, "password", (Style & Native.ES_PASSWORD) != 0 || PasswordCharacter != 0,
            "length", Length, "selection_start", Start, "selection_end", End, "limit", Limit); }
    }
    internal static class Rules
    {
        internal static string Refusal(Metadata value, Payload payload)
        {
            if (!(String.Equals(value.ClassName, "Edit", StringComparison.OrdinalIgnoreCase) || value.ClassName.StartsWith("WindowsForms10.EDIT.", StringComparison.OrdinalIgnoreCase))) return "unsupported_class";
            if (!value.Unicode) return "ansi_control";
            if ((value.Style & Native.ES_READONLY) != 0) return "readonly";
            if ((value.Style & Native.ES_PASSWORD) != 0 || value.PasswordCharacter != 0) return "password";
            if ((value.Style & (Native.ES_NUMBER | Native.ES_UPPERCASE | Native.ES_LOWERCASE | Native.ES_OEMCONVERT)) != 0) return "transforming_style";
            if (value.Length < 0 || value.Length > 32767) return "range_too_large";
            if (value.Start < 0 || value.End < value.Start || value.End > value.Length) return "invalid_selection";
            long result = (long)value.Length - (value.End - value.Start) + payload.Units;
            if (result > 32767 || result > value.Limit || value.Limit <= 0) return "capacity_exceeded";
            return null;
        }
    }
    internal sealed class Delivery
    {
        internal bool Acknowledged; internal int? ErrorCode;
        internal string TransportStatus { get { return Status(true, Acknowledged); } }
        internal static string Status(bool attempted, bool acknowledged) { return !attempted ? "not_submitted" : acknowledged ? "sent_unconfirmed" : "unknown"; }
    }

    internal sealed class HiddenRoot : Form
    {
        protected override bool ShowWithoutActivation { get { return true; } }
        protected override CreateParams CreateParams
        {
            get { var value = base.CreateParams; value.ExStyle |= 0x08000000 | 0x00000080; return value; }
        }
    }
    internal sealed class ProbeHost : IDisposable
    {
        private readonly HiddenRoot root;
        private readonly TextBox forms;
        private readonly bool raw;
        private readonly Receiver observer;
        private bool undone;
        internal IntPtr Root { get { return root.Handle; } }
        internal IntPtr Target { get; private set; }
        internal string Failure { get { return observer.Failure; } }
        internal ProbeHost(CaseSpec spec, EventWaitHandle entered, EventWaitHandle release, EventWaitHandle completed)
        {
            root = new HiddenRoot(); root.ShowInTaskbar = false; root.FormBorderStyle = FormBorderStyle.None;
            IntPtr parent = root.Handle;
            raw = spec.Kind == "raw";
            if (raw)
            {
                uint style = 0x40000000U | (uint)(spec.Multiline ? Native.ES_MULTILINE : Native.ES_AUTOHSCROLL) |
                    (uint)(spec.ReadOnly ? Native.ES_READONLY : 0) | (uint)(spec.Password ? Native.ES_PASSWORD : 0);
                Target = Native.CreateWindowExW(0, "EDIT", spec.Before, style, 0, 0, 200, 100, parent, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                Program.Check(Target != IntPtr.Zero, "raw_edit_create_failed");
                Native.SendMessageW(Target, Native.EM_SETLIMITTEXT, new UIntPtr((uint)spec.Limit), IntPtr.Zero);
            }
            else
            {
                forms = new TextBox(); forms.Multiline = spec.Multiline; forms.ReadOnly = spec.ReadOnly;
                forms.MaxLength = spec.Limit; if (spec.Password) forms.PasswordChar = '*';
                forms.Text = spec.Before; root.Controls.Add(forms); Target = forms.Handle;
            }
            Native.SendMessageW(Target, Native.EM_SETSEL, new UIntPtr((uint)spec.Start), new IntPtr(spec.End));
            Native.SendMessageW(Target, Native.EM_EMPTYUNDOBUFFER, UIntPtr.Zero, IntPtr.Zero);
            Program.Check(!Native.IsWindowVisible(parent) && !Native.IsWindowVisible(Target), "child_window_visible");
            observer = new Receiver(this, spec.Scenario, entered, release, completed); observer.AssignHandle(Target);
        }
        internal void Queue(Action action) { root.BeginInvoke(action); }
        internal string OwnedText()
        {
            int length = Native.GetWindowTextLengthW(Target); Program.Check(length >= 0 && length <= 32767, "owned_text_length");
            var text = new StringBuilder(length + 1); int copied = Native.GetWindowTextW(Target, text, text.Capacity);
            Program.Check(copied == length, "owned_text_read"); return text.ToString();
        }
        internal object Snapshot()
        {
            string text = OwnedText(); ulong selection = unchecked((ulong)Native.SendMessageW(Target, Native.EM_GETSEL, UIntPtr.Zero, IntPtr.Zero).ToInt64());
            Program.Check(selection <= UInt32.MaxValue && selection != UInt32.MaxValue, "owned_selection_range");
            return Program.Obj("length", text.Length, "sha256_utf8", Program.Hash(text), "selection_start", (int)(selection & 65535),
                "selection_end", (int)((selection >> 16) & 65535), "can_undo", Native.SendMessageW(Target, Native.EM_CANUNDO, UIntPtr.Zero, IntPtr.Zero) != IntPtr.Zero,
                "mutation_entered", observer.Entered, "mutation_completed", observer.Completed, "before_length", observer.BeforeLength,
                "before_sha256_utf8", observer.BeforeHash, "after_length", observer.AfterLength, "after_sha256_utf8", observer.AfterHash,
                "reply_message_succeeded", observer.ReplySucceeded, "failure", observer.Failure);
        }
        internal void UndoOnce()
        {
            Program.Check(!undone && observer.Completed == 1, "undo_once_required"); undone = true;
            Program.Check(Native.SendMessageW(Target, Native.EM_UNDO, UIntPtr.Zero, IntPtr.Zero) != IntPtr.Zero, "undo_failed");
        }
        public void Dispose()
        {
            if (observer != null) observer.ReleaseHandle();
            if (forms != null) forms.Dispose();
            if (raw && Target != IntPtr.Zero) Native.DestroyWindow(Target);
            root.Dispose();
        }
    }
    internal sealed class Receiver : NativeWindow
    {
        private readonly ProbeHost host;
        private readonly string scenario;
        private readonly EventWaitHandle enteredGate, releaseGate, completedGate;
        internal int Entered, Completed, BeforeLength = -1, AfterLength = -1;
        internal string BeforeHash, AfterHash, Failure;
        internal bool ReplySucceeded;
        internal Receiver(ProbeHost host, string scenario, EventWaitHandle entered, EventWaitHandle release, EventWaitHandle completed)
        { this.host = host; this.scenario = scenario; enteredGate = entered; releaseGate = release; completedGate = completed; }
        protected override void WndProc(ref Message message)
        {
            if (message.Msg != Native.EM_REPLACESEL) { base.WndProc(ref message); return; }
            try
            {
                Entered++; Program.Check(Entered == 1, "duplicate_replace_message");
                // Read existing CONTROL text only. Never inspect/copy message.LParam before release.
                string before = host.OwnedText(); BeforeLength = before.Length; BeforeHash = Program.Hash(before);
                enteredGate.Set();
                if (scenario == "early_reply") ReplySucceeded = Native.ReplyMessage(IntPtr.Zero);
                if (scenario == "timeout" || scenario == "early_reply")
                    Program.Check(Native.WaitForSingleObject(releaseGate.SafeWaitHandle.DangerousGetHandle(), 4000) == 0, "receiver_gate_timeout");
                // Original lParam, consumed only NOW by the original system/WinForms Edit procedure.
                base.WndProc(ref message);
                string after = host.OwnedText(); AfterLength = after.Length; AfterHash = Program.Hash(after);
                Completed++; completedGate.Set();
            }
            catch (Exception error)
            {
                Failure = error.Message; message.Result = IntPtr.Zero; completedGate.Set();
            }
        }
    }

    internal static class Native
    {
        internal const uint WM_GETTEXTLENGTH = 0x000E, EM_GETSEL = 0x00B0, EM_SETSEL = 0x00B1,
            EM_REPLACESEL = 0x00C2, EM_SETLIMITTEXT = 0x00C5, EM_CANUNDO = 0x00C6, EM_UNDO = 0x00C7,
            EM_EMPTYUNDOBUFFER = 0x00CD, EM_GETPASSWORDCHAR = 0x00D2, EM_GETLIMITTEXT = 0x00D5;
        internal const long ES_MULTILINE = 0x0004, ES_UPPERCASE = 0x0008, ES_LOWERCASE = 0x0010, ES_AUTOHSCROLL = 0x0080,
            ES_PASSWORD = 0x0020, ES_OEMCONVERT = 0x0400, ES_READONLY = 0x0800, ES_NUMBER = 0x2000;
        internal const uint SendFlags = 0x0001 | 0x0002 | 0x0020; // BLOCK | ABORTIFHUNG | ERRORONEXIT
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
        internal static extern IntPtr SendMessageW(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr CreateWindowExW(uint extendedStyle, string className, string text, uint style,
            int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool DestroyWindow(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool IsWindow(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool IsWindowEnabled(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool IsWindowUnicode(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true)]
        internal static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
        [DllImport("user32.dll", ExactSpelling = true)]
        internal static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr GetWindowLongPtrW(IntPtr hwnd, int index);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
        internal static extern int GetClassNameW(IntPtr hwnd, StringBuilder text, int count);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
        internal static extern int GetWindowTextLengthW(IntPtr hwnd);
        [DllImport("user32.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
        internal static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int count);
        [DllImport("user32.dll", ExactSpelling = true)] [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool ReplyMessage(IntPtr result);
        [DllImport("kernel32.dll", ExactSpelling = true)]
        internal static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll", ExactSpelling = true)]
        internal static extern void SetLastError(uint error);
        [DllImport("kernel32.dll", ExactSpelling = true)]
        internal static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    }
}
