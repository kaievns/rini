#!/usr/bin/env python3
# pattern.py <letter>: fills the terminal with a ruler that redraws at every size. Row r reads "<L><rr>" at both
# ends, the first row says TOP and the last BOT, and every 5 rows change colour, so a frame shows which rows a tile
# draws, where it cuts, and whether glyphs stretch.
import os, signal, sys, time

L = sys.argv[1] if len(sys.argv) > 1 else "A"
COLS = [31, 32, 33, 34, 35, 36]


def draw(*_):
    c, r = os.get_terminal_size()
    out = ["\x1b[?25l\x1b[H"]
    for i in range(r):
        n = i + 1
        tag = "TOP" if i == 0 else "BOT" if i == r - 1 else f"{n:03d}"
        left = f"{L}{tag}"
        body_w = max(0, c - 2 * len(left) - 2)
        body = "".join("|" if (k % 10) == 9 else str(k % 10) for k in range(body_w))
        if i in (0, r - 1):
            body = "=" * body_w
        line = f"{left} {body} {left}"[:c]
        out.append(f"\x1b[{COLS[(i // 5) % len(COLS)]};1m{line.ljust(c)}\x1b[0m")
        if i < r - 1:
            out.append("\r\n")
    sys.stdout.write("".join(out))
    sys.stdout.flush()


signal.signal(signal.SIGWINCH, draw)
sys.stdout.write("\x1b[?1049h")
draw()
while True:
    time.sleep(3600)
