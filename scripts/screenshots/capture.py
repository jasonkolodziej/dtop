"""Drive dtop in a pseudo-terminal and dump each screen's cells to JSON for render.py.

Usage: python3 capture.py OUT_DIR [-- COMMAND...]   (default command: .build/debug/dtop)

Expects the demo stack from compose.yaml (project "shop") plus a running `cache` container and a
created-but-never-started `migrate-db` container, so the views have realistic content.
"""
import os, pty, time, select, fcntl, termios, struct, json, sys
import pyte

COLS, ROWS = 150, 42
OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)
cmd = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else [".build/debug/dtop"]
cmd = cmd + ["--interval", "0.5"]
pid, fd = pty.fork()
if pid == 0:
    os.environ.update(COLORTERM="truecolor", TERM="xterm-256color")
    os.execvp(cmd[0], cmd)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)


def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                stream.feed(os.read(fd, 65536))
            except OSError:
                return


def lines():
    return [l.rstrip() for l in screen.display]


def find(text, after_row=0):
    for y, l in enumerate(lines()):
        if y >= after_row and text in l:
            return l.index(text), y
    raise SystemExit(f"NOT FOUND {text!r}\n" + "\n".join(lines()))


def click(text, after_row=0):
    x, y = find(text, after_row)
    os.write(fd, f"\x1b[<0;{x + 3};{y + 1}M\x1b[<0;{x + 3};{y + 1}m".encode())
    pump(0.8)


def send(data, wait=0.8):
    os.write(fd, data)
    pump(wait)


def capture(name, theme_kind="dark"):
    cells = []
    for y in range(ROWS):
        row = []
        for x in range(COLS):
            c = screen.buffer[y][x]
            row.append([c.data, c.fg, c.bg, c.bold, c.reverse])
        cells.append(row)
    with open(os.path.join(OUT, name + ".json"), "w") as f:
        json.dump({"cols": COLS, "rows": ROWS, "kind": theme_kind, "cells": cells}, f)
    print("captured", name)


pump(32)  # ~60 samples of history at 0.5s

click("● shop-web-1")
pump(2)
capture("containers")

send(b"2")
click("shop-web-1", after_row=10)
send(b"v", 2.5)
capture("compose-processes")

click("shop-client-1", after_row=10)
send(b"v", 2.5)
capture("logs")
send(b"v")  # back to overview

send(b"1")
click("● shop-web-1")
send(b"?", 1)
capture("help")
send(b"x")

click("migrate-db")
send(b"D", 1)
capture("confirm")
send(b"n")

click("● shop-web-1")
send(b"T", 1)
for _ in range(6):
    send(b"j", 0.15)  # accent
send(b"\r")
send(b"\x15")
send(b"#ff8a3d\r", 1)
for _ in range(5):
    send(b"j", 0.15)  # move a little further so the edited row shows as modified
capture("theme-editor")
send(b"\x1b")
send(b"y", 1)

themes = ["daylight", "midnight", "ember", "forest", "glacier", "dusk", "contrast"]
light = {"daylight", "glacier"}
for name in themes:
    send(b"t", 1.5)
    capture("theme-" + name, "light" if name in light else "dark")

send(b"\x03", 1)
