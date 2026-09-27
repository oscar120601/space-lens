"""Render the app's geometric icon using only Python's standard library."""
from pathlib import Path
import math
import struct
import zlib


def png(size):
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        for x in range(size):
            u, v = (x + 0.5) / size, (y + 0.5) / size
            dx, dy = max(abs(u - 0.5) - 0.30, 0), max(abs(v - 0.5) - 0.30, 0)
            if math.hypot(dx, dy) > 0.16:
                rows.extend((0, 0, 0, 0))
                continue
            color = (int(20 + 12 * v), int(105 + 35 * v), int(111 + 29 * v), 255)
            radius = math.hypot(u - 0.45, v - 0.43)
            if 0.235 < radius < 0.28:
                color = (237, 246, 238, 255)
            # Lens handle and a tiny treemap inside the lens.
            if 0.61 < u < 0.81 and 0.59 < v < 0.81 and abs((v - 0.60) - (u - 0.60)) < 0.037:
                color = (237, 246, 238, 255)
            if 0.30 < u < 0.43 and 0.30 < v < 0.57:
                color = (156, 206, 187, 255)
            if 0.45 < u < 0.60 and 0.30 < v < 0.42:
                color = (236, 188, 118, 255)
            if 0.45 < u < 0.60 and 0.44 < v < 0.57:
                color = (120, 182, 196, 255)
            rows.extend(color)

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')


directory = Path(__file__).resolve().parent.parent / 'packaging' / 'AppIcon.iconset'
directory.mkdir(exist_ok=True)
for size in (16, 32, 128, 256, 512):
    for scale in (1, 2):
        suffix = '@2x' if scale == 2 else ''
        (directory / f'icon_{size}x{size}{suffix}.png').write_bytes(png(size * scale))
