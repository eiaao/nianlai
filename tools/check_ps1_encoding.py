"""核对已跟踪的 PowerShell 脚本：含非 ASCII 字节（如中文）时必须带 UTF-8 BOM。

Windows PowerShell 5.1 会把没有 BOM 的文件按系统 ANSI 代码页读取，中文会变成乱码甚至解析失败。
"""

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BOM = b"\xef\xbb\xbf"


def missing_bom(paths: list[Path]) -> list[Path]:
    bad = []
    for path in paths:
        data = path.read_bytes()
        if not data.startswith(BOM) and any(byte > 127 for byte in data):
            bad.append(path)
    return bad


def tracked_scripts(root: Path) -> list[Path]:
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.ps1"],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return [root / name.decode("utf-8") for name in output.split(b"\0") if name]


def main() -> int:
    # Windows 管道默认是本地代码页（CI 上是 cp1252），中文会编码失败。
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    try:
        scripts = tracked_scripts(ROOT)
        bad = missing_bom(scripts)
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"PowerShell 编码核对失败：{error}")
        return 1
    for path in bad:
        print(f"PowerShell 编码核对失败：{path.relative_to(ROOT).as_posix()} 含非 ASCII 字符但没有 UTF-8 BOM。")
    if not bad:
        print(f"PowerShell 编码核对通过：{len(scripts)} 个脚本。")
    return int(bool(bad))


if __name__ == "__main__":
    raise SystemExit(main())
