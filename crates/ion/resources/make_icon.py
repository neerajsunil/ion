# Renders the Ion logo to an SVG (in-app) and an ICO (exe, taskbar, window).
# Run from the repo root: python crates/ion/resources/make_icon.py  (needs Pillow)
#
# The mark: an atom whose shells have cracked open and let an electron escape,
# i.e. ionization. Flat colours only, no gradients or effects.
import math
from PIL import Image, ImageDraw

root = '.'
INK, SHELL, ELECTRON = '#14151A', '#F4F1EA', '#FF5B2E'
TRAIL = '#8A3824'                 # ELECTRON at half strength over INK, kept opaque
TILE = (4, 4, 60, 60); RADIUS = 14
NUCLEUS = (26, 38, 4)             # cx, cy, r; the shells are centred on it
SHELLS = (10, 18)                 # radii
GAP = (-65, -25)                  # the opening in each shell (degrees, clockwise from +x)
STROKE = 3.2
TRAIL_LINE = ((38, 26), (42, 22), 2.6)
FREE = (47.5, 16.5, 5)            # the escaped electron

def rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))

def point(r, deg):
    cx, cy, _ = NUCLEUS
    a = math.radians(deg)
    return cx + r * math.cos(a), cy + r * math.sin(a)

def shell_path(r):
    (x0, y0), (x1, y1) = point(r, GAP[1]), point(r, GAP[0] + 360)
    return f'M{x0:.2f} {y0:.2f} A{r} {r} 0 1 1 {x1:.2f} {y1:.2f}'

(t0, t1, tw) = TRAIL_LINE
svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">
  <rect x="{TILE[0]}" y="{TILE[1]}" width="{TILE[2] - TILE[0]}" height="{TILE[3] - TILE[1]}" rx="{RADIUS}" fill="{INK}"/>
''' + ''.join(
    f'  <path d="{shell_path(r)}" fill="none" stroke="{SHELL}" stroke-width="{STROKE}" stroke-linecap="round"/>\n'
    for r in SHELLS
) + f'''  <circle cx="{NUCLEUS[0]}" cy="{NUCLEUS[1]}" r="{NUCLEUS[2]}" fill="{SHELL}"/>
  <path d="M{t0[0]} {t0[1]} L{t1[0]} {t1[1]}" stroke="{TRAIL}" stroke-width="{tw}" stroke-linecap="round"/>
  <circle cx="{FREE[0]}" cy="{FREE[1]}" r="{FREE[2]}" fill="{ELECTRON}"/>
</svg>
'''
open(f'{root}/crates/ui/brand/ion-logo.svg', 'w', newline='\n').write(svg)

def render(size):
    # Small sizes keep one heavier shell, a bigger electron and no trail, so the
    # mark doesn't collapse into a bullseye.
    small = size <= 24
    stroke = 5 if small else STROKE
    shells = (16,) if small else SHELLS
    s = 16  # supersample
    n = size * s; k = n / 64
    img = Image.new('RGBA', (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([v * k for v in TILE], RADIUS * k, fill=rgb(INK))

    def dot(cx, cy, r, colour):
        d.ellipse([(cx - r) * k, (cy - r) * k, (cx + r) * k, (cy + r) * k], fill=rgb(colour))

    cx, cy, nr = NUCLEUS
    h = stroke / 2
    for r in shells:
        box = [(cx - r - h) * k, (cy - r - h) * k, (cx + r + h) * k, (cy + r + h) * k]
        d.arc(box, GAP[1], GAP[0] + 360, fill=rgb(SHELL), width=round(stroke * k))
        for deg in GAP:  # round caps
            dot(*point(r, deg), h, SHELL)
    dot(cx, cy, nr + (1.5 if small else 0), SHELL)
    if not small:
        d.line([t0[0] * k, t0[1] * k, t1[0] * k, t1[1] * k], fill=rgb(TRAIL), width=round(tw * k))
        for p in (t0, t1):
            dot(*p, tw / 2, TRAIL)
    dot(FREE[0], FREE[1], FREE[2] + (2.5 if small else 0), ELECTRON)
    return img.resize((size, size), Image.LANCZOS)

render(256).save(f'{root}/crates/ion/resources/ion.png')
sizes = [16, 20, 24, 32, 40, 48, 64, 256]
frames = [render(s) for s in sizes]
frames[-1].save(f'{root}/crates/ion/resources/ion.ico', sizes=[(s, s) for s in sizes], append_images=frames[:-1])
print('ok')
