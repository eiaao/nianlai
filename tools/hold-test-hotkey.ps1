# 仅供独立测试机 / 无日常样机的 Windows 登录会话手测使用；不会启动拈来或发送按键。
# 前台运行，按 Enter 释放；不创建后台服务，不终止任何进程。
param(
    [ValidateRange(1, 15)][uint32]$Modifiers = 6, # MOD_CONTROL | MOD_SHIFT
    [ValidateRange(1, 254)][uint32]$VirtualKey = 0x7A # F11
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;

public static class NianlaiTestHotkeyHolder {
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(IntPtr window, int id, uint modifiers, uint key);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool UnregisterHotKey(IntPtr window, int id);

    public static void Hold(uint modifiers, uint key) {
        const int id = 0x4E48;
        // 注册、等待与注销都在同一个线程完成；只持有测试键，不安装键盘钩子。
        if (!RegisterHotKey(IntPtr.Zero, id, modifiers | 0x4000, key)) {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "测试键占用失败，请换用未使用的测试键");
        }
        try {
            Console.WriteLine("测试占用已就绪：PID={0}, modifiers={1}, VK=0x{2:X2}", Process.GetCurrentProcess().Id, modifiers, key);
            Console.WriteLine("保持此窗口打开；测试结束后按 Enter 注销并退出。");
            Console.ReadLine();
        } finally {
            if (!UnregisterHotKey(IntPtr.Zero, id)) {
                Console.Error.WriteLine("注销测试键失败，关闭本 PowerShell 会话后系统会释放该线程持有的注册。");
            }
        }
    }
}
'@
[NianlaiTestHotkeyHolder]::Hold($Modifiers, $VirtualKey)
