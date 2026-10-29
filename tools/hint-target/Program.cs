// 被动验收目标：只记录自己文本框收到的消息，不发键、不抢焦点、不读剪贴板。
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Security.Principal;
using System.Text;
using System.Windows.Forms;

sealed class KeyLedger {
    readonly HashSet<int> held = new HashSet<int>();
    public int[] Held { get { return held.OrderBy(k => k).ToArray(); } }
    public bool Record(int vk, bool down) {
        if (down) return held.Add(vk);
        return held.Remove(vk);
    }
    public void Clear() { held.Clear(); }
}

static class Evidence {
    public static string Hash(string text) {
        using (var sha = SHA256.Create())
            return BitConverter.ToString(sha.ComputeHash(Encoding.UTF8.GetBytes(text))).Replace("-", "").ToLowerInvariant();
    }
    public static string Utc() { return DateTime.UtcNow.ToString("o"); }
}

// 每类原生编辑消息分别保存本用例最后一次调用的内存摘要；不保存正文或在窗口过程写文件。
sealed class NativeEditRecord {
    public readonly int CallIndex;
    public int? BeforeLength, AfterLength;
    public string BeforeHash, AfterHash;
    public long? BeforeTimestampTicks, AfterTimestampTicks;
    public bool Completed;
    public NativeEditRecord(int callIndex) { CallIndex = callIndex; }
    public void CaptureBefore(string text) { BeforeLength = text.Length; BeforeHash = Evidence.Hash(text); }
    public void CaptureAfter(string text, long returnedAt) {
        AfterTimestampTicks = returnedAt; AfterLength = text.Length; AfterHash = Evidence.Hash(text);
        Completed = true;
    }
}

sealed class NativeEditLedger {
    public int Count { get; private set; }
    public NativeEditRecord Last { get; private set; }
    public NativeEditRecord Begin() { Last = new NativeEditRecord(++Count); return Last; }
    public void Clear() { Count = 0; Last = null; }
    public object Snapshot() {
        NativeEditRecord record = Last;
        object last = record == null ? null : new { call_index = record.CallIndex, completed = record.Completed,
            before_timestamp_ticks = record.BeforeTimestampTicks, after_timestamp_ticks = record.AfterTimestampTicks,
            before_text_utf16_length = record.BeforeLength, before_text_sha256_utf8 = record.BeforeHash,
            after_text_utf16_length = record.AfterLength, after_text_sha256_utf8 = record.AfterHash };
        return new { count = Count, timestamp_frequency_hz = Stopwatch.Frequency, last = last };
    }
}

sealed class NativeEditObservations {
    public readonly NativeEditLedger Paste = new NativeEditLedger();
    public readonly NativeEditLedger Replace = new NativeEditLedger();
    public void Clear() { Paste.Clear(); Replace.Clear(); }
}

sealed class TargetBox : TextBox {
    public Action<int, bool, bool> OnKeyMessage;
    readonly NativeEditObservations native = new NativeEditObservations();
    public object NativePaste { get { return native.Paste.Snapshot(); } }
    public object NativeReplace { get { return native.Replace.Snapshot(); } }
    public void ClearNativeObservations() { native.Clear(); }
    protected override void WndProc(ref Message message) {
        if (message.Msg == 0x302 || message.Msg == 0x00C2) { // WM_PASTE / EM_REPLACESEL
            // 只观察自己的 Text；不读取剪贴板或解引用消息中的文本指针。
            NativeEditLedger ledger = message.Msg == 0x302 ? native.Paste : native.Replace;
            NativeEditRecord record = null;
            try { record = ledger.Begin(); record.CaptureBefore(Text); }
            finally {
                // 即使前置观测异常，消息仍交给原生控件；这一分支只有这一处 base 调用。
                if (record != null) record.BeforeTimestampTicks = Stopwatch.GetTimestamp();
                base.WndProc(ref message);
            }
            long returnedAt = Stopwatch.GetTimestamp();
            if (record != null) record.CaptureAfter(Text, returnedAt);
            return;
        }
        bool down = message.Msg == 0x100 || message.Msg == 0x104;
        bool up = message.Msg == 0x101 || message.Msg == 0x105;
        if ((down || up) && OnKeyMessage != null)
            OnKeyMessage((int)message.WParam, down, (message.LParam.ToInt64() & (1L << 30)) != 0);
        base.WndProc(ref message);
    }
}

sealed class TargetForm : Form {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

    readonly string folder;
    readonly string run = Guid.NewGuid().ToString("N");
    readonly EvidenceOutput output;
    readonly KeyLedger ledger = new KeyLedger();
    readonly TargetBox box = new TargetBox();
    readonly Label status = new Label();
    readonly Timer timer = new Timer();
    readonly Stopwatch elapsed = Stopwatch.StartNew();
    readonly bool elevated = new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator);
    int caseNumber;
    int events;
    long formHandle;
    long editHandle;
    bool closing;
    const int EventLimit = 4096;
    public bool Failed { get { return output.Failed; } }

    public TargetForm(string output) {
        folder = output;
        this.output = new EvidenceOutput(folder, run, () => caseNumber, () => elapsed.ElapsedMilliseconds);
        Text = "拈来验收目标 · " + (elevated ? "管理员" : "普通权限");
        Width = 780; Height = 480;
        StartPosition = FormStartPosition.CenterScreen;
        var header = new FlowLayoutPanel { Dock = DockStyle.Top, Height = 48, Padding = new Padding(6) };
        var begin = new Button { Text = "开始新用例", AutoSize = true };
        begin.Click += delegate { if (BeginCase()) box.Focus(); };
        status.AutoSize = true;
        header.Controls.Add(begin); header.Controls.Add(status);
        box.Multiline = true; box.AcceptsReturn = true; box.AcceptsTab = true;
        box.MaxLength = 32768; box.Dock = DockStyle.Fill;
        box.Font = new Font("Consolas", 13);
        box.OnKeyMessage = Key;
        Controls.Add(box); Controls.Add(header);
        Shown += delegate {
            formHandle = Handle.ToInt64(); editHandle = box.Handle.ToInt64();
            if (!BeginCase()) return;
            box.Focus();
            if (!Write("ready.json", Snapshot("ready"))) return;
            timer.Start();
        };
        FormClosed += delegate {
            timer.Stop(); closing = true;
            if (!Failed) Write("state.json", Snapshot("closed"));
        };
        timer.Interval = 200;
        timer.Tick += delegate {
            if (Failed) return;
            if (elapsed.Elapsed.TotalMinutes >= 30 || File.Exists(Path.Combine(folder, "STOP"))) { Close(); return; }
            Write("state.json", Snapshot("running"));
        };
    }

    bool BeginCase() {
        if (Failed) return false;
        caseNumber++; ledger.Clear(); box.Clear(); box.ClearNativeObservations();
        if (!Append(new { kind = "case_started", run_id = run, case_id = caseNumber, utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds })) return false;
        status.Text = "用例 " + caseNumber + " · " + (elevated ? "管理员" : "普通权限") + " · 仅记录本窗口";
        return Write("state.json", Snapshot("running"));
    }

    void Key(int vk, bool down, bool previousDown) {
        if (Failed) return;
        bool changed = ledger.Record(vk, down);
        if (events < EventLimit) {
            if (!Append(new { kind = down ? "down" : "up", run_id = run, case_id = caseNumber,
                utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds, vk = vk,
                previous_down = previousDown, repeat = down && previousDown,
                matching_down = down ? (bool?)null : changed })) return;
        } else if (events == EventLimit) {
            if (!Append(new { kind = "truncated", run_id = run, utc = Evidence.Utc(), limit = EventLimit })) return;
        }
        events++;
    }

    object Snapshot(string phase) {
        uint pid; IntPtr foreground = GetForegroundWindow();
        GetWindowThreadProcessId(foreground, out pid);
        string text = box.Text;
        long textObservedAt = Stopwatch.GetTimestamp();
        return new { schema = 1, run_id = run, case_id = caseNumber, phase = phase,
            utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds,
            pid = Process.GetCurrentProcess().Id, hwnd = formHandle, edit = editHandle,
            elevated = elevated, foreground_is_target = foreground.ToInt64() == formHandle, foreground_pid = pid,
            text_utf16_length = text.Length, text_sha256_utf8 = Evidence.Hash(text),
            text_observed_timestamp_ticks = textObservedAt, native_paste = box.NativePaste, native_replace = box.NativeReplace,
            observed_held = ledger.Held, event_count = events, truncated = events > EventLimit, closed = closing };
    }

    bool Append(object value) {
        bool recorded = output.Append(value);
        if (!recorded) StopAfterFailure();
        return recorded;
    }

    bool Write(string name, object value) {
        bool recorded = output.Write(name, value);
        if (!recorded) StopAfterFailure();
        return recorded;
    }

    void StopAfterFailure() {
        timer.Stop();
        // 从按键消息中延后关闭，避免在 TextBox.WndProc 尚未返回时销毁窗口。
        if (closing || IsDisposed) return;
        closing = true;
        try { BeginInvoke(new MethodInvoker(Close)); }
        catch (InvalidOperationException) { Application.ExitThread(); }
    }
}

static class Program {
    static void Check(bool condition, string name) { if (!condition) throw new Exception("自测失败：" + name); }

    static int SelfTest() {
        // WinExe 没有控制台，不能设置控制台代码页；只给构建脚本提供的标准输出管道编码。
        Console.SetOut(new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true });
        var ledger = new KeyLedger();
        Check(ledger.Record(86, true) && !ledger.Record(86, true), "按住时重复按下");
        Check(ledger.Record(88, true) && ledger.Held.SequenceEqual(new[] { 86, 88 }), "重叠按键");
        Check(ledger.Record(86, false) && ledger.Held.SequenceEqual(new[] { 88 }), "只松一个键");
        Check(!ledger.Record(13, false), "识别没有对应按下的抬起");
        Check(ledger.Record(88, false) && ledger.Held.Length == 0, "最后一个松开");
        ledger.Record(17, true); ledger.Clear();
        Check(ledger.Held.Length == 0 && !ledger.Record(17, false), "用例边界清空记录");
        Check(Evidence.Hash("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "UTF-8 校验值");
        var paste = new NativeEditLedger();
        Check(paste.Count == 0 && paste.Last == null && Stopwatch.Frequency > 0, "尚无原生粘贴观测");
        var observed = paste.Begin();
        const string before = "虚构的调用前文本", after = "虚构的调用后文本 · 9471";
        observed.CaptureBefore(before); observed.BeforeTimestampTicks = Stopwatch.GetTimestamp();
        observed.CaptureAfter(after, Stopwatch.GetTimestamp());
        string serialized = new System.Web.Script.Serialization.JavaScriptSerializer().Serialize(paste.Snapshot());
        Check(observed.Completed && observed.BeforeLength == before.Length && observed.AfterLength == after.Length &&
            observed.BeforeHash == Evidence.Hash(before) && observed.AfterHash == Evidence.Hash(after) &&
            observed.AfterTimestampTicks >= observed.BeforeTimestampTicks &&
            !serialized.Contains(before) && !serialized.Contains(after), "原生前后摘要/时序/不保存正文");
        Check(observed.AfterLength > 0 && observed.AfterHash != Evidence.Hash(String.Empty) &&
            paste.Begin().CallIndex == 2 && paste.Count == 2 && !paste.Last.Completed && paste.Last.AfterLength == null,
            "后续当前空文本不改历史 after；新调用不复用旧 after");
        paste.Clear(); observed.CaptureAfter(after, Stopwatch.GetTimestamp());
        Check(paste.Count == 0 && paste.Last == null, "用例重置后旧观测不能复活");
        var native = new NativeEditObservations();
        var pasteRecord = native.Paste.Begin();
        var replaceRecord = native.Replace.Begin();
        const string pasteBefore = "虚构 WM_PASTE 前", pasteAfter = "虚构 WM_PASTE 后 · 5821";
        const string replaceBefore = "虚构 EM_REPLACESEL 前 · 文字", replaceAfter = "虚构 EM_REPLACESEL 后 · 中文😀";
        pasteRecord.CaptureBefore(pasteBefore); pasteRecord.BeforeTimestampTicks = Stopwatch.GetTimestamp();
        pasteRecord.CaptureAfter(pasteAfter, Stopwatch.GetTimestamp());
        replaceRecord.CaptureBefore(replaceBefore); replaceRecord.BeforeTimestampTicks = Stopwatch.GetTimestamp();
        replaceRecord.CaptureAfter(replaceAfter, Stopwatch.GetTimestamp());
        Check(native.Paste.Count == 1 && native.Replace.Count == 1 &&
            native.Paste.Last == pasteRecord && native.Replace.Last == replaceRecord &&
            pasteRecord.CallIndex == 1 && replaceRecord.CallIndex == 1 && pasteRecord.Completed && replaceRecord.Completed &&
            pasteRecord.BeforeLength == pasteBefore.Length && pasteRecord.AfterLength == pasteAfter.Length &&
            replaceRecord.BeforeLength == replaceBefore.Length && replaceRecord.AfterLength == replaceAfter.Length &&
            pasteRecord.BeforeHash == Evidence.Hash(pasteBefore) && pasteRecord.AfterHash == Evidence.Hash(pasteAfter) &&
            replaceRecord.BeforeHash == Evidence.Hash(replaceBefore) && replaceRecord.AfterHash == Evidence.Hash(replaceAfter) &&
            pasteRecord.AfterTimestampTicks >= pasteRecord.BeforeTimestampTicks &&
            replaceRecord.AfterTimestampTicks >= replaceRecord.BeforeTimestampTicks, "两类原生消息计数/摘要/时序分别保存");
        string separated = new System.Web.Script.Serialization.JavaScriptSerializer().Serialize(
            new { native_paste = native.Paste.Snapshot(), native_replace = native.Replace.Snapshot() });
        Check(native.Replace.Begin().CallIndex == 2 && native.Replace.Count == 2 &&
            !native.Replace.Last.Completed && native.Replace.Last.AfterLength == null &&
            native.Paste.Count == 1 && native.Paste.Last == pasteRecord && native.Paste.Last.AfterHash == Evidence.Hash(pasteAfter) &&
            separated.Contains(pasteRecord.AfterHash) && separated.Contains(replaceRecord.AfterHash) &&
            !separated.Contains(pasteBefore) && !separated.Contains(pasteAfter) &&
            !separated.Contains(replaceBefore) && !separated.Contains(replaceAfter), "替换消息不覆盖 WM_PASTE 计数或摘要且不保存正文");
        native.Clear();
        pasteRecord.CaptureAfter(pasteAfter, Stopwatch.GetTimestamp());
        replaceRecord.CaptureAfter(replaceAfter, Stopwatch.GetTimestamp());
        Check(native.Paste.Count == 0 && native.Paste.Last == null && native.Replace.Count == 0 && native.Replace.Last == null,
            "用例边界同时清空两类消息且旧观测不能复活");
        int fileChecks = FileIoTests.Run();
        Console.WriteLine("7 项记账/校验、7 项原生编辑消息观测内存自测及 " + fileChecks + " 项文件自测通过；没有创建窗口、注入输入或访问剪贴板。");
        return 0;
    }

    [STAThread] static int Main(string[] args) {
        try {
            Console.SetError(new StreamWriter(Console.OpenStandardError(), new UTF8Encoding(false)) { AutoFlush = true });
            if (args.Length == 1 && args[0] == "--self-test") return SelfTest();
            if (args.Length != 2 || args[0] != "--output") throw new Exception("用法：HintTarget.exe --output 全新目录，或 --self-test");
            string folder = Path.GetFullPath(args[1]);
            // 全新目录和独立 run_id，避免旧 STOP、退出请求或旧响应污染下一轮。
            if (Directory.Exists(folder) || File.Exists(folder)) throw new Exception("输出目录已存在；请使用全新路径");
            Directory.CreateDirectory(folder);
            Application.EnableVisualStyles(); Application.SetCompatibleTextRenderingDefault(false);
            // 写入故障由记录层封闭并主动退出；其他未处理回调异常交给 Main，避免 .NET 模态框。
            Application.SetUnhandledExceptionMode(UnhandledExceptionMode.ThrowException);
            using (var form = new TargetForm(folder)) {
                Application.Run(form);
                return form.Failed ? 1 : 0;
            }
        } catch (Exception error) {
            Console.Error.WriteLine(error.Message);
            return 1;
        }
    }
}
