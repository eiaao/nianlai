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
using System.Web.Script.Serialization;
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

sealed class TargetBox : TextBox {
    public Action<int, bool, bool> OnKeyMessage;
    protected override void WndProc(ref Message message) {
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
    readonly JavaScriptSerializer json = new JavaScriptSerializer();
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

    public TargetForm(string output) {
        folder = output;
        Text = "拈来验收目标 · " + (elevated ? "管理员" : "普通权限");
        Width = 780; Height = 480;
        StartPosition = FormStartPosition.CenterScreen;
        var header = new FlowLayoutPanel { Dock = DockStyle.Top, Height = 48, Padding = new Padding(6) };
        var begin = new Button { Text = "开始新用例", AutoSize = true };
        begin.Click += delegate { BeginCase(); box.Focus(); };
        status.AutoSize = true;
        header.Controls.Add(begin); header.Controls.Add(status);
        box.Multiline = true; box.AcceptsReturn = true; box.AcceptsTab = true;
        box.MaxLength = 32768; box.Dock = DockStyle.Fill;
        box.Font = new Font("Consolas", 13);
        box.OnKeyMessage = Key;
        Controls.Add(box); Controls.Add(header);
        Shown += delegate {
            formHandle = Handle.ToInt64(); editHandle = box.Handle.ToInt64();
            BeginCase(); box.Focus(); Write("ready.json", Snapshot("ready"));
            timer.Start();
        };
        FormClosed += delegate {
            timer.Stop(); closing = true;
            Write("state.json", Snapshot("closed"));
        };
        timer.Interval = 200;
        timer.Tick += delegate {
            if (elapsed.Elapsed.TotalMinutes >= 30 || File.Exists(Path.Combine(folder, "STOP"))) { Close(); return; }
            Write("state.json", Snapshot("running"));
        };
    }

    void BeginCase() {
        caseNumber++; ledger.Clear(); box.Clear();
        Append(new { kind = "case_started", run_id = run, case_id = caseNumber, utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds });
        status.Text = "用例 " + caseNumber + " · " + (elevated ? "管理员" : "普通权限") + " · 仅记录本窗口";
        Write("state.json", Snapshot("running"));
    }

    void Key(int vk, bool down, bool previousDown) {
        bool changed = ledger.Record(vk, down);
        if (events < EventLimit) {
            Append(new { kind = down ? "down" : "up", run_id = run, case_id = caseNumber,
                utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds, vk = vk,
                previous_down = previousDown, repeat = down && previousDown,
                matching_down = down ? (bool?)null : changed });
        } else if (events == EventLimit) {
            Append(new { kind = "truncated", run_id = run, utc = Evidence.Utc(), limit = EventLimit });
        }
        events++;
    }

    object Snapshot(string phase) {
        uint pid; IntPtr foreground = GetForegroundWindow();
        GetWindowThreadProcessId(foreground, out pid);
        return new { schema = 1, run_id = run, case_id = caseNumber, phase = phase,
            utc = Evidence.Utc(), elapsed_ms = elapsed.ElapsedMilliseconds,
            pid = Process.GetCurrentProcess().Id, hwnd = formHandle, edit = editHandle,
            elevated = elevated, foreground_is_target = foreground.ToInt64() == formHandle, foreground_pid = pid,
            text_utf16_length = box.Text.Length, text_sha256_utf8 = Evidence.Hash(box.Text),
            observed_held = ledger.Held, event_count = events, truncated = events > EventLimit, closed = closing };
    }

    void Append(object value) {
        File.AppendAllText(Path.Combine(folder, "events.jsonl"), json.Serialize(value) + Environment.NewLine, new UTF8Encoding(false));
    }

    void Write(string name, object value) {
        string path = Path.Combine(folder, name), temp = path + ".tmp";
        File.WriteAllText(temp, json.Serialize(value), new UTF8Encoding(false));
        if (File.Exists(path)) File.Replace(temp, path, null);
        else File.Move(temp, path);
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
        Console.WriteLine("7 项自测通过；没有创建窗口、注入输入或访问剪贴板。");
        return 0;
    }

    [STAThread] static int Main(string[] args) {
        try {
            if (args.Length == 1 && args[0] == "--self-test") return SelfTest();
            if (args.Length != 2 || args[0] != "--output") throw new Exception("用法：HintTarget.exe --output 全新目录，或 --self-test");
            string folder = Path.GetFullPath(args[1]);
            // 全新目录和独立 run_id，避免旧 STOP、退出请求或旧响应污染下一轮。
            if (Directory.Exists(folder) || File.Exists(folder)) throw new Exception("输出目录已存在；请使用全新路径");
            Directory.CreateDirectory(folder);
            Application.EnableVisualStyles(); Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new TargetForm(folder));
            return 0;
        } catch (Exception error) {
            Console.Error.WriteLine(error.Message);
            return 1;
        }
    }
}
