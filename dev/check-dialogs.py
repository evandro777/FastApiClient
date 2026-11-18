"""Checks focused dialog dismissal on private Xvfb with canned data.
Requires a debug build, xdotool, xclip, xwd, ffmpeg and tesseract. Click targets come from window OCR.
"""
import csv
import io
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "target/ui-snapshots"
OUT.mkdir(parents=True, exist_ok=True)


def check_dialog(page, title, marker, env):
    def run(*args):
        return subprocess.run(args, env=env, capture_output=True, text=True, timeout=10, check=True)

    def words(stage):
        path = OUT / f"dialogs-{page}-{stage}.png"
        pixels = subprocess.run(["xwd", "-id", window, "-silent"], env=env, capture_output=True, check=True, timeout=10).stdout
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "xwd_pipe", "-i", "-", str(path)],
                       input=pixels, capture_output=True, check=True, timeout=10)
        rows = list(csv.DictReader(io.StringIO(run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout), delimiter="\t"))
        return [row for row in rows if row["text"].strip()]

    def click(x, y):
        run("xdotool", "mousemove", "--window", window, str(x), str(y), "click", "1")
        time.sleep(.2)

    app = subprocess.Popen([ROOT / "target/debug/fastapiclient"], env=dict(env, FASTAPICLIENT_PAGE=page), stderr=subprocess.PIPE)
    try:
        window = None
        for _ in range(80):
            found = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "FastApiClient"],
                                   env=env, capture_output=True, text=True, timeout=10).stdout.splitlines()
            if found:
                window = found[0]
                break
            assert app.poll() is None, "App exited before showing a window"
            time.sleep(.1)
        assert window, "No app window"
        run("xdotool", "windowfocus", "--sync", window)
        time.sleep(.4)
        rows = words("before")
        assert any(row["text"] == marker for row in rows), f"Missing {page} dialog"
        if page in ("menu", "about", "manage"):
            if page == "menu":
                combo = next(row for row in rows if row["text"] == "Light")
                click(int(combo["left"]) + int(combo["width"]) // 2, int(combo["top"]) + int(combo["height"]) // 2)
                assert any(row["text"] == "Dark" for row in words("popup")), "Appearance list did not open"
                run("xdotool", "key", "--clearmodifiers", "Escape")
                time.sleep(.2)
                rows = words("popup-closed")
                assert any(row["text"] == marker for row in rows), "First Escape should only close the list"
                assert not any(row["text"] == "Dark" for row in rows), "Appearance list did not close"
            else:
                run("xdotool", "key", "--clearmodifiers", "Tab")
            run("xdotool", "key", "--clearmodifiers", "Escape")
            time.sleep(.2)
            assert not any(row["text"] == marker for row in words("escape")), f"Focused {page} dialog ignored Escape"
            print(f"PASS: focused {page} dialog closes on Escape", flush=True)
            return
        card_title = min((row for row in rows if row["text"] == title), key=lambda row: int(row["top"]))
        if page == "envs":
            field = next(row for row in rows if "token" in row["text"])
            click(int(field["left"]) + int(field["width"]) // 2, int(field["top"]) + int(field["height"]) // 2)
        else:
            browse = next(row for row in rows if row["text"].startswith("Bro"))
            click((int(card_title["left"]) + int(browse["left"])) // 2,
                  int(browse["top"]) + int(browse["height"]) // 2)
            run("xdotool", "key", "--clearmodifiers", "ctrl+a")
            run("xdotool", "type", "--clearmodifiers", "--delay", "1", "DialogProbe987")
            time.sleep(.2)
            run("xdotool", "key", "--clearmodifiers", "ctrl+a", "ctrl+c")
            time.sleep(.15)
            assert run("xclip", "-selection", "clipboard", "-o").stdout == "DialogProbe987", "Path field did not receive input"
        rows = words("focused")
        assert any(row["text"] == marker for row in rows), "Focusing the input dismissed the card"
        card_title = min((row for row in rows if row["text"] == title), key=lambda row: int(row["top"]))
        click(int(card_title["left"]) // 2, int(card_title["top"]) // 2)
        assert not any(row["text"] == marker for row in words("outside")), f"Focused {page} dialog ignored outside click"
        print(f"PASS: focused {page} dialog closes on outside click", flush=True)
    finally:
        app.terminate()
        app.wait(timeout=5)
        errors = app.stderr.read().decode()
        assert "panicked" not in errors and "binding loop" not in errors, errors


with tempfile.TemporaryDirectory(prefix="fastapiclient-dialogs-") as data:
    env = dict(os.environ, DISPLAY=":111", XDG_DATA_HOME=data, XDG_RUNTIME_DIR=data, LANGUAGE="en", LANG="C",
               FASTAPICLIENT_THEME="light", FASTAPICLIENT_FIXTURES="single")
    server = subprocess.Popen(["Xvfb", env["DISPLAY"], "-screen", "0", "1600x1000x24", "-nolisten", "tcp"], stderr=subprocess.PIPE)
    try:
        time.sleep(.5)
        assert server.poll() is None, "Private Xvfb failed to start"
        for page, title, marker in [("envs", "Environments", "JSON"), ("export", "Export", "Projects"), ("import", "Import", "Format"),
                                    ("menu", "Preferences", "Appearance"), ("about", "FastApiClient", "Version"), ("manage", "Manage", "Global")]:
            check_dialog(page, title, marker, env)
    finally:
        server.terminate()
        server.wait(timeout=5)
