using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Web.Script.Serialization;

// 只用独立临时目录和真实 Windows 文件句柄；不构造 Form 或调用输入接口。
static class FileIoTests {
    static void Check(bool condition, string name) {
        if (!condition) throw new Exception("文件自测失败：" + name);
    }

    static IOException Fails(Action action, string name) {
        try { action(); }
        catch (IOException error) { return error; }
        throw new Exception("文件自测应失败但成功：" + name);
    }

    static string NewPath(string root, string name) {
        string folder = Path.Combine(root, name);
        Directory.CreateDirectory(folder);
        return Path.Combine(folder, "state.json");
    }

    static void Pass(ref int count, string name) { count++; Console.WriteLine("PASS " + name); }

    public static int Run() {
        string tempRoot = Path.GetFullPath(Path.GetTempPath()).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        string folderName = "nianlai-hint-target-io-" + Guid.NewGuid().ToString("N");
        string root = Path.GetFullPath(Path.Combine(tempRoot, folderName));
        Directory.CreateDirectory(root);
        int count = 0;
        try {
            string path = NewPath(root, "new");
            SnapshotFile.Write(path, "{\"test\":\"虚构数据\"}");
            Check(File.ReadAllText(path) == "{\"test\":\"虚构数据\"}", "首次完整写入");
            Check(File.ReadAllBytes(path)[0] == (byte)'{', "UTF-8 无 BOM");
            Pass(ref count, "new snapshot / UTF-8 without BOM");

            path = NewPath(root, "delete-share");
            File.WriteAllText(path, "old");
            using (var reader = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
                SnapshotFile.Write(path, "new");
                Check(File.ReadAllText(path) == "new", "DELETE 共享时发布新一代");
                using (var text = new StreamReader(reader)) Check(text.ReadToEnd() == "old", "既有句柄保持完整旧一代");
            }
            Pass(ref count, "real reader sharing DELETE / complete old and new snapshots");

            path = NewPath(root, "transient-lock");
            File.WriteAllText(path, "old");
            int attempts = 0;
            var locked = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            using (var attempted = new ManualResetEvent(false)) {
                var release = new Thread(delegate() { attempted.WaitOne(); Thread.Sleep(50); locked.Dispose(); });
                release.Start();
                try {
                    SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                        attempts++;
                        try { SnapshotFile.Publish(temp, destination); }
                        finally { attempted.Set(); }
                    }, Thread.Sleep);
                } finally { attempted.Set(); release.Join(); locked.Dispose(); }
            }
            Check(attempts > 1 && attempts <= SnapshotFile.Attempts && File.ReadAllText(path) == "new", "真实短暂占用恢复");
            Pass(ref count, "real transient no-DELETE lock recovered / attempts=" + attempts);

            path = NewPath(root, "permanent-lock");
            File.WriteAllText(path, "old");
            attempts = 0;
            var watch = Stopwatch.StartNew();
            IOException permanent;
            using (var reader = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite)) {
                permanent = Fails(delegate { SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                    attempts++; SnapshotFile.Publish(temp, destination);
                }, Thread.Sleep); }, "长期占用必须失败");
            }
            Check(attempts == SnapshotFile.Attempts && watch.ElapsedMilliseconds < 5000, "有界尝试次数");
            Check(File.ReadAllText(path) == "old" && Directory.GetFiles(Path.GetDirectoryName(path), "*.tmp").Length == 0,
                "真实长期共享冲突保留旧快照并清理临时文件");
            Check(permanent.InnerException is IOException, "保留底层失败");
            Pass(ref count, "real permanent no-DELETE lock rejected / attempts=" + attempts +
                " / inner HRESULT=0x" + permanent.InnerException.HResult.ToString("x8"));

            path = NewPath(root, "readonly");
            File.WriteAllText(path, "old");
            File.SetAttributes(path, File.GetAttributes(path) | FileAttributes.ReadOnly);
            try { Fails(delegate { SnapshotFile.Write(path, "new"); }, "只读目标必须失败"); }
            finally { File.SetAttributes(path, FileAttributes.Normal); }
            Check(File.ReadAllText(path) == "old", "只读目标不被非原子覆盖");
            Pass(ref count, "real read-only destination rejected");

            path = NewPath(root, "replace-reported-failure");
            File.WriteAllText(path, "old");
            attempts = 0;
            SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                attempts++; SnapshotFile.Publish(temp, destination);
                throw new IOException("injected: replacement committed before reported error");
            }, Thread.Sleep);
            Check(attempts == 1 && File.ReadAllText(path) == "new", "替换后抛错需确认实际目标");
            Pass(ref count, "injected error after successful Replace / confirmed once");

            path = NewPath(root, "move-reported-failure");
            SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                SnapshotFile.Publish(temp, destination);
                throw new IOException("injected: initial move committed before reported error");
            }, Thread.Sleep);
            Check(File.ReadAllText(path) == "new", "首次移动后抛错也确认目标");
            Pass(ref count, "injected error after successful initial Move / confirmed");

            path = NewPath(root, "partial-replace");
            File.WriteAllText(path, "old");
            attempts = 0;
            SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                attempts++;
                if (attempts == 1) {
                    File.Delete(destination); File.Delete(temp);
                    throw new IOException("injected: destination and source names unavailable after partial replacement");
                }
                SnapshotFile.Publish(temp, destination);
            }, Thread.Sleep);
            Check(attempts == 2 && File.ReadAllText(path) == "new", "结果不确定时重建完整临时文件");
            Pass(ref count, "injected partial Replace / missing source and destination rebuilt");

            path = NewPath(root, "unconfirmable");
            File.WriteAllText(path, "old");
            FileStream unreadable = null;
            try {
                Fails(delegate { SnapshotFile.Write(path, "new", delegate(string temp, string destination) {
                    if (unreadable == null) {
                        SnapshotFile.Publish(temp, destination);
                        unreadable = new FileStream(destination, FileMode.Open, FileAccess.Read, FileShare.None);
                        throw new IOException("injected: committed but target cannot be confirmed");
                    }
                    SnapshotFile.Publish(temp, destination);
                }, Thread.Sleep); }, "无法确认时不得声称成功");
            } finally { if (unreadable != null) unreadable.Dispose(); }
            Check(File.ReadAllText(path) == "new", "确实制造了已发布却不可确认的状态");
            Pass(ref count, "injected committed but unconfirmable destination / rejected");

            path = NewPath(root, "snapshot-failure-session");
            File.WriteAllText(path, "{\"phase\":\"running\"}");
            var output = new EvidenceOutput(Path.GetDirectoryName(path), "test-run", () => 3, () => 100L);
            using (var reader = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
                Check(!output.Write("state.json", new { phase = "running", generation = 2 }), "记录层识别快照失败");
            Check(output.Failed && !output.Write("state.json", new { phase = "closed" }), "失败后拒绝 closed");
            Check(File.ReadAllText(path) == "{\"phase\":\"running\"}", "失败后不产生成功快照");
            var marker = new JavaScriptSerializer().DeserializeObject(File.ReadAllText(Path.Combine(Path.GetDirectoryName(path), "failure.json")))
                as System.Collections.Generic.Dictionary<string, object>;
            Check((string)marker["phase"] == "failed" && (string)marker["operation"] == "state.json" &&
                (string)marker["run_id"] == "test-run" && (int)marker["case_id"] == 3, "失败协议字段");
            Check((string)marker["cause_hresult"] == "0x" + permanent.InnerException.HResult.ToString("x8"),
                "失败标记保留真实共享冲突原因");
            Pass(ref count, "permanent snapshot failure / failure.json / sticky invalid session");

            path = NewPath(root, "append-failure-session");
            string events = Path.Combine(Path.GetDirectoryName(path), "events.jsonl");
            output = new EvidenceOutput(Path.GetDirectoryName(path), "test-run", () => 1, () => 10L);
            Check(output.Append(new { kind = "case_started" }), "事件初次追加");
            using (var reader = new FileStream(events, FileMode.Open, FileAccess.Read, FileShare.Read))
                Check(!output.Append(new { kind = "down", vk = 86 }), "事件共享冲突必须失败");
            Check(output.Failed && !output.Append(new { kind = "up", vk = 86 }) &&
                !output.Write("state.json", new { phase = "closed" }), "漏记后拒绝所有后续记录");
            Check(File.ReadAllLines(events).Length == 1 && !File.Exists(path), "不重复追加也不声称闭合");
            Check(File.Exists(Path.Combine(Path.GetDirectoryName(path), "failure.json")), "事件失败也写标记");
            Pass(ref count, "real append sharing failure / no retry or later events / no closed");

            string missing = Path.Combine(root, "missing-directory");
            output = new EvidenceOutput(missing, "test-run", () => 1, () => 0L);
            Check(!output.Write("state.json", new { phase = "running" }) && output.Failed, "故障标记本身也写不了仍失败");
            Directory.CreateDirectory(missing);
            Check(!output.Write("state.json", new { phase = "closed" }) && !File.Exists(Path.Combine(missing, "state.json")),
                "故障标记不可写不能恢复成功态");
            Pass(ref count, "failure marker unavailable / session remains failed");

            // 递归清理前验证最终绝对路径和本次 GUID 目录名，绝不把计算出的父目录作为目标。
            string cleanupRoot = Path.GetFullPath(root);
            Check(string.Equals(Path.GetDirectoryName(cleanupRoot), tempRoot, StringComparison.OrdinalIgnoreCase) &&
                string.Equals(Path.GetFileName(cleanupRoot), folderName, StringComparison.Ordinal) &&
                (File.GetAttributes(cleanupRoot) & FileAttributes.ReparsePoint) == 0, "清理路径必须是本次临时目录");
            Directory.Delete(cleanupRoot, true);
            return count;
        } catch {
            Console.Error.WriteLine("文件自测失败；保留合成测试目录：" + root);
            throw;
        }
    }
}
