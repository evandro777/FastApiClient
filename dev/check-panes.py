"""Private-Xvfb divider checks; requires a debug build, Pillow, Xvfb, xdotool, xwd, ffmpeg and tesseract.

Pointer targets come from window pixels/OCR, never fixed screen coordinates. All settings/projects are temporary.
"""
import csv
import ctypes
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "target/ui-snapshots"
OUT.mkdir(parents=True, exist_ok=True)
data = tempfile.TemporaryDirectory(prefix="fastapiclient-panes-")
env = dict(os.environ, DISPLAY=":109", XDG_DATA_HOME=data.name, XDG_RUNTIME_DIR=data.name,
           LANG="C", FASTAPICLIENT_THEME="light", FASTAPICLIENT_FIXTURES="single", FASTAPICLIENT_PAGE="response-normal")
settings = Path(data.name) / "FastApiClient/settings.toml"
settings.parent.mkdir()
settings.write_text('width = 1000\nheight = 700\ntheme = "light"\n')
app = None
window = None


def run(*args, check=True):
    return subprocess.run(args, env=env, text=True, capture_output=True, check=check, timeout=10)


def start():
    global app, window
    app = subprocess.Popen([ROOT / "target/debug/fastapiclient"], env=env, stderr=subprocess.PIPE)
    for _ in range(60):
        windows = run("xdotool", "search", "--onlyvisible", "--name", "FastApiClient", check=False).stdout.splitlines()
        if windows:
            window = windows[0]
            run("xdotool", "windowfocus", "--sync", window)
            time.sleep(0.5)
            return
        time.sleep(0.1)
    raise AssertionError("No app window")


def stop(normal=False):
    if normal:
        # Xvfb has no window manager; send the normal WM_DELETE_WINDOW protocol ourselves.
        class ClientMessage(ctypes.Structure):
            _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                        ("display", ctypes.c_void_p), ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                        ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]
        x11 = ctypes.CDLL("libX11.so.6")
        x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x11.XOpenDisplay.restype = ctypes.c_void_p
        x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        x11.XInternAtom.restype = ctypes.c_ulong
        x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
        x11.XFlush.argtypes = [ctypes.c_void_p]
        x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
        display = x11.XOpenDisplay(env["DISPLAY"].encode())
        assert display
        event = ClientMessage(type=33, send_event=1, display=display, window=int(window), format=32,
                              message_type=x11.XInternAtom(display, b"WM_PROTOCOLS", 0))
        event.data[0] = x11.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
        # XEvent is a 24-long union, larger than XClientMessageEvent.
        buffer = (ctypes.c_long * 24)()
        ctypes.memmove(buffer, ctypes.byref(event), ctypes.sizeof(event))
        x11.XSendEvent(display, int(window), 0, 0, buffer)
        x11.XFlush(display)
        x11.XCloseDisplay(display)
    else:
        app.terminate()
    app.wait(timeout=8)
    errors = app.stderr.read().decode()
    assert "panicked" not in errors and "binding loop" not in errors, errors


def shot(name="panes-current"):
    time.sleep(0.15)
    path = OUT / f"{name}.png"
    pixels = subprocess.run(["xwd", "-id", window, "-silent"], env=env, capture_output=True, check=True, timeout=10).stdout
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "xwd_pipe", "-i", "-", str(path)],
                   input=pixels, capture_output=True, check=True, timeout=10)
    return path, Image.open(path).convert("RGB")


def geometry():
    path, im = shot()
    w, h = im.size
    # The sidebar's flat background ends at the handle, including in empty/project/folder views.
    sidebar = next(x for x in range(w) if im.getpixel((x, h - 1)) != im.getpixel((0, h - 1)))
    words = list(csv.DictReader(io.StringIO(run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout), delimiter="\t"))
    response = [int(word["top"]) for word in words if word["text"] in ("Response", "Send") and int(word["top"]) > h // 4]
    assert response, "Could not locate response header"
    header = min(response)
    x0, x1 = sidebar + 16, w - 8
    # The full-width line immediately above the response header is the horizontal divider.
    lines = [y for y in range(header) if im.getpixel((x0, y)) != im.getpixel((x0, y - 1))
             and im.getpixel((x0, y)) != im.getpixel((w - 1, h - 1))
             and all(im.getpixel((x, y)) == im.getpixel((x0, y)) for x in range(x0, x1))]
    return sidebar, max(lines), w, h


def request_start():
    path, im = shot()
    words = list(csv.DictReader(io.StringIO(run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout), delimiter="\t"))
    tab = next(word for word in words if word["text"] == "Params")
    x = im.width - 9
    line = next(y for y in range(int(tab["top"]) + int(tab["height"]), im.height)
                if im.getpixel((x, y)) != im.getpixel((x, y - 1)))
    return line + 9  # one-pixel tab underline, then the existing 8px section gap


def click(x, y, count=1):
    run("xdotool", "mousemove", "--window", window, str(round(x)), str(round(y)), "click", "--repeat", str(count), "1")
    time.sleep(0.15)


def key(*keys):
    run("xdotool", "key", "--clearmodifiers", *keys)
    time.sleep(0.15)


def drag(x, y, dx, dy):
    run("xdotool", "mousemove", "--window", window, str(round(x)), str(round(y)), "mousedown", "1")
    for step in range(1, 5):
        run("xdotool", "mousemove", "--window", window, str(round(x + dx * step / 4)), str(round(y + dy * step / 4)))
        time.sleep(0.05)
    run("xdotool", "mouseup", "1")
    time.sleep(0.15)


server = subprocess.Popen(["Xvfb", env["DISPLAY"], "-screen", "0", "1600x1000x24", "-nolisten", "tcp"], stderr=subprocess.PIPE)
try:
    time.sleep(0.5)
    assert server.poll() is None, server.stderr.read().decode()
    start()
    s, y, w, h = geometry()
    assert s == 300
    _, im = shot()
    x0, x1 = s + 24, w - 16
    editor_top = next(row for row in range(y + 8, h)
                      if im.getpixel((x0, row)) != im.getpixel((w - 1, h - 1))
                      and all(im.getpixel((x, row)) == im.getpixel((x0, row)) for x in range(x0, x1)))
    # Before this change: 25px status + 32px tabs + 32px toolbar, three 6px gaps, 17px separator/gaps.
    recovered = 25 + 32 + 32 + 18 + 17 - (editor_top - (y + 4.5) + 8)
    assert recovered >= 24, recovered
    print(f"PASS: normal response recovers {recovered:.0f}px of combined editor height", flush=True)
    drag(s + 4, h / 2, 80, 0)
    assert geometry()[0] == 380, geometry()
    key("Tab", "shift+Tab", "Right")
    assert geometry()[0] == 388
    key("Left", "Return")
    assert geometry()[0] == 300
    drag(geometry()[0] + 4, h / 2, -200, 0)
    assert geometry()[0] == 260
    drag(geometry()[0] + 4, h / 2, w, 0)
    assert geometry()[0] == w - 592
    click(geometry()[0] + 4, h / 2, 2)
    assert geometry()[0] == 300
    s, y, w, h = geometry()
    drag((s + w) / 2, y, 0, 48)
    assert abs(geometry()[1] - y - 48) <= 1, (y, geometry())
    key("Tab", "shift+Tab", "Down")
    assert abs(geometry()[1] - y - 56) <= 1
    key("Up", "Return")
    assert abs(geometry()[1] - y) <= 1
    drag((s + w) / 2, y, 0, -h)
    top = geometry()[1]
    assert abs(top - request_start() - 3.5 - 140) <= 1, top
    drag((s + w) / 2, top, 0, h * 2)
    bottom = geometry()[1]
    assert bottom > top and abs(h - 8 - bottom - 4.5 - 180) <= 1, bottom
    click((s + w) / 2, bottom, 2)
    assert abs(geometry()[1] - y) <= 1
    drag((s + w) / 2, y, 0, 80)
    drag(s + 4, h / 2, 80, 0)
    run("xdotool", "windowsize", window, "900", "560")
    assert geometry()[0] == 308
    run("xdotool", "windowsize", window, str(w), str(h))
    assert geometry()[0] == 380
    assert abs(geometry()[1] - y - 80) <= 1
    shot("panes-pointer-keyboard")
    stop()

    # Use the real save/restore path with two disposable requests and an isolated instance socket.
    del env["FASTAPICLIENT_FIXTURES"]
    del env["FASTAPICLIENT_PAGE"]
    projects = settings.parent / "projects"
    projects.mkdir()
    (projects / "panes.json").write_text(json.dumps({"id": "panes", "name": "Pane check", "requests": [
        {"name": "First", "url": "https://example.com/first"}, {"name": "Second", "url": "https://example.com/second"}]}))
    settings.write_text('width = 1000\nheight = 700\nlast_project = "panes"\nlast_request = 0\ntheme = "light"\n')
    start()
    s, y, w, h = geometry()
    drag(s + 4, h / 2, 60, 0)
    drag((w + 360) / 2, y, 0, -40)
    expected = geometry()[:2]
    # Select the next request using the existing sidebar keyboard navigation.
    path, _ = shot()
    words = list(csv.DictReader(io.StringIO(run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout), delimiter="\t"))
    row = next(word for word in words if word["text"] == "Second")
    click(int(row["left"]) + int(row["width"]) / 2, int(row["top"]) + int(row["height"]) / 2)
    assert geometry()[:2] == expected
    stop(normal=True)
    saved = tomllib.loads(settings.read_text())
    assert saved["sidebar_width"] == 360 and 0 < saved["request_split"] < 0.5, saved
    start()
    assert geometry()[:2] == expected, (expected, geometry())
    stop(normal=True)
    print("PASS: pointer capture, boundaries, 8px keyboard steps, resets, window clamping, request switching, restart persistence")
finally:
    if app and app.poll() is None:
        app.terminate()
        app.wait(timeout=5)
    server.terminate()
    server.wait(timeout=5)
    data.cleanup()
