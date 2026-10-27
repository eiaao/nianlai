using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;

// 隔离检查只能读取当前进程/线程的对象信息，绝不切换桌面或访问输入桌面。
sealed class ProbeContext {
    internal const string InteractiveOptIn = "M0_CURRENT_STATION_TEST_CLIPBOARD_V1";
    public readonly string Station;
    public readonly string Desktop;
    public readonly string RunId;
    public readonly uint Thread;
    public readonly string Scope;
    public bool LastReceivesInput { get; private set; }
    public string LastVerifiedUtc { get; private set; }
    public bool IsPrivate { get { return Scope == "private"; } }
    ProbeContext(string station, string desktop, string runId, uint thread, string scope) {
        Station = station; Desktop = desktop; RunId = runId; Thread = thread; Scope = scope;
    }

    [DllImport("kernel32.dll")] internal static extern uint GetCurrentThreadId();
    [DllImport("user32.dll", SetLastError = true)] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll", SetLastError = true)] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll", EntryPoint = "GetUserObjectInformationW", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool UserObjectName(IntPtr handle, int index, StringBuilder value, uint bytes, out uint needed);
    [DllImport("user32.dll", EntryPoint = "GetUserObjectInformationW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool UserObjectInput(IntPtr handle, int index, out int value, uint bytes, out uint needed);

    static string Name(IntPtr handle) {
        if (handle == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "无法取得隔离对象句柄");
        uint needed;
        bool unexpected = UserObjectName(handle, 2, null, 0, out needed); // UOI_NAME，长度单位为字节。
        if (unexpected || Marshal.GetLastWin32Error() != 122 || needed < 2 || needed > 4096 || needed % 2 != 0)
            throw new InvalidOperationException("UOI_NAME 长度查询结果未知，拒绝继续");
        var value = new StringBuilder((int)(needed / 2));
        if (!UserObjectName(handle, 2, value, needed, out needed))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "无法核对隔离对象名称");
        if (value.Length == 0) throw new InvalidOperationException("隔离对象名称为空");
        return value.ToString();
    }

    static bool ReceivesInput(IntPtr desktop) {
        int receivesInput;
        uint needed;
        if (!UserObjectInput(desktop, 6, out receivesInput, 4, out needed)) // UOI_IO
            throw new Win32Exception(Marshal.GetLastWin32Error(), "无法核对输入桌面状态，拒绝继续");
        if (needed != 4 || (receivesInput != 0 && receivesInput != 1))
            throw new InvalidOperationException("UOI_IO 结果未知");
        return receivesInput == 1;
    }
    static void RequireNoProduct() {
        Process[] processes = Process.GetProcessesByName("nianlai");
        try { if (processes.Length != 0) throw new InvalidOperationException("nianlai 正在运行，授权诊断拒绝开始或继续"); }
        finally { foreach (Process process in processes) process.Dispose(); }
    }
    public static ProbeContext Require(string scope, string optIn, string expectedStation, string expectedDesktop) {
        string runId;
        if (scope == "private") {
            if (optIn != null) throw new InvalidOperationException("private 模式不接受交互 opt-in");
            Match station = Regex.Match(expectedStation, "^NianlaiNativeProbe-([0-9a-f]{32})$", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);
            Match desktop = Regex.Match(expectedDesktop, "^NianlaiNativeProbe-Desk-([0-9a-f]{32})$", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);
            if (!station.Success || !desktop.Success ||
                !String.Equals(station.Groups[1].Value, desktop.Groups[1].Value, StringComparison.OrdinalIgnoreCase) ||
                station.Groups[1].Value == new string('0', 32))
                throw new InvalidOperationException("必须提供同一次运行的唯一私有 station/desktop 名称");
            runId = station.Groups[1].Value.ToLowerInvariant();
        } else if (scope == "authorized-interactive") {
            // 这是本轮人工许可后由父端设置的显式开关，不是私有创建失败后的 fallback。
            if (optIn != InteractiveOptIn || !String.Equals(expectedStation, "WinSta0", StringComparison.OrdinalIgnoreCase) ||
                String.IsNullOrWhiteSpace(expectedDesktop)) throw new InvalidOperationException("缺少本轮授权诊断的显式 opt-in/对象参数");
            RequireNoProduct();
            runId = Guid.NewGuid().ToString("N");
        } else throw new InvalidOperationException("未知 scope，拒绝继续");
        uint thread = GetCurrentThreadId();
        IntPtr stationHandle = GetProcessWindowStation(), desktopHandle = GetThreadDesktop(thread);
        string actualStation = Name(stationHandle), actualDesktop = Name(desktopHandle);
        if (!String.Equals(actualStation, expectedStation, StringComparison.OrdinalIgnoreCase) ||
            !String.Equals(actualDesktop, expectedDesktop, StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("实际 station/desktop 不匹配；禁止 fallback");
        var context = new ProbeContext(actualStation, actualDesktop, runId, thread, scope);
        context.AssertCurrent();
        return context;
    }
    public void AssertCurrent() {
        if (GetCurrentThreadId() != Thread) throw new InvalidOperationException("观察线程已改变");
        string station = Name(GetProcessWindowStation());
        IntPtr desktopHandle = GetThreadDesktop(Thread);
        string desktop = Name(desktopHandle);
        if (!String.Equals(station, Station, StringComparison.OrdinalIgnoreCase) ||
            !String.Equals(desktop, Desktop, StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("运行中的 station/desktop 身份已改变");
        bool receivesInput = ReceivesInput(desktopHandle);
        bool winSta0 = String.Equals(station, "WinSta0", StringComparison.OrdinalIgnoreCase);
        if (IsPrivate) {
            if (winSta0 || receivesInput) throw new InvalidOperationException("private 模式明确拒绝 WinSta0 或输入桌面");
        } else {
            if (!winSta0 || !receivesInput) throw new InvalidOperationException("授权诊断不在当前输入桌面，停止");
            RequireNoProduct();
        }
        LastReceivesInput = receivesInput; LastVerifiedUtc = DateTime.UtcNow.ToString("o");
    }

    [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct GuiThreadInfo {
        public uint Size, Flags;
        public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public Rect CaretRect;
    }
    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)] static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);

    public object Focus(IntPtr edit) {
        var info = new GuiThreadInfo(); info.Size = (uint)Marshal.SizeOf(typeof(GuiThreadInfo));
        bool ok = GetGUIThreadInfo(Thread, ref info);
        return new { focus_query_ok = ok, focus_hwnd = ok ? info.Focus.ToInt64() : 0L,
            focus_is_edit = ok && info.Focus == edit, focus_error = ok ? 0 : Marshal.GetLastWin32Error() };
    }
}
