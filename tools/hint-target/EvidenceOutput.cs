using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;

// 同目录原子发布；不退回到原地覆盖，读者只能看到完整的一代快照。
static class SnapshotFile {
    internal const int Attempts = 9;
    internal const int RetryDelayMs = 25;

    internal static void Publish(string temp, string path) {
        if (File.Exists(path)) File.Replace(temp, path, null);
        else File.Move(temp, path);
    }

    static bool Matches(string path, byte[] expected) {
        try {
            using (var stream = new FileStream(path, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete)) {
                if (stream.Length != expected.Length) return false;
                for (int i = 0; i < expected.Length; i++)
                    if (stream.ReadByte() != expected[i]) return false;
                return stream.ReadByte() == -1;
            }
        } catch (FileNotFoundException) { return false; }
    }

    public static void Write(string path, string contents) {
        Write(path, contents, Publish, Thread.Sleep);
    }

    // 注入仅供无窗口自测制造 ReplaceFile 已改变路径后仍报错的状态。
    internal static void Write(string path, string contents, Action<string, string> publish, Action<int> pause) {
        byte[] expected = new UTF8Encoding(false).GetBytes(contents);
        string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        Exception failure = null;
        try {
            for (int attempt = 0; attempt < Attempts; attempt++) {
                try {
                    // 上次失败可能已消耗或部分写入临时文件；每次从完整内容重建。
                    File.WriteAllBytes(temp, expected);
                    publish(temp, path);
                    if (Matches(path, expected)) return;
                    failure = new IOException("发布后的快照内容无法确认");
                } catch (IOException error) { failure = error; }
                catch (UnauthorizedAccessException error) { failure = error; }

                // ReplaceFile 报错不保证文件名/内容完全未变；先核对真实目标再决定重试。
                try { if (Matches(path, expected)) return; }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
                if (attempt + 1 < Attempts) pause(RetryDelayMs);
            }
            throw new IOException("快照在 " + Attempts + " 次尝试后仍未确认写入；本轮证据无效", failure);
        } finally {
            // 临时文件不是验收证据。清理失败不改变已确认的目标，也不掩盖写入异常。
            try { File.Delete(temp); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
    }
}

// 一次记录失败后永久封闭本实例的所有输出，避免漏记后又出现 closed 成功态。
sealed class EvidenceOutput {
    readonly string folder;
    readonly string run;
    readonly Func<int> currentCase;
    readonly Func<long> elapsed;
    readonly JavaScriptSerializer json = new JavaScriptSerializer();
    public bool Failed { get; private set; }

    public EvidenceOutput(string folder, string run, Func<int> currentCase, Func<long> elapsed) {
        this.folder = folder; this.run = run; this.currentCase = currentCase; this.elapsed = elapsed;
    }

    public bool Write(string name, object value) {
        return Record(name, delegate { SnapshotFile.Write(Path.Combine(folder, name), json.Serialize(value)); });
    }

    public bool Append(object value) {
        return Record("events.jsonl", delegate {
            // 追加失败可能已写入部分内容，不能盲重试造成重复或破坏事件配对。
            File.AppendAllText(Path.Combine(folder, "events.jsonl"), json.Serialize(value) + Environment.NewLine,
                new UTF8Encoding(false));
        });
    }

    bool Record(string operation, Action action) {
        if (Failed) return false;
        try { action(); return true; }
        catch (Exception error) {
            Failed = true;
            Exception cause = error.GetBaseException();
            Console.Error.WriteLine("HintTarget 记录失败，整轮无效（" + operation + "，" +
                error.GetType().Name + "，0x" + error.HResult.ToString("x8") + "；原因 " +
                cause.GetType().Name + "，0x" + cause.HResult.ToString("x8") + "）：" + error.Message);
            try {
                // 即使标记只写出一部分，存在 failure.json 就足以拒绝本轮。
                using (var stream = new FileStream(Path.Combine(folder, "failure.json"), FileMode.CreateNew,
                        FileAccess.Write, FileShare.Read))
                using (var writer = new StreamWriter(stream, new UTF8Encoding(false)))
                    writer.Write(json.Serialize(new { schema = 1, run_id = run, case_id = currentCase(),
                        phase = "failed", utc = Evidence.Utc(), elapsed_ms = elapsed(),
                        pid = System.Diagnostics.Process.GetCurrentProcess().Id, operation = operation,
                        error_type = error.GetType().FullName, hresult = "0x" + error.HResult.ToString("x8"),
                        cause_type = cause.GetType().FullName, cause_hresult = "0x" + cause.HResult.ToString("x8") }));
            } catch (Exception markerError) {
                Console.Error.WriteLine("failure.json 也无法写入：" + markerError.GetType().Name +
                    "，0x" + markerError.HResult.ToString("x8") + "；必须按非零退出/缺少正常 closed 判为无效。");
            }
            return false;
        }
    }
}
