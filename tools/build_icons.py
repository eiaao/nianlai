# -*- coding: utf-8 -*-
"""拈来程序图标生成器：`src-tauri/icons/source/` 里的 SVG 源 -> 全部程序图标。

只依赖 Python 标准库和本机 Edge（矢量尺寸的光栅化）。重复运行输出逐字节一致。

规则（与 internal/design/v12-icon 第 6 轮一致）：
- 16/24 应用图标与托盘三态用整数格手绘源，逐像素转 PNG（不经缩放）。
- 托盘 32 三档由对应 16 源最近邻放大 2 倍；24 用手绘 24 源。
- 32/48 应用图标用手绘专门版；128/256/512 及 Square*、StoreLogo 由 256 主稿矢量渲染。
- icon.ico 自组装：16/24/32/48 为 BMP 条目，256 为 PNG 条目，固定五档；16/24 来自手绘源。
- icon.icns：ic07(128) / ic08(256) / ic09(512) 三档 PNG。

用法：python -X utf8 -I tools/build_icons.py [--output <目录>]
默认输出 src-tauri/icons。结构测试见 tools/test_build_icons.py。
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_OUTPUT = os.path.join(REPO, "src-tauri", "icons")
SOURCE = os.path.join(DEFAULT_OUTPUT, "source")
EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"

# 矢量渲染清单：尺寸 -> 源文件（256 主稿负责一切可缩放档位）。
VECTOR: dict[int, str] = {
    30: "app-256.svg", 44: "app-256.svg", 50: "app-256.svg", 64: "app-256.svg",
    71: "app-256.svg", 89: "app-256.svg", 107: "app-256.svg", 128: "app-256.svg",
    142: "app-256.svg", 150: "app-256.svg", 256: "app-256.svg", 284: "app-256.svg",
    310: "app-256.svg", 512: "app-256.svg",
    32: "app-32.svg", 48: "app-48.svg",
}
TRAY = {
    "recording": ("tray-recording-16.svg", "tray-recording-24.svg"),
    "paused-light": ("tray-paused-16-light.svg", "tray-paused-24-light.svg"),
    "paused-dark": ("tray-paused-16-dark.svg", "tray-paused-24-dark.svg"),
    "error": ("tray-error-16.svg", "tray-error-24.svg"),
}
RECT = re.compile(r'<rect x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)" fill="([#0-9A-Fa-f]{4,9})"/>')


# ---------- 像素 SVG -> 网格 ----------
def pixel_grid(path: str) -> list[list[tuple[int, int, int, int]]]:
    text = open(path, encoding="utf-8").read()
    size = int(re.search(r'viewBox="0 0 (\d+) \1"', text).group(1))
    grid = [[(0, 0, 0, 0)] * size for _ in range(size)]
    for x, y, w, h, fill in RECT.findall(text):
        color = (int(fill[1:3], 16), int(fill[3:5], 16), int(fill[5:7], 16), 255)
        for dy in range(int(h)):
            for dx in range(int(w)):
                grid[int(y) + dy][int(x) + dx] = color
    return grid


def upscale2x(grid: list[list[tuple[int, int, int, int]]]) -> list[list[tuple[int, int, int, int]]]:
    return [[color for color in row for _ in range(2)] for row in grid for _ in range(2)]


# ---------- PNG 编码（8 位 RGBA） ----------
def chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def encode_png(grid: list[list[tuple[int, int, int, int]]]) -> bytes:
    height, width = len(grid), len(grid[0])
    raw = b"".join(b"\x00" + b"".join(bytes(pixel) for pixel in row) for row in grid)
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


# ---------- 矢量 SVG -> PNG（无头 Edge） ----------
def render_vector(work: str, size: int, source_name: str) -> list[list[tuple[int, int, int, int]]]:
    source_path = os.path.join(SOURCE, source_name)
    page = os.path.join(work, f"page{size}.html")
    shot = os.path.join(work, f"shot{size}.png")
    html = (f'<!doctype html><meta charset="utf-8"><style>'
            f'html,body{{margin:0;padding:0;background:transparent;overflow:hidden}}'
            f'img{{display:block;width:{size}px;height:{size}px}}</style>'
            f'<img src="file:///{source_path.replace(os.sep, "/")}">')
    open(page, "w", encoding="utf-8").write(html)
    command = [
        EDGE, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run",
        f"--window-size={size},{size}", "--default-background-color=00000000",
        "--force-device-scale-factor=1", f"--screenshot={shot}", f"file:///{page.replace(os.sep, '/')}",
    ]
    done = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if done.stderr.strip():
        # 非阻断输出照抄给使用者判断；是否失败只由返回码决定。
        print(f"[edge {size}] {done.stderr.strip()[:200]}", file=sys.stderr)
    if done.returncode != 0:
        raise RuntimeError(f"Edge 渲染 {size}×{size} 失败，returncode={done.returncode}")
    data = open(shot, "rb").read()
    grid = decode_png(data)
    assert len(grid) == size and len(grid[0]) == size, f"Edge 渲染 {size} 得到 {len(grid[0])}×{len(grid)}"
    return grid


def decode_png(data: bytes) -> list[list[tuple[int, int, int, int]]]:
    pos, width, height, raw_chunks = 8, 0, 0, []
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        raw_chunks.append((kind, data[pos + 8:pos + 8 + length]))
        if kind == b"IHDR":
            width, height, _, color = struct.unpack(">IIBB", raw_chunks[-1][1][:10])
            assert color == 6, "Edge 截图应为 RGBA"
        pos += 12 + length
        if kind == b"IEND":
            break
    raw = zlib.decompress(b"".join(body for kind, body in raw_chunks if kind == b"IDAT"))
    stride = width * 4
    rows, prev, offset = [], bytearray(stride), 0
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
        rows.append([tuple(line[i:i + 4]) for i in range(0, stride, 4)])
        prev = line
    return rows


# ---------- ICO / ICNS 组装 ----------
def bmp_entry(grid: list[list[tuple[int, int, int, int]]]) -> bytes:
    size = len(grid)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(
        b"".join(struct.pack("<4B", pixel[2], pixel[1], pixel[0], pixel[3]) for pixel in row)
        for row in reversed(grid))
    mask_row = ((size + 31) // 32) * 4
    mask = b"\x00" * (mask_row * size)
    return header + pixels + mask


def build_ico(entries: list[tuple[int, bytes]]) -> bytes:
    header = struct.pack("<HH", 0, 1) + struct.pack("<H", len(entries))
    directory, body, offset = b"", b"", 6 + 16 * len(entries)
    for size, payload in entries:
        directory += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(payload), offset)
        body += payload
        offset += len(payload)
    return header + directory + body


def build_icns(sizes: dict[int, bytes]) -> bytes:
    body = b""
    for size, kind in ((128, b"ic07"), (256, b"ic08"), (512, b"ic09")):
        payload = sizes[size]
        body += kind + struct.pack(">I", len(payload) + 8) + payload
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def commit_staging(staging: str, output: str) -> None:
    """同文件系统替换整个目录；旧目录先保留为 backup，失败可改回。"""
    backup = staging + "-backup"
    had_output = os.path.exists(output)
    if had_output:
        os.replace(output, backup)
    try:
        os.replace(staging, output)
    except OSError as commit_error:
        if had_output:
            try:
                os.replace(backup, output)
            except OSError as rollback_error:
                raise RuntimeError(
                    f"图标提交失败且 rollback 失败；旧输出仍在 {backup}："
                    f"commit={commit_error}; rollback={rollback_error}"
                ) from rollback_error
        raise
    if had_output:
        shutil.rmtree(backup)


def main() -> int:
    parser = argparse.ArgumentParser(description="生成拈来全部程序图标")
    parser.add_argument("--output", default=DEFAULT_OUTPUT, help="输出目录（默认 src-tauri/icons）")
    options = parser.parse_args()
    output = os.path.abspath(options.output)
    if os.path.exists(output) and not os.path.isdir(output):
        raise NotADirectoryError(f"图标输出不是目录：{output}")
    parent = os.path.dirname(output)
    os.makedirs(parent, exist_ok=True)
    staging = tempfile.mkdtemp(prefix=f".{os.path.basename(output)}-staging-", dir=parent)
    work = tempfile.mkdtemp(prefix="work-", dir=staging)
    try:
        # source/ 和其它既有内容随 staging 一起保留；原 output 在提交前完全不动。
        if os.path.isdir(output):
            shutil.copytree(output, staging, dirs_exist_ok=True)
        grids: dict[int, list[list[tuple[int, int, int, int]]]] = {}
        # 手绘档：应用图标 16/24、托盘 16/24/32。
        app16 = pixel_grid(os.path.join(SOURCE, "app-16.svg"))
        app24 = pixel_grid(os.path.join(SOURCE, "app-24.svg"))
        for name, (s16, s24) in TRAY.items():
            grid16 = pixel_grid(os.path.join(SOURCE, s16))
            grid24 = pixel_grid(os.path.join(SOURCE, s24))
            os.makedirs(os.path.join(staging, "tray"), exist_ok=True)
            for size, grid in ((16, grid16), (24, grid24), (32, upscale2x(grid16))):
                open(os.path.join(staging, "tray", f"{name}-{size}.png"), "wb").write(encode_png(grid))
        # 矢量档：Edge 渲染。
        special = {512: "icon.png", 256: "128x128@2x.png", 128: "128x128.png", 32: "32x32.png", 50: "StoreLogo.png"}
        for size, source_name in sorted(VECTOR.items()):
            grid = render_vector(work, size, source_name)
            grids[size] = grid
            name = special.get(size) or f"Square{size}x{size}Logo.png"
            open(os.path.join(staging, name), "wb").write(encode_png(grid))
            print(f"渲染 {size}×{size} -> {name}")
        # ICO：四档 BMP + 256 PNG，共五档；16/24 用手绘源，绝不能从 256 缩小。
        entries: list[tuple[int, bytes]] = [(16, bmp_entry(app16)), (24, bmp_entry(app24))]
        for size in (32, 48):
            entries.append((size, bmp_entry(grids[size])))
        entries.append((256, encode_png(grids[256])))
        open(os.path.join(staging, "icon.ico"), "wb").write(build_ico(entries))
        # ICNS。
        open(os.path.join(staging, "icon.icns"), "wb").write(
            build_icns({size: encode_png(grids[size]) for size in (128, 256, 512)}))
        # 临时 Edge 页面不属于产物；全部成功后才同文件系统提交。
        shutil.rmtree(work)
        commit_staging(staging, output)
        print(f"图标已生成：{output}")
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)
        shutil.rmtree(staging, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
