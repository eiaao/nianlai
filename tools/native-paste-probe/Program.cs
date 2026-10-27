// 诊断专用被动接收端：默认严格私有；当前站分支必须显式 opt-in，不能自动降级。
using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Web.Script.Serialization;
using System.Windows.Forms;

sealed class Options {
    public string Station, Desktop, Output, OptIn;
    public string Scope = "private";
    public int Timeout;
    public static Options Parse(string[] args) {
        if (args.Length < 8 || args.Length > 12 || args.Length % 2 != 0) throw new ArgumentException("需要四组基础参数；授权分支另加 --scope / --opt-in");
        var result = new Options();
        bool timeoutSeen = false;
        bool scopeSeen = false;
        for (int i = 0; i < args.Length; i += 2) {
            if (args[i] == "--expected-station" && result.Station == null) result.Station = args[i + 1];
            else if (args[i] == "--expected-desktop" && result.Desktop == null) result.Desktop = args[i + 1];
            else if (args[i] == "--output" && result.Output == null) result.Output = args[i + 1];
            else if (args[i] == "--scope" && !scopeSeen) { scopeSeen = true; result.Scope = args[i + 1]; }
            else if (args[i] == "--opt-in" && result.OptIn == null) result.OptIn = args[i + 1];
            else if (args[i] == "--timeout-ms" && !timeoutSeen) {
                timeoutSeen = true;
                if (!Int32.TryParse(args[i + 1], out result.Timeout) || result.Timeout < 1000 || result.Timeout > 30000)
                    throw new ArgumentException("timeout-ms 必须在 1000..30000，通常为 15000");
            } else throw new ArgumentException("未知或重复参数");
        }
        if (result.Station == null || result.Desktop == null || result.Output == null || !timeoutSeen)
            throw new ArgumentException("参数不完整");
        if (!Path.IsPathRooted(result.Output) || result.Output.StartsWith(@"\\", StringComparison.Ordinal))
            throw new ArgumentException("输出必须是本地绝对路径");
        result.Output = Path.GetFullPath(result.Output);
        return result;
    }
}

sealed class Metric {
    public readonly int Length;
    public readonly string Hash;
    public Metric(string text) {
        Length = text.Length;
        using (var sha = SHA256.Create())
            Hash = BitConverter.ToString(sha.ComputeHash(Encoding.UTF8.GetBytes(text))).Replace("-", "").ToLowerInvariant();
    }
}

sealed class PasteObservation {
    public int Calls;
    public Metric Before, After;
    public string BeforeUtc, AfterUtc;
}

sealed class PasteBox : TextBox {
    public Action CheckContext;
    public Action<PasteObservation> Observed;
    public Action<Exception> Failed;
    public int PasteCalls { get; private set; }
    bool stopped;
    public void StopAccepting() { stopped = true; }
    protected override void WndProc(ref Message message) {
        if (message.Msg != 0x302) { base.WndProc(ref message); return; } // WM_PASTE
        if (stopped) { message.Result = IntPtr.Zero; return; }
        try {
            if (CheckContext != null) CheckContext();
            PasteCalls++;
            var observation = new PasteObservation { Calls = PasteCalls, BeforeUtc = DateTime.UtcNow.ToString("o"), Before = new Metric(Text) };
            // 原生 WinForms Edit 负责实际粘贴；本观察器不调用任何 Clipboard API。
            base.WndProc(ref message);
            observation.After = new Metric(Text); observation.AfterUtc = DateTime.UtcNow.ToString("o");
            if (Observed != null) Observed(observation);
        } catch (Exception error) { stopped = true; if (Failed != null) Failed(error); }
    }
}

sealed class PassiveForm : Form {
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get { var value = base.CreateParams; value.ExStyle |= 0x08000000 | 0x80; return value; }
    }
}

sealed class ProbeOutput : IDisposable {
    readonly string folder;
    readonly ProbeContext context;
    readonly FileStream claim;
    readonly JavaScriptSerializer json = new JavaScriptSerializer();
    public bool Failed { get; private set; }
    public ProbeOutput(string folder, ProbeContext context) {
        this.folder = folder; this.context = context;
        if (Directory.Exists(folder) || File.Exists(folder)) throw new IOException("输出目录已存在，禁止复用");
        Directory.CreateDirectory(folder);
        claim = new FileStream(Path.Combine(folder, ".owner"), FileMode.CreateNew, FileAccess.Write, FileShare.None);
    }
    public void Write(string name, object value) {
        string path = Path.Combine(folder, name), temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try {
            File.WriteAllText(temp, json.Serialize(value), new UTF8Encoding(false));
            if (File.Exists(path)) File.Replace(temp, path, null);
            else File.Move(temp, path);
        } finally {
            try { File.Delete(temp); } catch (IOException) { } catch (UnauthorizedAccessException) { }
        }
    }
    public void Append(object value) {
        File.AppendAllText(Path.Combine(folder, "paste-events.jsonl"), json.Serialize(value) + Environment.NewLine, new UTF8Encoding(false));
    }
    public void Fail(Exception error) {
        if (Failed) return;
        Failed = true;
        try {
            using (var stream = new FileStream(Path.Combine(folder, "failure.json"), FileMode.CreateNew, FileAccess.Write, FileShare.Read))
            using (var writer = new StreamWriter(stream, new UTF8Encoding(false)))
                writer.Write(json.Serialize(new { schema = 1, phase = "failed", pid = Process.GetCurrentProcess().Id,
                    scope = context.Scope, run_id = context.RunId, station = context.Station, desktop = context.Desktop,
                    private_context_verified = context.IsPrivate,
                    utc = DateTime.UtcNow.ToString("o"), error_type = error.GetType().FullName,
                    hresult = "0x" + error.HResult.ToString("x8"), observer_clipboard_api_calls = 0 }));
        } catch (Exception) { }
        Program.Report(error);
    }
    public void Dispose() { claim.Dispose(); }
}

sealed class ProbeHost : IDisposable {
    readonly Options options;
    readonly ProbeContext context;
    readonly ProbeOutput output;
    readonly ApplicationContext loop = new ApplicationContext();
    readonly PassiveForm form = new PassiveForm();
    readonly PasteBox box = new PasteBox();
    readonly Timer timer = new Timer();
    readonly Stopwatch elapsed = Stopwatch.StartNew();
    readonly Metric empty = new Metric(String.Empty);
    PasteObservation last;
    bool ready;
    public ProbeHost(Options options, ProbeContext context, ProbeOutput output) {
        this.options = options; this.context = context; this.output = output;
        form.Text = "NativePasteProbe · 隐藏接收端"; form.Width = 780; form.Height = 480;
        box.Multiline = true; box.AcceptsReturn = true; box.AcceptsTab = true; box.MaxLength = 32768;
        box.ReadOnly = false; box.Dock = DockStyle.Fill; box.Font = new Font("Consolas", 13);
        box.CheckContext = context.AssertCurrent;
        box.Observed = delegate(PasteObservation observation) {
            last = observation;
            object state = State("paste_observed");
            output.Append(state); output.Write("state.json", state);
        };
        box.Failed = Fail;
        form.Controls.Add(box);
        // 只创建隐藏的真实 Form/Edit HWND；不 Show，不设置 ActiveControl、Focus 或前台。
        IntPtr parent = form.Handle, edit = box.Handle;
        timer.Interval = 50;
        timer.Tick += delegate {
            try {
                context.AssertCurrent();
                // 首个真实消息循环回调才发布 ready，不把构造完成误当成循环已开始。
                if (!ready) { PublishReady(parent, edit); ready = true; }
                bool stop = File.Exists(Path.Combine(options.Output, "STOP"));
                if (!stop && elapsed.ElapsedMilliseconds < options.Timeout) return;
                timer.Stop();
                if (!output.Failed) output.Write("state.json", State(stop ? "stopped" : "timeout", true));
                loop.ExitThread();
            } catch (Exception error) { Fail(error); }
        };
    }
    void PublishReady(IntPtr parent, IntPtr edit) {
        output.Write("state.json", State("ready"));
        output.Write("ready.json", new { schema = 1, run_id = context.RunId, pid = Process.GetCurrentProcess().Id,
            thread = context.Thread, station = context.Station, desktop = context.Desktop, scope = context.Scope,
            private_context_verified = context.IsPrivate, desktop_receives_input = context.LastReceivesInput,
            context_verified_utc = context.LastVerifiedUtc,
            message_loop_running = true, hidden_controls = true,
            hwnd = parent.ToInt64(), edit = edit.ToInt64(), read_only = box.ReadOnly, multiline = box.Multiline,
            accepts_return = box.AcceptsReturn, accepts_tab = box.AcceptsTab, max_length = box.MaxLength,
            observer_clipboard_api_calls = 0, focus = context.Focus(edit), utc = DateTime.UtcNow.ToString("o") });
    }
    object State(string phase, bool finalCurrent = false) {
        Metric before = last == null ? empty : last.Before, after = last == null ? empty : last.After;
        // 正常结束由 UI 线程独立读取自有控件；watchdog 绝不跨线程读控件或等待日志。
        Metric current = finalCurrent ? new Metric(box.Text) : after;
        return new { schema = 1, run_id = context.RunId, phase = phase, pid = Process.GetCurrentProcess().Id,
            thread = context.Thread, station = context.Station, desktop = context.Desktop, scope = context.Scope,
            private_context_verified = context.IsPrivate, desktop_receives_input = context.LastReceivesInput,
            context_verified_utc = context.LastVerifiedUtc, hidden_controls = true,
            hwnd = form.Handle.ToInt64(), edit = box.Handle.ToInt64(), paste_calls = box.PasteCalls,
            length_utf16 = current.Length, sha256_utf8 = current.Hash,
            before_length_utf16 = before.Length, before_sha256_utf8 = before.Hash,
            after_length_utf16 = after.Length, after_sha256_utf8 = after.Hash,
            final_current_length_utf16 = finalCurrent ? (int?)current.Length : null,
            final_current_sha256_utf8 = finalCurrent ? current.Hash : null,
            digest_origin = finalCurrent ? "ui_final_current_text" : (last == null ? "known_initial_empty" : "wm_paste_after"),
            before_utc = last == null ? null : last.BeforeUtc, after_utc = last == null ? null : last.AfterUtc,
            wm_paste_completed = last != null, observer_clipboard_api_calls = 0,
            focus = context.Focus(box.Handle), elapsed_ms = elapsed.ElapsedMilliseconds, utc = DateTime.UtcNow.ToString("o") };
    }
    void Fail(Exception error) { timer.Stop(); box.StopAccepting(); output.Fail(error); loop.ExitThread(); }
    public void Run() { timer.Start(); Application.Run(loop); }
    public void Dispose() { timer.Dispose(); form.Dispose(); loop.Dispose(); }
}

// 最外层自身看门狗不依赖 UI 消息循环、控件或日志 I/O，强制超时与正常 STOP 分开编码。
sealed class ExitWatchdog : IDisposable {
    public const int HardExitCode = 124;
    readonly System.Threading.Timer timer;
    int completed;
    public readonly string ArmedUtc = DateTime.UtcNow.ToString("o");
    public ExitWatchdog(int milliseconds) {
        timer = new System.Threading.Timer(delegate {
            if (System.Threading.Interlocked.CompareExchange(ref completed, 1, 0) == 0) Environment.Exit(HardExitCode);
        }, null, milliseconds, System.Threading.Timeout.Infinite);
    }
    public void Dispose() { System.Threading.Interlocked.Exchange(ref completed, 1); timer.Dispose(); }
}

static class Program {
    internal static void Report(Exception error) {
        try { Console.Error.WriteLine(error.GetType().Name + ": 0x" + error.HResult.ToString("x8") + " " + error.Message); }
        catch (IOException) { } catch (ObjectDisposedException) { }
    }
    [STAThread] static int Main(string[] args) {
        ProbeOutput output = null;
        try {
            var options = Options.Parse(args);
            using (var watchdog = new ExitWatchdog(options.Timeout + 1000)) {
                // 在 Form、控件、消息循环或任何剪贴板行为之前，严格选择唯一的显式 scope。
                var context = ProbeContext.Require(options.Scope, options.OptIn, options.Station, options.Desktop);
                Application.SetUnhandledExceptionMode(UnhandledExceptionMode.ThrowException);
                Application.EnableVisualStyles(); Application.SetCompatibleTextRenderingDefault(false);
                output = new ProbeOutput(options.Output, context);
                using (output) {
                    output.Write("watchdog.json", new { schema = 1, scope = context.Scope, pid = Process.GetCurrentProcess().Id,
                        armed_utc = watchdog.ArmedUtc, ui_timeout_ms = options.Timeout,
                        hard_timeout_ms = options.Timeout + 1000, hard_timeout_exit_code = ExitWatchdog.HardExitCode });
                    using (var host = new ProbeHost(options, context, output)) { host.Run(); return output.Failed ? 1 : 0; }
                }
            }
        } catch (Exception error) {
            if (output != null) output.Fail(error); else Report(error);
            return 1;
        }
    }
}
