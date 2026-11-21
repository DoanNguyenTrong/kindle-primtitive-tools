#!/usr/bin/env python3
"""Make the light and dark forms of the bundled app icons (assets/icons/apps).

Every app icon has two forms (PrimitiveOS design, "Icons" and "Dark mode"):
  <name>.png       light: black glyph on a white tile
  <name>-dark.png  dark:  white glyph on a black tile
The launcher draws the form for the theme (kl-core `IconSource`, kl-render `load_icons`).

The bundled icons are flat black and white squares (the launcher rounds the corners), so one form is the other
inverted, pixel by pixel (v -> 255 - v), which keeps the anti-aliasing. For each <name>.png without a dark form,
this script looks at the tile (the outermost ring of pixels): a white tile gets <name>-dark.png made from it; a black
tile is the dark form, so it is renamed to <name>-dark.png and <name>.png is made from it. Pairs that exist already
are left alone. With --check it only checks every pair: a white light tile, a black dark tile, and each the other
inverted. Run with `python3 -I scripts/icon-forms.py [--check] [dir]`; no packages needed (8-bit grayscale PNGs,
read and written with zlib and struct).
"""
import os
import struct
import sys
import zlib

SIGNATURE = b"\x89PNG\r\n\x1a\n"


def read_png(path):
    data = open(path, "rb").read()
    if not data.startswith(SIGNATURE):
        raise ValueError(f"{path}: not a PNG")
    pos, idat, header = 8, b"", None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    width, height, depth, colour, _, _, interlace = header
    if (depth, colour, interlace) != (8, 0, 0):
        raise ValueError(f"{path}: expected an 8-bit grayscale PNG, not interlaced")
    raw, rows, prev = zlib.decompress(idat), [], bytearray(width)
    for y in range(height):
        start = y * (width + 1)
        kind, line = raw[start], bytearray(raw[start + 1:start + 1 + width])
        for x in range(width):
            a = line[x - 1] if x else 0
            b, c = prev[x], (prev[x - 1] if x else 0)
            if kind == 1:
                line[x] = (line[x] + a) & 255
            elif kind == 2:
                line[x] = (line[x] + b) & 255
            elif kind == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        prev = line
    return width, height, rows


def write_png(path, width, height, rows):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + bytes(row) for row in rows)
    with open(path, "wb") as out:
        out.write(SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
                  + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def inverted(rows):
    return [bytearray(255 - v for v in row) for row in rows]


def tile_shade(rows):
    """The mean of the outermost ring of pixels: near 255 for a white tile, near 0 for a black one."""
    ring = list(rows[0]) + list(rows[-1]) + [row[0] for row in rows] + [row[-1] for row in rows]
    return sum(ring) / len(ring)


def names(folder):
    return sorted(f[:-4] for f in os.listdir(folder) if f.endswith(".png") and not f.endswith("-dark.png"))


def make(folder):
    for name in names(folder):
        light, dark = os.path.join(folder, name + ".png"), os.path.join(folder, name + "-dark.png")
        if os.path.exists(dark):
            continue
        width, height, rows = read_png(light)
        if tile_shade(rows) >= 128:
            write_png(dark, width, height, inverted(rows))
            print(f"{name}: white tile, made {name}-dark.png")
        else:
            os.rename(light, dark)
            write_png(light, width, height, inverted(rows))
            print(f"{name}: black tile, renamed it {name}-dark.png and made {name}.png")


def check(folder):
    problems = []
    for name in names(folder):
        dark_path = os.path.join(folder, name + "-dark.png")
        if not os.path.exists(dark_path):
            problems.append(f"{name}: no dark form")
            continue
        lw, lh, light = read_png(os.path.join(folder, name + ".png"))
        dw, dh, dark = read_png(dark_path)
        if tile_shade(light) < 250:
            problems.append(f"{name}.png: not a white tile")
        if tile_shade(dark) > 5:
            problems.append(f"{name}-dark.png: not a black tile")
        if (lw, lh) != (dw, dh) or inverted(light) != dark:
            problems.append(f"{name}: the forms are not each other inverted")
    for problem in problems:
        print(problem)
    print(f"{len(names(folder))} icons, {len(problems)} problems")
    return not problems


if __name__ == "__main__":
    args = sys.argv[1:]
    only_check = "--check" in args
    args = [a for a in args if a != "--check"]
    folder = args[0] if args else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "icons", "apps")
    if only_check:
        sys.exit(0 if check(folder) else 1)
    make(folder)
    sys.exit(0 if check(folder) else 1)
