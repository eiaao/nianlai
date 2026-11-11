"""验证 PowerShell 编码核对能拦住含中文却没有 BOM 的脚本。"""

from pathlib import Path
import tempfile
import unittest

from check_ps1_encoding import BOM, missing_bom, tracked_scripts


class Ps1EncodingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nianlai-ps1-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, name: str, data: bytes) -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def test_rejects_non_ascii_without_bom(self):
        path = self.write("a.ps1", "Write-Host '中文'\n".encode("utf-8"))
        self.assertEqual(missing_bom([path]), [path])

    def test_accepts_non_ascii_with_bom(self):
        path = self.write("a.ps1", BOM + "Write-Host '中文'\n".encode("utf-8"))
        self.assertEqual(missing_bom([path]), [])

    def test_accepts_pure_ascii_with_or_without_bom(self):
        plain = self.write("a.ps1", b"Write-Host 'ok'\n")
        marked = self.write("b.ps1", BOM + b"Write-Host 'ok'\n")
        self.assertEqual(missing_bom([plain, marked]), [])

    def test_repository_scripts_pass(self):
        self.assertEqual(missing_bom(tracked_scripts(Path(__file__).resolve().parents[1])), [])


if __name__ == "__main__":
    unittest.main()
