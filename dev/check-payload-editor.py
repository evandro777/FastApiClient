"""Private-Xvfb keyboard checks; debug build, xdotool, xclip, xwd, ffmpeg and tesseract required."""
import csv
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
data = tempfile.TemporaryDirectory(prefix="fastapiclient-editor-")
env = dict(os.environ, DISPLAY=":108", XDG_DATA_HOME=data.name, LANG="C", RUST_BACKTRACE="1", FASTAPICLIENT_FIXTURES="single", FASTAPICLIENT_PAGE="validation-json", FASTAPICLIENT_THEME="light")

def run(*args, check=True):
    return subprocess.run(args, env=env, text=True, capture_output=True, check=check, timeout=10)

def keys(*args):
    run("xdotool", "key", "--clearmodifiers", *args)
    time.sleep(0.2)

def copied():
    keys("ctrl+c")
    return run("xclip", "-selection", "clipboard", "-o", check=False).stdout

def paste(text, shortcut="ctrl+v"):
    subprocess.run(["xclip", "-selection", "clipboard"], input=text, env=env, text=True, check=True, timeout=10)
    keys(shortcut)
    time.sleep(0.15)  # X11 clipboard delivery completes asynchronously, especially for large selections.

def body():
    keys("ctrl+a")
    return copied()

def screenshot(name, settle=0.35):
    time.sleep(settle)  # let the debounced validation and layout settle
    path = ROOT / "target/ui-snapshots" / f"editor-{name}.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    pixels = subprocess.run(["xwd", "-id", windows[0], "-silent"], env=env, capture_output=True, check=True, timeout=10).stdout
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "xwd_pipe", "-i", "-", str(path)], input=pixels, capture_output=True, check=True, timeout=10)
    return path

def words(path):
    result = run("tesseract", str(path), "stdout", "--psm", "11", "tsv").stdout
    return list(csv.DictReader(io.StringIO(result), delimiter="\t"))

def click_caption(caption, first=False):
    matches = [word for word in words(screenshot("controls")) if word["text"] == caption]
    assert matches, f"Could not find {caption!r} in the app window"
    word = matches[0] if first else matches[-1]
    x = int(word["left"]) + int(word["width"]) // 2
    y = int(word["top"]) + int(word["height"]) // 2
    run("xdotool", "mousemove", "--window", windows[0], str(x), str(y), "click", "1")
    time.sleep(0.2)

def format_body():
    click_caption("Format")

server = subprocess.Popen(["Xvfb", ":108", "-screen", "0", "1280x900x24", "-nolisten", "tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
app = None
try:
    time.sleep(0.7)
    assert server.poll() is None, server.stderr.read().decode()
    app = subprocess.Popen([ROOT / "target/debug/fastapiclient"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    for _ in range(50):
        windows = run("xdotool", "search", "--onlyvisible", "--name", "FastApiClient", check=False).stdout.splitlines()
        if windows:
            break
        time.sleep(0.1)
    assert windows, "No app window"
    run("xdotool", "windowfocus", "--sync", windows[0])
    source = '{\n    // A comment is accepted\n    "name": "Ana"\n}'
    for _ in range(45):
        keys("ctrl+a")
        if copied() == source:
            break
        keys("Tab")
    else:
        raise AssertionError("Could not focus payload using keyboard traversal")
    indented = "\n".join("    " + line for line in source.split("\n"))
    keys("Tab")
    assert copied() == indented[4:], repr(copied())
    keys("ctrl+z")
    assert copied() == source, "Undo did not restore original selection"
    keys("Tab", "ctrl+a")
    assert copied() == indented
    keys("shift+Tab", "ctrl+a")
    unindented = copied()
    assert unindented == source, f"Shift+Tab did not unindent selected lines: {unindented!r}"
    keys("ctrl+z", "ctrl+a")
    assert copied() == indented, "Unindent undo failed"
    keys("shift+Tab", "ctrl+End", "ctrl+shift+Home", "Tab")
    assert copied() == indented[4:], "Reversed selection was not preserved"
    keys("ctrl+z")
    assert copied() == source
    keys("Escape")
    keys("shift+Tab")
    keys("Tab", "ctrl+a")
    focus_text = copied()
    assert focus_text == source, f"Leaving and returning to the editor changed its text: {focus_text!r}"
    keys("ctrl+a")
    run("xdotool", "type", "--clearmodifiers", "--delay", "40", "{{ho")
    time.sleep(0.2)
    keys("Return", "ctrl+a")
    assert copied() == "{{host}}", "Variable completion failed"
    keys("ctrl+z", "ctrl+a")
    assert copied() == "{{ho", "Variable completion undo failed"
    keys("Escape")  # dismiss suggestions before the paste checks

    compact = '{"z":1e+02,"a":[1,2]}'
    formatted = '{\n    "z": 1e+02,\n    "a": [\n        1,\n        2\n    ]\n}'
    keys("ctrl+a")
    paste(compact)
    assert body() == compact, "Ctrl+V changed pasted text"
    keys("ctrl+End", "ctrl+shift+Home")
    format_body()
    assert body() == formatted, "Manual JSON formatting did not use four spaces"
    keys("ctrl+z")
    assert copied() == compact, "Format undo did not restore text and selection"
    keys("shift+Right")
    assert copied() == compact[1:], "Format undo lost reversed selection direction"
    keys("ctrl+a")
    keys("BackSpace")
    paste(compact, "shift+Insert")
    actual = body()
    assert actual == compact, f"Shift+Insert changed pasted text: {actual!r}"
    keys("ctrl+z")
    run("xdotool", "type", "--clearmodifiers", "x")
    assert body() == "x", "Empty-body paste undo failed"
    keys("BackSpace")
    paste("{}")
    keys("Left")
    paste(compact)
    assert body() == "{" + compact + "}", "A fragment paste was formatted"
    keys("ctrl+a", "BackSpace")
    paste("{}")
    keys("Left", "Return")
    assert body() == "{\n    \n}", "Enter did not split matching delimiters"
    keys("ctrl+z")
    keys("shift+Right")
    assert copied() == "}", "Enter undo did not restore the caret"
    keys("ctrl+a", "BackSpace")
    paste("    abc")  # invalid JSON: preserve the source, including spaces
    keys("End", "Return")
    actual = body()
    assert actual == "    abc\n    ", f"Enter lost the current indentation: {actual!r}"
    for unsafe in ['{"x":}', '{"x":"{{host}}"}', '{"x":"Ana”\n}']:
        paste(unsafe)
        assert body() == unsafe, "Invalid/template paste was changed"
        format_body()
        assert body() == unsafe, "Format changed invalid/template text"
    # A no-op Format must not replace the existing formatting undo entry.
    paste(compact)
    format_body()
    format_body()
    keys("ctrl+z")
    assert body() == compact, "No-op Format replaced the undo entry"
    print("PASS: verbatim keyboard paste, manual JSON Format, undo and selection restoration", flush=True)
    keys("ctrl+a")
    long = '{"SCROLLSTART":"' + "m" * 180 + 'SCROLLEND"}'
    paste(long)
    format_body()
    keys("ctrl+Home", "Down", "Home")
    start_image = screenshot("scroll-start")
    start_words = words(start_image)
    assert any("SCROLLSTART" in w["text"] for w in start_words), "Start of long line is not visible"
    keys("End")
    end_image = screenshot("scroll-end")
    end_words = words(end_image)
    assert any("SCROLLEND" in w["text"] for w in end_words), "Caret did not scroll the long line horizontally"
    # Number 2 stays in place while the long second line scrolls. No pointer coordinates are used.
    gutter_start = [(w["left"], w["top"]) for w in start_words if w["text"] == "2"]
    gutter_end = [(w["left"], w["top"]) for w in end_words if w["text"] == "2"]
    assert any(x == ex and abs(int(y) - int(ey)) <= 5 for x, y in gutter_start for ex, ey in gutter_end), "Line-number gutter moved with horizontal scrolling"
    keys("ctrl+a")
    assert copied() == '{\n    "SCROLLSTART": "' + "m" * 180 + 'SCROLLEND"\n}', "Gutter numbers leaked into copied text"
    click_caption("JSON", first=True)
    keys("Down", "Return")
    format_body()  # return focus to the editor
    keys("ctrl+a")
    paste("<root><a> A &amp; B </a><b/></root>")
    xml = "<root>\n    <a> A &amp; B </a>\n    <b/>\n</root>"
    assert body() == "<root><a> A &amp; B </a><b/></root>", "XML paste changed text"
    format_body()
    actual = body()
    assert actual == xml, f"Manual XML formatting failed: {actual!r}"
    print("PASS: explicit XML Format", flush=True)
    for unsafe in ["<p>hi <b>x</b>!</p>", "<root xml:space='preserve'><a/></root>", "<!DOCTYPE a><a/>", "<a>{{host}}</a>", "<a>"]:
        paste(unsafe, "shift+Insert")
        actual = body()
        assert actual == unsafe, f"Unsafe XML paste was changed: {actual!r}"
        format_body()
        assert body() == unsafe, "Format changed unsafe XML"
    paste("<a></a>")
    keys("Home", "Right", "Right", "Right", "Return")
    assert body() == "<a>\n    \n</a>", "XML Enter did not split matching elements"
    screenshot("xml-enter")
    print("PASS: native editing, formatting, undo, completion and horizontal scrolling", flush=True)
    paste("bad\n" * 99)  # invalid XML stays verbatim, including the empty hundredth line
    keys("ctrl+End")
    bottom = screenshot("gutter-100")
    assert any(w["text"] == "100" for w in words(bottom)), "Trailing line 100 did not scroll into view"
    assert body() == "bad\n" * 99, "Trailing empty line or body text was lost"
    # Literal tabs, multibyte text and combining marks must survive both color and native-selection layers.
    unicode_xml = '<r>\n\t<a n="猫">na\u0303o &amp; café</a>\n</r>'
    paste(unicode_xml, "shift+Insert")
    assert body() == unicode_xml, "Paste changed Unicode or literal tabs"
    paste(unicode_xml.replace("<r>", "<r>{{host}}"))  # templates bypass formatting
    assert body() == unicode_xml.replace("<r>", "<r>{{host}}")
    keys("ctrl+Home", "Right")
    screenshot("highlight-unicode")
    print("PASS: Unicode and literal-tab selection", flush=True)

    # Above the shared cap, manual formatting must leave the whole payload intact.
    oversized = '<r><a/>' + ' ' * 65_536 + '</r>'
    keys("ctrl+a")
    paste(oversized)
    assert body() == oversized, "Oversized whole-body paste was formatted or truncated"
    format_body()
    assert body() == oversized, "Format changed an oversized body"
    keys("ctrl+Home")
    screenshot("highlight-oversized")
    keys("ctrl+a")
    paste('<r><a x="last">last</a></r>')
    format_body()
    actual = body()
    assert actual == '<r>\n    <a x="last">last</a>\n</r>', f"Colors did not recover after a large payload: {actual[:100]!r}"
    keys("ctrl+Home", "Right")
    screenshot("highlight-recovered")
    print("PASS: oversized paste stays verbatim; small-payload formatting recovers", flush=True)

    # Traverse into the read-only response and verify copying and Find still use the source text.
    keys("Escape", "Tab")
    for _ in range(45):
        keys("ctrl+a")
        response = copied()
        if '"id": 1' in response and '"address"' in response:
            break
        keys("Tab")
    else:
        raise AssertionError("Could not select the response through keyboard traversal")
    assert 'não' in response and '"name": "Ana"' in response
    def click_caption(caption):
        matches = [word for word in words(screenshot("response-controls")) if word["text"] == caption]
        assert matches, f"Could not find {caption!r} in the app window"
        word = matches[-1]
        x = int(word["left"]) + int(word["width"]) // 2
        y = int(word["top"]) + int(word["height"]) // 2
        run("xdotool", "mousemove", "--window", windows[0], str(x), str(y), "click", "1")
    click_caption("Beautified")
    keys("Up", "Return")
    click_caption("Copy")
    raw = run("xclip", "-selection", "clipboard", "-o").stdout
    assert raw.startswith('{"id":1,') and r'\u00e3' in raw and '\n' not in raw, f"Raw changed the response text: {raw[:100]!r}"
    assert json.loads(raw) == json.loads(response), "Raw and Beautified changed response values"
    screenshot("response-raw")
    click_caption("Raw")
    keys("Down", "Return")
    click_caption("Copy")
    assert run("xclip", "-selection", "clipboard", "-o").stdout == response, "Cached Beautified changed the shown text"
    print("PASS: Raw preserves Unicode escapes; cached Beautified and toolbar Copy agree", flush=True)
    # Return focus to the response so Ctrl+F searches the shown body.
    for _ in range(12):
        keys("Tab", "ctrl+a")
        if copied() == response:
            break
    else:
        raise AssertionError("Could not focus the response after changing modes")
    keys("ctrl+f", "ctrl+a")
    paste("Recife")
    subprocess.run(["xclip", "-selection", "clipboard"], input="__find_probe__", env=env, text=True, check=True, timeout=10)
    for _ in range(12):
        keys("Tab")
        if copied() == "Recife":
            break
    else:
        raise AssertionError("Find did not select the matching response bytes after Unicode text")
    screenshot("highlight-find")
    keys("ctrl+f", "Escape")
    # During continuous typing the unchanged sentinel must keep its blue syntax color in every frame.
    for _ in range(45):
        keys("Tab", "ctrl+a")
        if '<a x="last">' in copied():
            break
    else:
        raise AssertionError("Could not return to the request editor")
    seed = '<r steady="COLOR_SENTINEL" typed=""/>'
    paste(seed)
    keys("ctrl+Home", "End", "Left", "Left", "Left")
    baseline = screenshot("typing-before")
    sentinel = next(w for w in words(baseline) if "SENTINEL" in w["text"])
    crop = ':'.join(sentinel[key] for key in ["width", "height", "left", "top"])
    def blue_pixels(path):
        pixels = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", str(path), "-vf", f"crop={crop}", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"], env=env, capture_output=True, check=True, timeout=10).stdout
        return sum(b > r + 20 for r, g, b in zip(pixels[::3], pixels[1::3], pixels[2::3]))
    colored = blue_pixels(baseline)
    assert colored > 20, "Could not identify colored sentinel text"
    typed = "abcdefghijklmnopqrstuvxyz" * 4
    typing = subprocess.Popen(["xdotool", "type", "--clearmodifiers", "--delay", "40", typed], env=env)
    try:
        for frame in range(5):
            assert typing.poll() is None, "Typing finished before the sampling check"
            current = screenshot(f"typing-during-{frame}", settle=0)
            assert blue_pixels(current) >= colored // 2, "Unchanged syntax colors flashed off while typing"
        typing.wait(timeout=10)
    finally:
        if typing.poll() is None:
            typing.terminate()
            typing.wait(timeout=5)
    assert body() == seed.replace('typed=""', f'typed="{typed}"'), "Color updates changed typed text"
    print("PASS: unchanged syntax colors stay visible throughout continuous typing", flush=True)
    print("PASS: editing, paste/undo, gutter, scrolling, completion, Unicode, size fallback, response Copy/Find")
finally:
    if app:
        app.terminate()
        app.wait(timeout=5)
        errors = app.stderr.read().decode()
        if errors:
            print(errors)
    server.terminate()
    server.wait(timeout=5)
    data.cleanup()
