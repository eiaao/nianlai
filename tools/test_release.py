"""在隔离仓库中验证便携版发布脚本，不执行真实构建。"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "tools" / "release.ps1"
POWERSHELL = Path(os.environ.get("WINDIR", r"C:\Windows")) / "System32/WindowsPowerShell/v1.0/powershell.exe"
VERSION = "0.1.0-alpha.1"


@unittest.skipUnless(os.name == "nt", "需要 Windows PowerShell 5.1")
class ReleaseTests(unittest.TestCase):
    def setUp(self):
        test_root = ROOT / "target" / "release-tests"
        test_root.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="发布 自测 ", dir=test_root)
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name).resolve()
        self.assertTrue(self.repo.is_relative_to(test_root.resolve()))
        (self.repo / "tools").mkdir()
        shutil.copyfile(SCRIPT, self.repo / "tools/release.ps1")
        (self.repo / "src-tauri").mkdir()
        self.manifest = self.repo / "src-tauri/Cargo.toml"
        self.manifest.write_text(
            '[package]\nname = "nianlai"\nversion = "' + VERSION + '"\n'
            '[dependencies]\nexample = { version = "1.2.3" }\n', encoding="utf-8"
        )
        self.package = self.repo / "package.json"
        self.package.write_text(json.dumps({"name": "nianlai", "version": VERSION}), encoding="utf-8")
        self.config = self.repo / "src-tauri/tauri.conf.json"
        self.config.write_text('{"productName":"Nianlai","bundle":{"active":true}}', encoding="utf-8")
        (self.repo / ".gitignore").write_text("dist/\nbin/\nbuild-output/\ncommands.log\n", encoding="utf-8")
        self.binary = self.repo / "build-output/release/nianlai.exe"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"MZ\x00\x00portable-release-test")
        self.bin = self.repo / "bin"
        self.bin.mkdir()
        metadata = json.dumps({"target_directory": str(self.repo / "build-output")})
        self.ps_stub("cargo", "if ($args[0] -eq 'metadata') { '" + metadata + "' }")
        self.ps_stub("rustc", "'rustc 1.90.0 (release-test)'")
        self.stub("pnpm", "exit /b 0")
        self.env = os.environ.copy()
        self.env["PATH"] = str(self.bin) + os.pathsep + self.env["PATH"]
        # 从 Python 启动 5.1 时不继承 PowerShell 7 的模块目录。
        self.env.pop("PSMODULEPATH", None)
        self.env["NIANLAI_RELEASE_TEST_COMMAND_LOG"] = str(self.repo / "commands.log")
        self.env.pop("NIANLAI_RELEASE_TEST_FAIL", None)
        self.git("init", "--quiet")
        self.git("add", ".")
        self.git("-c", "user.name=发布测试", "-c", "user.email=release-test@example.invalid", "commit", "--quiet", "-m", "发布测试基线")
        self.commit = self.git("rev-parse", "HEAD").strip()

    def stub(self, name, body):
        # 测试桩只记录参数并返回固定输出，不运行编译器或包管理器。
        content = (
            "@echo off\r\n"
            f'echo {name} %*>> "%NIANLAI_RELEASE_TEST_COMMAND_LOG%"\r\n'
            f'if "%NIANLAI_RELEASE_TEST_FAIL%"=="{name}" exit /b 19\r\n'
            + body + "\r\n"
        )
        (self.bin / (name + ".cmd")).write_bytes(content.encode("utf-8"))

    def ps_stub(self, name, body):
        # 不经过 cmd，防止本机 AutoRun 的提示文字污染元数据标准输出。
        content = (
            f"[IO.File]::AppendAllText($env:NIANLAI_RELEASE_TEST_COMMAND_LOG, '{name} ' + ($args -join ' ') + \"`r`n\")\n"
            + body + "\nexit 0\n"
        )
        (self.bin / (name + ".ps1")).write_text(content, encoding="utf-8-sig")

    def git(self, *arguments):
        result = subprocess.run(["git", "-C", str(self.repo), *arguments], capture_output=True, text=True, encoding="utf-8", check=True)
        return result.stdout

    def release(self, *arguments, success=True):
        result = subprocess.run(
            [str(POWERSHELL), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(self.repo / "tools/release.ps1"), *arguments],
            cwd=ROOT, env=self.env, capture_output=True, timeout=40,
        )
        output = result.stdout.decode("utf-8", errors="replace") + result.stderr.decode("utf-8", errors="replace")
        if success:
            self.assertEqual(result.returncode, 0, output)
        else:
            self.assertNotEqual(result.returncode, 0, output)
        return output

    def commands(self):
        path = self.repo / "commands.log"
        return path.read_text(encoding="utf-8") if path.exists() else ""

    def verify_artifacts(self, directory, version=VERSION):
        archive = directory / f"nianlai-{version}-windows-x64-portable.zip"
        checksum = directory / f"nianlai-{version}.sha256"
        info = json.loads((directory / "build-info.json").read_text(encoding="utf-8-sig"))
        with zipfile.ZipFile(archive) as contents:
            self.assertEqual(set(contents.namelist()), {"nianlai.exe", "使用说明.txt"})
            self.assertEqual(contents.read("nianlai.exe"), self.binary.read_bytes())
            instructions = contents.read("使用说明.txt").decode("utf-8-sig")
            for text in ("WebView2", "SmartScreen", "更多信息 → 仍要运行", r"%LOCALAPPDATA%\Nianlai\g0\data", "Ctrl+Alt+Shift+Q"):
                self.assertIn(text, instructions)
        self.assertEqual(checksum.read_text(encoding="utf-8-sig").strip(), hashlib.sha256(archive.read_bytes()).hexdigest() + " *" + archive.name)
        self.assertEqual(info["version"], version)
        self.assertEqual(info["git_commit"], self.commit)
        self.assertIn("rustc 1.90.0", info["rustc_version"])
        self.assertTrue(info["build_time_utc"])
        self.assertTrue(info["packaged_at_utc"])
        self.assertEqual(info["exe_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
        self.assertEqual(list(directory.glob(".nianlai-release-*")), [])
        return info

    def test_powershell_51_syntax_and_encoding(self):
        data = SCRIPT.read_bytes()
        self.assertTrue(data.startswith(b"\xef\xbb\xbf"))
        self.assertNotIn(b"\n", data.replace(b"\r\n", b""))
        command = "$tokens=$null; $errors=$null; [void][System.Management.Automation.Language.Parser]::ParseFile($args[0],[ref]$tokens,[ref]$errors); if ($errors.Count) { $errors | Out-String; exit 1 }; $PSVersionTable.PSVersion.ToString()"
        result = subprocess.run([str(POWERSHELL), "-NoProfile", "-Command", "& { " + command + " }", str(SCRIPT)], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        self.assertIn(b"5.1.", result.stdout)

    def test_skip_build_packages_shared_target_and_defaults(self):
        self.release("-SkipBuild")
        info = self.verify_artifacts(self.repo / "dist")
        self.assertTrue(info["skip_build"])
        self.assertFalse(info["git_dirty"])
        self.assertIn("pnpm install --frozen-lockfile", self.commands())
        self.assertNotIn("tauri build", self.commands())

    def test_build_invocation(self):
        self.release()
        info = self.verify_artifacts(self.repo / "dist")
        self.assertFalse(info["skip_build"])
        self.assertEqual(self.commands().count("pnpm tauri build --no-bundle"), 1)

    def test_version_override_and_relative_output_with_spaces(self):
        version = "0.2.0-rc.2+test.1"
        before = self.config.read_bytes()
        self.release("-SkipBuild", "-Version", version, "-OutDir", "dist/输出 文件")
        info = self.verify_artifacts(self.repo / "dist/输出 文件", version)
        self.assertTrue(info["git_dirty"])
        self.assertIn('version = "' + version + '"', self.manifest.read_text(encoding="utf-8-sig"))
        self.assertIn('example = { version = "1.2.3" }', self.manifest.read_text(encoding="utf-8-sig"))
        self.assertEqual(json.loads(self.package.read_text(encoding="utf-8-sig"))["version"], version)
        self.assertEqual(self.config.read_bytes(), before)

    def test_rejects_untracked_changes(self):
        (self.repo / "未提交.txt").write_text("未提交", encoding="utf-8")
        self.release("-SkipBuild", success=False)
        self.assertEqual(self.commands(), "")
        self.assertFalse((self.repo / "dist").exists())

    def test_rejects_staged_changes(self):
        self.package.write_text('{"name":"nianlai","version":"0.2.0"}', encoding="utf-8")
        self.git("add", "package.json")
        self.release("-SkipBuild", success=False)
        self.assertEqual(self.commands(), "")

    def test_allows_dirty_only_when_requested(self):
        self.package.write_text('{"name":"nianlai","version":"0.0.0"}', encoding="utf-8")
        self.release("-SkipBuild", success=False)
        self.assertEqual(self.commands(), "")
        self.release("-SkipBuild", "-AllowDirty")
        info = self.verify_artifacts(self.repo / "dist")
        self.assertTrue(info["git_dirty"])
        self.assertEqual(json.loads(self.package.read_text(encoding="utf-8-sig"))["version"], VERSION)

    def test_invalid_versions_do_not_modify_files(self):
        before = self.manifest.read_bytes(), self.package.read_bytes()
        for version in ("01.2.3", "0.1.0-alpha.01", "v0.1.0", "0.1.0/../../bad", "0.1.0-", "0.1.0\n"):
            with self.subTest(version=version):
                self.release("-SkipBuild", "-Version", version, success=False)
                self.assertEqual((self.manifest.read_bytes(), self.package.read_bytes()), before)
                self.assertEqual(self.commands(), "")

    def test_missing_binary_does_not_create_archive(self):
        self.binary.unlink()
        self.release("-SkipBuild", success=False)
        self.assertFalse((self.repo / "dist").exists())
        self.assertNotIn("tauri build", self.commands())

    def test_rejects_second_tauri_version_source(self):
        before = self.manifest.read_bytes(), self.package.read_bytes()
        self.config.write_text('{"version":"0.0.0"}', encoding="utf-8")
        self.release("-SkipBuild", "-AllowDirty", "-Version", "0.2.0", success=False)
        self.assertEqual((self.manifest.read_bytes(), self.package.read_bytes()), before)
        self.assertEqual(self.commands(), "")

    def test_failed_build_does_not_package_old_binary(self):
        # 第一次安装成功，只有正式构建步骤失败，已有旧程序也不得继续打包。
        self.stub("pnpm", 'if "%1"=="tauri" exit /b 23\r\nexit /b 0')
        self.release(success=False)
        self.assertFalse((self.repo / "dist").exists())

    def test_failed_install_stops_before_build(self):
        self.env["NIANLAI_RELEASE_TEST_FAIL"] = "pnpm"
        self.release(success=False)
        self.assertNotIn("tauri build", self.commands())
        self.assertFalse((self.repo / "dist").exists())


if __name__ == "__main__":
    unittest.main()
