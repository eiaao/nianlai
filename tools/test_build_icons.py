# -*- coding: utf-8 -*-
"""图标生成产物的结构测试：全部断言只依赖 Python 标准库。

运行：python -X utf8 -I tools/test_build_icons.py [产物目录]
默认产物目录是仓库里的 src-tauri/icons。生成器见 tools/build_icons.py。
"""
from __future__ import annotations

import importlib.util
import os
import re
import struct
import sys
import tempfile
import types
import unittest.mock
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GENERATOR = os.path.join(REPO, "tools", "build_icons.py")
ICONS = os.path.join(REPO, "src-tauri", "icons")
SOURCE = os.path.join(ICONS, "source")

# 与 build_icons.py 约定的全部产物；tray/ 供 Rust include_bytes! 打包。
TARGETS = [
    "icon.png", "icon.ico", "icon.icns", "32x32.png", "128x128.png", "128x128@2x.png",
    "Square30x30Logo.png", "Square44x44Logo.png", "Square48x48Logo.png", "Square64x64Logo.png",
    "Square71x71Logo.png", "Square89x89Logo.png",
    "Square107x107Logo.png", "Square142x142Logo.png", "Square150x150Logo.png",
    "Square284x284Logo.png", "Square310x310Logo.png", "StoreLogo.png",
] + [f"tray/{name}-{size}.png" for name in ("recording", "paused-light", "paused-dark", "error") for size in (16, 24, 32)]

PNG_SIZES = {
    "icon.png": 512, "32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256,
    "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square48x48Logo.png": 48,
    "Square64x64Logo.png": 64, "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89, "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142, "Square150x150Logo.png": 150, "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310, "StoreLogo.png": 50,
}
TRAY_SIZES = {16: 16, 24: 24, 32: 32}
# ICO 必须包含五档；16/24 用的手绘专门版。
ICO_REQUIRED = {16, 24, 32, 48, 256}


# ---------- PNG 解码（8 位 RGBA，标准库 zlib + 反滤波） ----------
def png_rgba(data: bytes) -> tuple[int, int, list[list[tuple[int, int, int, int]]]]:
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "PNG 魔数不对"
    pos, width, height, bit_depth, color, chunks = 8, 0, 0, 0, 0, []
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        (crc,) = struct.unpack(">I", data[pos + 8 + length:pos + 12 + length])
        assert crc == zlib.crc32(kind + body) & 0xFFFFFFFF, f"{kind} CRC 不符"
        if kind == b"IHDR":
            width, height, bit_depth, color = struct.unpack(">IIBB", body[:10])
        chunks.append((kind, body))
        pos += 12 + length
        if kind == b"IEND":
            break
    assert bit_depth == 8 and color == 6, "只测 8 位 RGBA"
    raw = zlib.decompress(b"".join(body for kind, body in chunks if kind == b"IDAT"))
    stride = width * 4
    rows: list[list[tuple[int, int, int, int]]] = []
    prev = bytearray(stride)
    offset = 0
    for _ in range(height):
        filter_type = raw[offset]
        line = bytearray(raw[offset + 1:offset + 1 + stride])
        offset += 1 + stride
        for i in range(stride):
            left = line[i - 4] if i >= 4 else 0
            up = prev[i]
            corner = prev[i - 4] if i >= 4 else 0
            if filter_type == 1:
                line[i] = (line[i] + left) & 0xFF
            elif filter_type == 2:
                line[i] = (line[i] + up) & 0xFF
            elif filter_type == 3:
                line[i] = (line[i] + (left + up) // 2) & 0xFF
            elif filter_type == 4:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                line[i] = (line[i] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 0xFF
            elif filter_type != 0:
                raise AssertionError(f"未知滤波 {filter_type}")
        rows.append([tuple(line[i:i + 4]) for i in range(0, stride, 4)])
        prev = line
    return width, height, rows


# ---------- 像素 SVG 解析：手绘源全是整数格 <rect> ----------
RECT = re.compile(
    r'<rect x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)" fill="([#0-9A-Fa-f]{4,9})"/>')


def pixel_svg_grid(path: str) -> list[list[tuple[int, int, int, int]]]:
    text = open(path, encoding="utf-8").read()
    match = re.search(r'viewBox="0 0 (\d+) \1"', text)
    assert match, f'手绘源缺少正方形 viewBox：{path}'
    size = int(match.group(1))
    grid = [[(0, 0, 0, 0)] * size for _ in range(size)]
    for x, y, w, h, fill in RECT.findall(text):
        r, g, b = int(fill[1:3], 16), int(fill[3:5], 16), int(fill[5:7], 16)
        for dy in range(int(h)):
            for dx in range(int(w)):
                grid[int(y) + dy][int(x) + dx] = (r, g, b, 255)
    return grid


def assert_grid_equal(actual, expected, what: str) -> None:
    assert len(actual) == len(expected), f"{what} 尺寸不符"
    for y, (row_a, row_e) in enumerate(zip(actual, expected)):
        for x, (a, e) in enumerate(zip(row_a, row_e)):
            assert a == e, f"{what} 在 ({x},{y}) 像素 {a} ≠ 期望源 {e}"


def read_ico_entries(data: bytes) -> dict[int, bytes]:
    assert data[:4] == b"\x00\x00\x01\x00", "ICO 头不对"
    (count,) = struct.unpack("<H", data[4:6])
    assert count > 0 and len(data) >= 6 + count * 16, "ICO 目录越界"
    entries = {}
    for i in range(count):
        start = 6 + i * 16
        width = data[start] or 256
        height = data[start + 1] or 256
        assert width == height, f"ICO 第 {i} 项宽高不符：{width}×{height}"
        assert width not in entries, f"ICO 尺寸重复：{width}"
        (length, offset) = struct.unpack("<II", data[start + 8:start + 16])
        assert offset >= 6 + count * 16 and offset + length <= len(data), f"ICO {width} 档越界"
        entries[width] = data[offset:offset + length]
    return entries


def decode_ico_entry(size: int, payload: bytes):
    if payload[:8] == b"\x89PNG\r\n\x1a\n":
        width, height, grid = png_rgba(payload)
        assert (width, height) == (size, size), f"ICO {size} PNG 是 {width}×{height}"
        return grid
    # BMP 条目：BITMAPINFOHEADER + 自底向上 BGRA + AND 掩码。
    assert len(payload) >= 40, f"ICO {size} BMP 头截断"
    (header_size,) = struct.unpack("<I", payload[:4])
    (width_i, h2) = struct.unpack("<ii", payload[4:12])
    (planes, bpp) = struct.unpack("<HH", payload[12:16])
    assert header_size == 40 and width_i == size and h2 == size * 2, f"{size} 档 BMP 尺寸不合规"
    assert planes == 1 and bpp == 32, f"{size} 档 BMP 格式不合规"
    height = h2 // 2
    stride = ((size * 4 + 3) // 4) * 4
    mask_row = ((size + 31) // 32) * 4
    assert len(payload) >= 40 + stride * height + mask_row * height, f"ICO {size} BMP 像素或掩码越界"
    grid = []
    for y in range(height):
        row = []
        base = 40 + (height - 1 - y) * stride
        for x in range(size):
            b, g, r, a = payload[base + x * 4:base + x * 4 + 4]
            row.append((r, g, b, a))
        grid.append(row)
    return grid


def load_generator():
    spec = importlib.util.spec_from_file_location("nianlai_build_icons_under_test", GENERATOR)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def expect_raises(exception, run, what: str):
    try:
        run()
    except exception as error:
        return error
    raise AssertionError(f"{what} 应抛出 {exception.__name__}")


def tree_snapshot(root: str):
    if not os.path.exists(root):
        return None
    snapshot = {}
    for base, directories, files in os.walk(root):
        directories.sort()
        files.sort()
        relative_base = os.path.relpath(base, root)
        if relative_base != ".":
            snapshot[relative_base.replace(os.sep, "/") + "/"] = None
        for name in files:
            path = os.path.join(base, name)
            relative = os.path.relpath(path, root).replace(os.sep, "/")
            with open(path, "rb") as source:
                snapshot[relative] = source.read()
    return snapshot


def fake_grid(size: int):
    color = (size % 256, 80, 120, 255)
    return [[color for _ in range(size)] for _ in range(size)]


def run_generator(module, output: str):
    with unittest.mock.patch.object(module.sys, "argv", ["build_icons.py", "--output", output]):
        return module.main()


def seeded_output(parent: str) -> str:
    output = os.path.join(parent, "custom-icons")
    os.makedirs(os.path.join(output, "source"))
    with open(os.path.join(output, "source", "keep.txt"), "wb") as target:
        target.write(b"source must survive\x00\xff")
    with open(os.path.join(output, "icon.png"), "wb") as target:
        target.write(b"the only old icon")
    with open(os.path.join(output, "unrelated.bin"), "wb") as target:
        target.write(bytes(range(256)))
    return output


def main() -> int:
    icons = sys.argv[1] if len(sys.argv) > 1 else ICONS
    failures: list[str] = []

    def check(name: str, run) -> None:
        try:
            run()
            print(f"PASS  {name}")
        except (AssertionError, OSError, RuntimeError, struct.error, ValueError, KeyError, IndexError, zlib.error) as error:
            failures.append(f"{name}: {error}")
            print(f"FAIL  {name}: {error}")

    # 1. 生成器与产物齐全、PNG 尺寸正确。
    def generator_exists():
        assert os.path.isfile(GENERATOR), f"生成器不存在：{GENERATOR}"
    check("生成器存在", generator_exists)

    def all_targets_exist():
        for name in TARGETS:
            assert os.path.isfile(os.path.join(icons, name)), f"缺少 {name}"
    check("全部产物存在", all_targets_exist)

    for name, size in PNG_SIZES.items():
        def png_size(name=name, size=size):
            width, height, _ = png_rgba(open(os.path.join(icons, name), "rb").read())
            assert (width, height) == (size, size), f"{name} 是 {width}×{height}，应为 {size}"
        check(f"PNG 尺寸 {name}={size}", png_size)

    # 2. ICO：五档齐全，所有载荷都解码并与对应生产源逐像素比较。
    def ico_has_all_sizes():
        data = open(os.path.join(icons, "icon.ico"), "rb").read()
        entries = read_ico_entries(data)
        assert set(entries) == ICO_REQUIRED, f"ICO 应严格为 {sorted(ICO_REQUIRED)}，实际 {sorted(entries)}"
    check("ICO 包含 16/24/32/48/256", ico_has_all_sizes)

    def ico_entry_matches_source():
        data = open(os.path.join(icons, "icon.ico"), "rb").read()
        entries = read_ico_entries(data)
        for size, source_name in (
            (16, "source/app-16.svg"),
            (24, "source/app-24.svg"),
            (32, "32x32.png"),
            (48, "Square48x48Logo.png"),
            (256, "128x128@2x.png"),
        ):
            if source_name.endswith(".svg"):
                expected = pixel_svg_grid(os.path.join(icons, source_name))
            else:
                width, height, expected = png_rgba(open(os.path.join(icons, source_name), "rb").read())
                assert (width, height) == (size, size), f"生产 PNG {source_name} 尺寸不符"
            grid = decode_ico_entry(size, entries[size])
            assert_grid_equal(grid, expected, f"ICO {size} 档")
    check("ICO 16/24/32/48/256 与对应生产源逐像素一致", ico_entry_matches_source)

    def edge_nonzero_rejects_even_with_decodable_screenshot():
        module = load_generator()
        with tempfile.TemporaryDirectory(prefix="nianlai-icon-edge-red-") as work:
            def nonzero(command, **kwargs):
                shot_arg = next(arg for arg in command if arg.startswith("--screenshot="))
                shot = shot_arg.split("=", 1)[1]
                with open(shot, "wb") as target:
                    target.write(module.encode_png(fake_grid(32)))
                return types.SimpleNamespace(returncode=23, stderr="mock Edge failed")
            with unittest.mock.patch.object(module.subprocess, "run", side_effect=nonzero):
                error = expect_raises(
                    RuntimeError,
                    lambda: module.render_vector(work, 32, "app-32.svg"),
                    "Edge 非零返回码",
                )
            assert "32" in str(error) and "returncode=23" in str(error), error
    check("Edge 非零返回码拒绝可解码截图", edge_nonzero_rejects_even_with_decodable_screenshot)

    def render_failure_preserves_custom_output():
        module = load_generator()
        with tempfile.TemporaryDirectory(prefix="nianlai-icon-atomic-red-") as parent:
            output = seeded_output(parent)
            before = tree_snapshot(output)
            def fail_midway(work, size, source_name):
                if size == 64:
                    raise RuntimeError("mock 渲染中途失败")
                return fake_grid(size)
            with unittest.mock.patch.object(module, "render_vector", side_effect=fail_midway):
                expect_raises(RuntimeError, lambda: run_generator(module, output), "渲染中途失败")
            assert tree_snapshot(output) == before, "失败前原 custom output 已逐字节改变"
            assert os.listdir(parent) == ["custom-icons"], "失败后不得留下 staging/backup"
    check("渲染中途失败保持自定义输出逐字节不变", render_failure_preserves_custom_output)

    def commit_failure_rolls_back_custom_output():
        module = load_generator()
        with tempfile.TemporaryDirectory(prefix="nianlai-icon-rollback-") as parent:
            output = seeded_output(parent)
            before = tree_snapshot(output)
            real_replace = module.os.replace
            replacements = 0
            def fail_second_replace(source, target):
                nonlocal replacements
                replacements += 1
                if replacements == 2:
                    raise OSError("mock staging 提交失败")
                return real_replace(source, target)
            with unittest.mock.patch.object(module, "render_vector", side_effect=lambda work, size, source: fake_grid(size)), \
                    unittest.mock.patch.object(module.os, "replace", side_effect=fail_second_replace):
                expect_raises(OSError, lambda: run_generator(module, output), "staging 提交失败")
            assert replacements >= 3, "提交失败后必须实际调用 rollback replace"
            assert tree_snapshot(output) == before, "rollback 后原 output 应逐字节不变"
            assert os.listdir(parent) == ["custom-icons"], "rollback 后不得留下 staging/backup"
    check("提交替换失败 rollback 恢复原输出", commit_failure_rolls_back_custom_output)

    def successful_custom_output_is_complete_and_preserves_source():
        module = load_generator()
        with tempfile.TemporaryDirectory(prefix="nianlai-icon-success-") as parent:
            output = seeded_output(parent)
            with unittest.mock.patch.object(module, "render_vector", side_effect=lambda work, size, source: fake_grid(size)):
                assert run_generator(module, output) == 0
            for name in TARGETS:
                path = os.path.join(output, name)
                assert os.path.isfile(path), f"成功产物缺少 {name}"
                if name.endswith(".png"):
                    width, height, _ = png_rgba(open(path, "rb").read())
                    expected = PNG_SIZES.get(name, 32 if name.startswith("tray/") and name.endswith("-32.png") else 16 if name.endswith("-16.png") else 24)
                    assert (width, height) == (expected, expected), f"{name} 尺寸 {width}×{height}"
            assert open(os.path.join(output, "source", "keep.txt"), "rb").read() == b"source must survive\x00\xff"
            assert open(os.path.join(output, "unrelated.bin"), "rb").read() == bytes(range(256))
            assert os.listdir(parent) == ["custom-icons"], "成功后必须清理 staging/backup"
    check("成功提交完整产物并保留 source 与既有内容", successful_custom_output_is_complete_and_preserves_source)

    # 3. ICNS：魔数与 PNG 载荷。
    def icns_magic():
        data = open(os.path.join(icons, "icon.icns"), "rb").read()
        assert data[:4] == b"icns", "ICNS 魔数不对"
        (total,) = struct.unpack(">I", data[4:8])
        assert total == len(data), "ICNS 总长不符"
        pos, types = 8, set()
        while pos + 8 <= len(data):
            kind = data[pos:pos + 4]
            (length,) = struct.unpack(">I", data[pos + 4:pos + 8])
            assert length >= 8 and pos + length <= len(data), f"ICNS 条目长度异常：{length}"
            body = data[pos + 8:pos + length]
            types.add(kind.decode("ascii"))
            if kind in (b"ic07", b"ic08", b"ic09"):
                width, height, _ = png_rgba(body)
                expect = {b"ic07": 128, b"ic08": 256, b"ic09": 512}[kind]
                assert (width, height) == (expect, expect), f"{kind} 是 {width}×{height}"
            pos += length
        assert pos == len(data), "ICNS 末尾有多余或截断数据"
        assert {"ic07", "ic08", "ic09"} <= types, f"ICNS 缺档 {types}"
    check("ICNS 含 128/256/512 PNG 档", icns_magic)

    # 4. 托盘：16/24 等于手绘源；32 是对应 16 最近邻放大 2 倍。
    tray_sources = {
        "recording": ("app-16.svg", "app-24.svg"),
        "paused-light": ("tray-paused-16-light.svg", "tray-paused-24-light.svg"),
        "paused-dark": ("tray-paused-16-dark.svg", "tray-paused-24-dark.svg"),
        "error": ("tray-error-16.svg", "tray-error-24.svg"),
    }
    for name, (s16, s24) in tray_sources.items():
        def tray_pixels(name=name, s16=s16, s24=s24):
            grids = {}
            for size in TRAY_SIZES:
                width, height, grid = png_rgba(open(os.path.join(icons, "tray", f"{name}-{size}.png"), "rb").read())
                assert (width, height) == (size, size)
                grids[size] = grid
            assert_grid_equal(grids[16], pixel_svg_grid(os.path.join(SOURCE, s16)), f"tray/{name}-16")
            assert_grid_equal(grids[24], pixel_svg_grid(os.path.join(SOURCE, s24)), f"tray/{name}-24")
            base = pixel_svg_grid(os.path.join(SOURCE, s16))
            for y in range(32):
                for x in range(32):
                    assert grids[32][y][x] == base[y // 2][x // 2], f"tray/{name}-32 不是 16 最近邻放大（{x},{y}）"
            # 24 必须来自独立手绘源，不能等于 32 的最近邻或双线性缩小产物。
            nearest_differed = any(
                grids[24][y][x] != grids[32][y * 32 // 24][x * 32 // 24]
                for y in range(24) for x in range(24))
            bilinear_differed = any(
                grids[24][y][x] != blend_32(grids[32], x, y)
                for y in range(24) for x in range(24))
            assert nearest_differed and bilinear_differed, f"tray/{name}-24 疑似由 32 缩小而来"
        check(f"托盘 {name} 三档", tray_pixels)

    print(("\n全部通过" if not failures else f"\n{len(failures)} 项失败") + f"（{icons}）")
    return 1 if failures else 0


def blend_32(grid, x, y):
    """把 32 图按双线性缩小到 24 的 (x,y) 处的近似值，用来证明 24 不是缩放产物。"""
    fx, fy = x * 32 / 24, y * 32 / 24
    x0, y0 = min(int(fx), 31), min(int(fy), 31)
    x1, y1 = min(x0 + 1, 31), min(y0 + 1, 31)
    tx, ty = fx - x0, fy - y0
    out = []
    for c in range(4):
        top = grid[y0][x0][c] * (1 - tx) + grid[y0][x1][c] * tx
        bottom = grid[y1][x0][c] * (1 - tx) + grid[y1][x1][c] * tx
        out.append(round(top * (1 - ty) + bottom * ty))
    return tuple(out)


if __name__ == "__main__":
    sys.exit(main())
