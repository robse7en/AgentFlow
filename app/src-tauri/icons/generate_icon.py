"""Deterministic AgentFlow "A" mark for the Windows executable; needs Pillow.

Mirrors the sidebar logo in frontend/src/components/AppSidebar.vue: the dark arch
on the lime gradient tile, drawn from the same bezier path.
"""
from pathlib import Path
from PIL import Image, ImageDraw

MASTER = 1024
SIZES = [16, 24, 32, 48, 64, 128, 256]
FROM, TO, INK = (0xDC, 0xF7, 0x7A), (0xA8, 0xD9, 0x3A), (0x0E, 0x0E, 0x0C)
RADIUS = 0.4        # tile corner radius, as a fraction of the tile (16px on the 40px logo)
GLYPH = 0.62        # glyph box as a fraction of the tile; larger than the sidebar's 0.55 so it holds up small
STROKE = 2.4        # in the logo's 24-unit viewBox

# M4 17 c4 0 4-10 8-10 s4 10 8 10, as absolute cubic segments in the 24-unit viewBox.
SEGMENTS = [((4, 17), (8, 17), (8, 7), (12, 7)), ((12, 7), (16, 7), (16, 17), (20, 17))]


def bezier(p0, p1, p2, p3, steps=200):
    for i in range(steps + 1):
        t = i / steps
        u = 1 - t
        yield tuple(u**3 * a + 3 * u * u * t * b + 3 * u * t * t * c + t**3 * d
                    for a, b, c, d in zip(p0, p1, p2, p3))


def master():
    gradient = Image.new('RGB', (MASTER, MASTER))
    pixels = gradient.load()
    for y in range(MASTER):
        for x in range(MASTER):
            t = (x + y) / (2 * (MASTER - 1))  # 135deg
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(FROM, TO))

    mask = Image.new('L', (MASTER, MASTER), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, MASTER - 1, MASTER - 1), RADIUS * MASTER, fill=255)
    tile = gradient.convert('RGBA')
    tile.putalpha(mask)

    scale = GLYPH * MASTER / 24
    offset = (MASTER - 24 * scale) / 2
    points = [(offset + x * scale, offset + y * scale)
              for segment in SEGMENTS for x, y in bezier(*segment)]
    ink = Image.new('L', (MASTER, MASTER), 0)
    draw = ImageDraw.Draw(ink)
    width = STROKE * scale
    for x, y in points:  # a dense run of round dots gives a clean stroke with round caps
        draw.ellipse((x - width / 2, y - width / 2, x + width / 2, y + width / 2), fill=255)
    tile.paste(Image.new('RGBA', (MASTER, MASTER), INK + (255,)), (0, 0), ink)
    return tile


if __name__ == '__main__':
    master().save(Path(__file__).with_name('icon.ico'), format='ICO',
                  sizes=[(s, s) for s in SIZES])
