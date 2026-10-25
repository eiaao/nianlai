"""只读核对发布版本，防止构建和界面使用不同版本来源。"""

import json
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def check_versions(root: Path) -> list[str]:
    cargo = tomllib.loads((root / "src-tauri/Cargo.toml").read_text(encoding="utf-8-sig"))
    package = json.loads((root / "package.json").read_text(encoding="utf-8-sig"))
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8-sig"))
    config = json.loads((root / "src-tauri/tauri.conf.json").read_text(encoding="utf-8-sig"))
    errors = []
    version = cargo.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        errors.append("src-tauri/Cargo.toml 必须提供应用版本。")
    if package.get("version") != version:
        errors.append("package.json 的版本与 src-tauri/Cargo.toml 不一致。")
    locked = [entry.get("version") for entry in lock.get("package", []) if entry.get("name") == "nianlai"]
    if locked != [version]:
        errors.append("Cargo.lock 必须包含唯一且与 Cargo.toml 一致的 nianlai 版本。")
    if "version" in config:
        errors.append("tauri.conf.json 不得设置独立 version，Tauri 必须使用 Cargo.toml 的版本。")
    return errors


def main() -> int:
    try:
        errors = check_versions(ROOT)
    except (OSError, ValueError, TypeError) as error:
        print(f"发布版本核对失败：{error}")
        return 1
    for error in errors:
        print(f"发布版本核对失败：{error}")
    if not errors:
        print("发布版本核对通过：Cargo.toml、package.json、Cargo.lock 一致，Tauri 使用 Cargo 版本。")
    return int(bool(errors))


if __name__ == "__main__":
    raise SystemExit(main())
