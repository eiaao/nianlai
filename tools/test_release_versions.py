"""验证发布版本核对能拦住独立版本来源和清单漂移。"""

import json
from pathlib import Path
import tempfile
import unittest

from check_release_versions import check_versions


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nianlai-version-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "src-tauri").mkdir()
        (self.root / "src-tauri/Cargo.toml").write_text(
            '[package]\nname = "nianlai"\nversion = "0.1.0-alpha.1"\n', encoding="utf-8"
        )
        (self.root / "package.json").write_text('{"version":"0.1.0-alpha.1"}', encoding="utf-8")
        (self.root / "src-tauri/tauri.conf.json").write_text("{}", encoding="utf-8")
        self.lock = self.root / "Cargo.lock"
        self.lock.write_text('[[package]]\nname = "nianlai"\nversion = "0.1.0-alpha.1"\n', encoding="utf-8")

    def test_accepts_single_consistent_version(self):
        self.assertEqual(check_versions(self.root), [])

    def test_rejects_package_version_drift(self):
        (self.root / "package.json").write_text('{"version":"0.0.0"}', encoding="utf-8")
        self.assertTrue(any("package.json" in error for error in check_versions(self.root)))

    def test_rejects_missing_duplicate_or_outdated_lock_version(self):
        original = self.lock.read_text(encoding="utf-8")
        for data in ("", original + original, original.replace("alpha.1", "alpha.2")):
            with self.subTest(data=data):
                self.lock.write_text(data, encoding="utf-8")
                self.assertTrue(any("Cargo.lock" in error for error in check_versions(self.root)))

    def test_rejects_second_tauri_version_even_when_equal(self):
        for version in ("0.1.0-alpha.1", "0.0.0", None):
            with self.subTest(version=version):
                (self.root / "src-tauri/tauri.conf.json").write_text(json.dumps({"version": version}), encoding="utf-8")
                self.assertTrue(any("tauri.conf.json" in error for error in check_versions(self.root)))

    def test_rejects_missing_cargo_version(self):
        (self.root / "src-tauri/Cargo.toml").write_text('[package]\nname = "nianlai"\n', encoding="utf-8")
        self.assertTrue(any("Cargo.toml 必须" in error for error in check_versions(self.root)))


if __name__ == "__main__":
    unittest.main()
