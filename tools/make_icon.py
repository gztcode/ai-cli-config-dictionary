#!/usr/bin/env python3
"""生成 App 图标源 PNG（1024x1024，纯标准库实现）。

图案：深色圆角底 + 蓝色「配置清单」三行 + 一枚高亮光标块，
呼应「配置字典 / 键列表」的产品定位。
"""
import zlib
import struct
import sys

S = 1024
BG_TOP = (0x14, 0x18, 0x1F)
BG_BOT = (0x0C, 0x0E, 0x12)
ACCENT = (0x5A, 0xA9, 0xFF)
ACCENT_DIM = (0x2E, 0x5A, 0x8C)
FG = (0xDF, 0xE4, 0xEC)
MUTED = (0x4A, 0x55, 0x66)

R = 180  # 圆角半径


def rounded_alpha(x, y):
    """圆角矩形覆盖率（4x 超采样抗锯齿）。"""
    hits = 0
    for dx in (0.25, 0.75):
        for dy in (0.25, 0.75):
            px, py = x + dx - 0.5, y + dy - 0.5
            cx = min(max(px, R), S - R)
            cy = min(max(py, R), S - R)
            if (px - cx) ** 2 + (py - cy) ** 2 <= R * R:
                hits += 1
    return hits / 4.0


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


def build():
    rows = []
    # 三行「键 : 值」示意。坐标为 (x1, y1, x2, y2, 色)，y2 为下边界。
    lines = []
    for row_y, key_w, val_w, key_c, val_c in [
        (300, 250, 215, ACCENT, FG),
        (470, 250, 175, ACCENT_DIM, MUTED),
        (640, 250, 200, ACCENT_DIM, MUTED),
    ]:
        lines.append((250, row_y, 250 + key_w, row_y + 40, key_c))          # 键名
        lines.append((525, row_y + 8, 550, row_y + 32, MUTED))               # 冒号
        lines.append((575, row_y, 575 + val_w, row_y + 40, val_c))          # 值
    for y in range(S):
        row = bytearray()
        row.append(0)  # PNG filter: None
        for x in range(S):
            a = rounded_alpha(x, y)
            if a <= 0:
                row += bytes((0, 0, 0, 0))
                continue
            t = y / S
            color = lerp(BG_TOP, BG_BOT, t)
            for x1, y1, x2, y2, c in lines:
                if x1 <= x <= x2 and y1 <= y <= y2:
                    color = c
                    break
            row += bytes((color[0], color[1], color[2], int(round(a * 255))))
        rows.append(bytes(row))
    return rows


def write_png(path, rows):
    raw = b"".join(rows)

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)
    print(f"已生成 {path}  ({len(png)} 字节)")


if __name__ == "__main__":
    write_png(sys.argv[1] if len(sys.argv) > 1 else "app-icon.png", build())
