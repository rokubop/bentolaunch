"""Rasterise bentolaunch.svg to a multi-size .ico. Stdlib only, no image libraries.

    python assets/make-icon.py

The svg is the drawing anyone reads; this is the one Windows loads. Keep the
shapes in step.
"""
import os, struct, zlib

os.chdir(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

BG      = (0x1A, 0x1A, 0x1E)
SLATE   = (0x4C, 0x55, 0x68)
GAP     = (0x2A, 0x2F, 0x3C)
WARM_A  = (0xFF, 0xC2, 0x4B)
WARM_B  = (0xFF, 0x5F, 0x6D)

SS = 4  # supersampling factor per axis


def in_round_rect(px, py, x, y, w, h, r):
    if px < x or py < y or px > x + w or py > y + h:
        return False
    r = min(r, w / 2, h / 2)
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    dx, dy = px - cx, py - cy
    return dx * dx + dy * dy <= r * r


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def render(size, mode):
    """RGBA bytes, top-down. `mode` is "full", "plain" or "tiny".

    The lift that carries the idea at 256px turns to mush at 16, where the
    picked tile collides with the one below it. Small sizes get a clean 2x2
    instead: same four tiles, same one in colour, nothing overlapping.
    """
    s = size / 256.0
    px = bytearray(size * size * 4)

    if mode == "tiny":
        tiles = [(40, 40), (140, 40), (40, 140)]
        picked = (140, 140, 76, 76, 20)
        radius = 20
    else:
        tiles = [(44, 44), (136, 44), (44, 136)]
        picked = (150, 154, 88, 88, 21)
        radius = 18
    with_gap = mode == "full"
    step = 1.0 / SS

    for row in range(size):
        for col in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(SS):
                for sx in range(SS):
                    # Sample point in 256-space.
                    ux = (col + (sx + 0.5) * step) / s
                    uy = (row + (sy + 0.5) * step) / s

                    colr, a = None, 0.0
                    if in_round_rect(ux, uy, 0, 0, 256, 256, 56):
                        colr, a = BG, 1.0

                    if with_gap and in_round_rect(ux, uy, 133, 133, 82, 82, 21) \
                            and not in_round_rect(ux, uy, 139, 139, 70, 70, 15):
                        colr, a = GAP, 1.0

                    for tx, ty in tiles:
                        if in_round_rect(ux, uy, tx, ty, 76, 76, radius):
                            colr, a = SLATE, 1.0
                            break

                    pxx, pyy, pw, ph, pr = picked
                    if in_round_rect(ux, uy, pxx, pyy, pw, ph, pr):
                        t = ((ux - pxx) / pw + (uy - pyy) / ph) / 2
                        colr, a = lerp(WARM_A, WARM_B, min(max(t, 0.0), 1.0)), 1.0

                    if colr:
                        acc[0] += colr[0] * a
                        acc[1] += colr[1] * a
                        acc[2] += colr[2] * a
                        acc[3] += a

            n = SS * SS
            alpha = acc[3] / n
            i = (row * size + col) * 4
            if alpha > 0:
                px[i + 0] = round(acc[0] / acc[3])
                px[i + 1] = round(acc[1] / acc[3])
                px[i + 2] = round(acc[2] / acc[3])
            px[i + 3] = round(alpha * 255)
    return bytes(px)


def png(size, rgba):
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + rgba[r * size * 4:(r + 1) * size * 4] for r in range(size))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def dib(size, rgba):
    """BMP form for an ICO entry: BGRA bottom-up, then an empty AND mask."""
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    rows = []
    for r in range(size - 1, -1, -1):
        row = bytearray()
        for c in range(size):
            i = (r * size + c) * 4
            row += bytes((rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]))
        rows.append(bytes(row))
    mask_row = ((size + 31) // 32) * 4
    return header + b"".join(rows) + b"\x00" * (mask_row * size)


SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
images = []
for sz in SIZES:
    mode = "tiny" if sz <= 20 else ("plain" if sz < 48 else "full")
    rgba = render(sz, mode)
    payload = png(sz, rgba) if sz == 256 else dib(sz, rgba)
    images.append((sz, payload))
    if sz == 256:
        open("assets/bentolaunch-256.png", "wb").write(payload)

# Chrome wants loose PNGs. Same shapes from the same script, so the extension
# icon cannot drift from the app's.
os.makedirs("extension/icons", exist_ok=True)
for sz in (16, 32, 48, 128):
    mode = "tiny" if sz <= 20 else ("plain" if sz < 48 else "full")
    open(f"extension/icons/{sz}.png", "wb").write(png(sz, render(sz, mode)))
print("extension icons: 16, 32, 48, 128")

out = bytearray(struct.pack("<HHH", 0, 1, len(images)))
offset = 6 + 16 * len(images)
entries, blobs = bytearray(), bytearray()
for sz, payload in images:
    entries += struct.pack("<BBBBHHII", sz % 256, sz % 256, 0, 0, 1, 32, len(payload), offset)
    blobs += payload
    offset += len(payload)

open("assets/bentolaunch.ico", "wb").write(bytes(out + entries + blobs))
print("ico:", os.path.getsize("assets/bentolaunch.ico"), "bytes,", len(images), "sizes")
print("png:", os.path.getsize("assets/bentolaunch-256.png"), "bytes")
