"""Creation/menu checks on private Xvfb with temporary projects; requires a debug build,
Pillow, xdotool, xclip, xwd, ffmpeg and tesseract. Pointer targets come from window OCR/pixels.
"""
import csv
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "target/ui-snapshots"
OUT.mkdir(parents=True, exist_ok=True)
data = tempfile.TemporaryDirectory(prefix="fastapiclient-creation-")
env = dict(os.environ, DISPLAY=":110", XDG_DATA_HOME=data.name, XDG_RUNTIME_DIR=data.name,
           XDG_CACHE_HOME=data.name, LANG="C", FASTAPICLIENT_THEME="light")
env.pop("FASTAPICLIENT_FIXTURES", None)
env.pop("FASTAPICLIENT_PAGE", None)
home = Path(data.name) / "FastApiClient"
(home / "projects").mkdir(parents=True)
project = home / "projects/creation.json"
project.write_text(json.dumps({"id": "creation", "name": "Creation checks", "requests": [
    {"name": "Original", "url": "https://example.com/original"}], "folders": [
    {"name": "TopFolder"}, {"name": "NestedFolder", "parent": 0}]}))
(home / "settings.toml").write_text('last_project = "creation"\nlast_request = 0\ntheme = "light"\n')
app = None
window = None


def run(*args, check=True):
    return subprocess.run(args, env=env, text=True, capture_output=True, check=check, timeout=10)


def keys(*args):
    run("xdotool", "key", "--clearmodifiers", *args)
    time.sleep(0.15)


def copied():
    keys("ctrl+c")
    return run("xclip", "-selection", "clipboard", "-o", check=False).stdout


def type_text(text):
    run("xdotool", "type", "--clearmodifiers", "--delay", "1", text)
    time.sleep(0.15)


def words(name="creation-current"):
    time.sleep(0.15)
    path = OUT / f"{name}.png"
    pixels = subprocess.run(
        ["xwd", "-id", window, "-silent"], env=env, capture_output=True, check=True, timeout=10).stdout
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "xwd_pipe", "-i", "-", str(path)],
                   input=pixels, capture_output=True, check=True, timeout=10)
    rows = list(csv.DictReader(io.StringIO(run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout), delimiter="\t"))
    return [row for row in rows if row["text"].strip()], Image.open(path).convert("RGB")


def click(caption, button="1", menu=False):
    rows, _ = words()
    matches = [row for row in rows if row["text"] == caption]
    if menu:
        rename_y = int(next(row for row in rows if row["text"] == "Rename")["top"])
        matches = [row for row in matches if int(row["top"]) < rename_y]
    assert matches, f"Missing {caption}: {[row['text'] for row in rows]}"
    row = max(matches, key=lambda row: int(row["top"])) if menu else min(matches, key=lambda row: int(row["left"]))
    x = int(row["left"]) + int(row["width"]) // 2
    y = int(row["top"]) + int(row["height"]) // 2
    run("xdotool", "mousemove", "--window", window, str(x), str(y), "click", button)
    time.sleep(0.2)


def saved():
    time.sleep(1.1)  # include the normal autosave when comparing persisted state
    return json.loads(project.read_text())


def click_primary():
    # OCR misses white button labels on green; locate the undimmed accent rectangle instead.
    _, image = words()
    bands = []
    for y in range(image.height):
        xs = [x for x in range(image.width)
              if (c := image.getpixel((x, y)))[1] > 100 and c[0] * 2 < c[1] and c[2] * 2 < c[1]]
        if xs:
            if not bands or bands[-1][-1][0] != y - 1:
                bands.append([])
            bands[-1].append((y, xs))
    assert bands, "No primary button"
    band = max(bands, key=len)
    y, xs = band[len(band) // 2]
    run("xdotool", "mousemove", "--window", window, str((xs[0] + xs[-1]) // 2), str(y), "click", "1")
    time.sleep(0.2)


def open_create(folder, target=None):
    if target:
        click(target, "3")
    click("folder" if folder else "request", menu=bool(target))
    actual = copied()
    if actual != ("New folder" if folder else "New request"):
        words("creation-failure")
        raise AssertionError(f"Default name is not selected: {actual!r}, {folder=}, {target=}")


server = subprocess.Popen(["Xvfb", env["DISPLAY"], "-screen", "0", "1600x1000x24", "-nolisten", "tcp"], stderr=subprocess.PIPE)
try:
    time.sleep(0.5)
    assert server.poll() is None, server.stderr.read().decode()
    app = subprocess.Popen([ROOT / "target/debug/fastapiclient"], env=env, stderr=subprocess.PIPE)
    for _ in range(60):
        windows = run("xdotool", "search", "--onlyvisible", "--name", "FastApiClient", check=False).stdout.splitlines()
        if windows:
            window = windows[0]
            break
        time.sleep(0.1)
    assert window, "No app window"
    run("xdotool", "windowfocus", "--sync", window)
    # Find the URL through keyboard traversal; do not depend on field coordinates or tab order.
    for _ in range(40):
        keys("ctrl+a")
        if copied() == "https://example.com/original":
            break
        keys("Tab")
    else:
        raise AssertionError("Could not focus URL")
    type_text("https://example.com/edited")
    before = saved()
    for folder in ([] if "--menus" in sys.argv else [False, True]):
        for dismiss in ["cancel", "escape", "outside", "blank"]:
            open_create(folder)
            if dismiss == "cancel":
                click("Cancel")
            elif dismiss == "escape":
                keys("Escape")
            elif dismiss == "outside":
                rows, _ = words()
                title = next(row for row in rows if row["text"] == "New")
                run("xdotool", "mousemove", "--window", window,
                    str(int(title["left"]) // 2), title["top"], "click", "1")
            else:
                type_text("   ")
                keys("Return")
            assert saved() == before, f"{dismiss} changed the project"
            rows, image = words()
            assert not any(row["text"] == "Cancel" for row in rows), f"{folder=}, {dismiss=}: dialog did not close"
            assert any(row["text"] == "https://example.com/edited" and int(row["left"]) > image.width // 3 for row in rows), f"{folder=}, {dismiss=}: selection or editor changed"
    if "--menus" not in sys.argv:
        print("PASS: selected defaults; Cancel, Escape, outside and blank preserve items, selection and edited URL", flush=True)

    open_create(False)
    keys("Return")
    state = saved()
    assert len(state["requests"]) == 2 and state["requests"][-1]["name"] == "New request"
    assert state["requests"][0]["url"] == "https://example.com/edited"
    # Sidebar folder creation at project level: switch to Global config via Manage.
    click("Manage")
    click("Global")
    open_create(True)
    keys("Return")
    state = saved()
    assert len(state["folders"]) == 3 and state["folders"][-1]["name"] == "New folder"
    assert state["folders"][-1].get("parent") is None
    for target, index in [("TopFolder", 0), ("NestedFolder", 1)]:
        open_create(False, target)
        type_text("  CustomRequest  ")
        click_primary()
        state = saved()
        assert len(state["folders"][index]["requests"]) == 1
        assert state["folders"][index]["requests"][0]["name"] == "CustomRequest"
        open_create(True, target)
        type_text("  ChildFolder  ")
        keys("Return")
        state = saved()
        assert state["folders"][-1]["name"] == "ChildFolder" and state["folders"][-1]["parent"] == index
    click("NestedFolder", "3")
    words("creation-nested-menu")
    click("Rename")
    type_text("RenamedFolder")
    keys("Return")
    assert saved()["folders"][1]["name"] == "RenamedFolder"
    keys("F2")
    assert copied() == "RenamedFolder"
    keys("Escape")
    click("RenamedFolder", "3")
    click("Delete")
    click_primary()
    assert not any(folder["name"] == "RenamedFolder" for folder in saved()["folders"])
    print("PASS: Enter/Create, trimmed names, project/top-level/nested placement, right-click targeting, Rename/F2/Delete", flush=True)
finally:
    if app and app.poll() is None:
        app.terminate()
        app.wait(timeout=5)
        errors = app.stderr.read().decode()
        assert "panicked" not in errors and "binding loop" not in errors, errors
    server.terminate()
    server.wait(timeout=5)
    data.cleanup()
