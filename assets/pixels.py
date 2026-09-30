"""Hand-tuned small iroh-share icons: pixel art with deliberate anti-aliasing.

Legend: '#' black, 'w' white, 'g' grey between black and white, 'l' light
grey (eye highlight at 16 px), 'a' half-transparent black (soft outer edge),
anything else transparent.
"""
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
COLORS = {'#': '#000', 'w': '#fff', 'g': '#8a8a8a', 'l': '#b8b8b8', 'a': 'rgba(0,0,0,0.5)'}

def blank(n):
    return [['.'] * n for _ in range(n)]

def glove(grid, white, arm):
    w = set(white)
    near = lambda x, y, ds: {(x + dx, y + dy) for dx, dy in ds}
    edge = set().union(*(near(x, y, ((1,0),(-1,0),(0,1),(0,-1))) for x, y in w)) - w
    corners = set().union(*(near(x, y, ((1,1),(-1,1),(1,-1),(-1,-1))) for x, y in w)) - w - edge
    for x, y in corners: grid[y][x] = 'a'
    for x, y in edge | set(arm): grid[y][x] = '#'
    for x, y in w: grid[y][x] = 'w'

def case(grid, x0, y0, x1, y1, t):
    """Rounded case with a t-pixel outline; x1/y1 inclusive."""
    for y in range(y0, y1 + 1):
        for x in range(x0, x1 + 1):
            inner = x0 + t <= x <= x1 - t and y0 + t <= y <= y1 - t
            grid[y][x] = 'w' if inner else '#'
    for cx, cy, dx, dy in ((x0, y0, 1, 1), (x1, y0, -1, 1), (x0, y1, 1, -1), (x1, y1, -1, -1)):
        if t == 2:
            grid[cy][cx] = '.'
            grid[cy][cx + dx] = 'a'; grid[cy + dy][cx] = 'a'
        else:
            grid[cy][cx] = 'a'
        if t == 2:
            grid[cy + t * dy][cx + t * dx] = "g"  # soften the inner corner

def put(grid, x, y, rows):
    for j, row in enumerate(rows):
        for i, c in enumerate(row):
            if c != ' ':
                grid[y + j][x + i] = c

def svg(grid):
    n = len(grid)
    rects = ''.join(f'<rect x="{x}" y="{y}" width="1" height="1" fill="{COLORS[c]}"/>'
                    for y, row in enumerate(grid) for x, c in enumerate(row) if c in COLORS)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}" '
            f'shape-rendering="crispEdges">{rects}</svg>\n')

# 32 px
g = blank(32)
glove(g, [(c, r) for c in (2, 4, 6) for r in (3, 4, 5)] + [(c, r) for c in range(2, 7) for r in range(6, 10)]
      + [(7, 6), (8, 6), (7, 7), (8, 7)],
      [(4, 11), (4, 12), (4, 13), (5, 14), (6, 15), (7, 16), (8, 17), (9, 17)])
case(g, 10, 2, 29, 29, 2)
eye = ['g##g', '##w#', '####', '####', 'g##g']
put(g, 14, 6, eye); put(g, 22, 6, eye)
put(g, 14, 12, ['#          #',
                'g##g    g##g',
                '  g######g  '])
for y in (16, 17, 22, 23):
    put(g, 12, y, ['#' * 16])
for y in (19, 25):
    put(g, 14, y, ['g######g  ##', 'g######g  ##'])
open(HERE / 'icon-32.svg', 'w').write(svg(g))

# 16 px
g = blank(16)
glove(g, [(1, 2), (3, 2)] + [(c, r) for c in range(1, 4) for r in (3, 4)], [(2, 6), (3, 7), (4, 8)])
case(g, 5, 1, 14, 14, 1)
put(g, 7, 3, ['#l', '##', '##']); put(g, 11, 3, ['#l', '##', '##'])
put(g, 7, 7, ['#    #', 'g####g'])
put(g, 6, 10, ['#' * 8])
put(g, 7, 12, ['g#g ##'])
open(HERE / 'icon-16.svg', 'w').write(svg(g))
