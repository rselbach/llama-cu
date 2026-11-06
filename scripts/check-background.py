#!/usr/bin/env python3
"""Exercise background input in disposable native and optional Chromium apps."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

LSREGISTER = (
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/"
    "LaunchServices.framework/Support/lsregister"
)


def wait_for(read, predicate, description):
    deadline = time.monotonic() + 3
    state = None
    while time.monotonic() < deadline:
        state = read()
        if state and predicate(state):
            return state
        time.sleep(0.02)
    panels = None if state is None else state.get("panels")
    if panels:
        panels = [dict(p, events=len(p["events"])) for p in panels]
    raise AssertionError(f"{description}: {panels if panels is not None else state}")


def stop(pid):
    """Stops the probe and waits until it exits and stops writing state."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            return
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.02)
    raise RuntimeError(f"probe {pid} did not exit")


def check_electron(runtime, root, call):
    """Run real AX and pointer input against isolated Chromium web content."""
    for name in ("app", "app.asar"):
        if (runtime / "Contents/Resources" / name).exists():
            raise ValueError("--electron needs a stock Electron.app, not a packaged application")
    app = root / "GreendaleElectron.app"
    shutil.copytree(runtime, app, symlinks=True)
    plist = app / "Contents/Info.plist"
    info = plistlib.loads(plist.read_bytes())
    info.update({
        "CFBundleIdentifier": f"com.rselbach.llama-cu.web-probe.{uuid.uuid4().hex}",
        "CFBundleName": "GreendaleElectron", "CFBundleDisplayName": "GreendaleElectron",
        "LSUIElement": True,
    })
    plist.write_bytes(plistlib.dumps(info))
    resources = app / "Contents/Resources/app"
    resources.mkdir()
    shutil.copy(Path(__file__).parent / "fixtures/ElectronProbe.cjs", resources / "main.cjs")
    (resources / "package.json").write_text(json.dumps({
        "name": "greendale-probe", "version": "1.0.0", "main": "main.cjs",
    }))
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(app)], check=True)

    def read():
        path = root / "web-state.json"
        return json.loads(path.read_text()) if path.exists() else None

    def wait(predicate, description):
        state = wait_for(read, predicate, description)
        assert not state["everFocused"], "Chromium window took focus"
        return state

    process = None
    try:
        env = dict(os.environ)
        env.pop("ELECTRON_RUN_AS_NODE", None)
        with (root / "electron.log").open("w") as log:
            process = subprocess.Popen(
                [str(app / "Contents/MacOS" / info["CFBundleExecutable"])],
                env=env, stdout=log, stderr=log,
            )
        wait(lambda s: True, "Chromium loaded")
        details = call("get-app", app)
        assert details["app"]["pid"] == process.pid
        tree = call("get-ax-state")
        button = next(n for n in tree["nodes"] if n["role"] == "button"
                      and n.get("title") == "Enroll")
        call("click", "--element", button["id"], "--observe")
        wait(lambda s: s["presses"] == 1, "Chromium AX button")
        # Observe replaced the IDs. Resolve the field again before setting it.
        tree = call("get-ax-state")
        field = next(n for n in tree["nodes"] if n["role"] == "text field")
        call("set-value", "--element", field["id"], "Troy Barnes")
        wait(lambda s: s["text"] == "Troy Barnes", "Chromium AX text")
        capture = call("get-screenshot")
        assert Path(capture["path"]).read_bytes().startswith(b"\x89PNG\r\n\x1a\n")

        frame = button["frame"]
        at = f'{frame["x"] + frame["width"] / 2},{frame["y"] + frame["height"] / 2}'
        call("click", "--at", at)
        # Older Chromium versions drop inactive coordinate clicks. Report that
        # limitation separately; AX button and text actions above must work.
        time.sleep(0.3)
        state = read()
        assert not state["everFocused"], "Chromium coordinate click took focus"
        clicks = "delivered" if state["presses"] == 2 else "ignored"
        assert state["presses"] in (1, 2), state
        drag = "unavailable (coordinate clicks ignored)"
        if clicks == "delivered":
            canvas = state["canvas"]
            start = f'{canvas["x"] + 20},{canvas["y"] + 30}'
            end = f'{canvas["x"] + 160},{canvas["y"] + 30}'
            call("drag", "--from", start, "--to", end)
            state = wait(lambda s: s["downs"] > 0 and s["moves"] > 0, "Chromium drag events")
            drag = "delivered" if state["heldMoves"] > 0 else "unavailable (DOM buttons=0)"
        call("scroll", "--at", at, "--direction", "down")
        wait(lambda s: s["scroll"] > 0, "Chromium scroll")
        print(f'PASS: Electron {state["versions"]["electron"]}, Chromium '
              f'{state["versions"]["chrome"]}: AX button, AX text, screenshot, scroll; '
              f'coordinate clicks {clicks}; foreground, pointer and clipboard unchanged.')
        print(f'Chromium drag capability: {drag}.')
    except Exception:
        print((root / "electron.log").read_text(), file=sys.stderr)
        raise
    finally:
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            subprocess.run([LSREGISTER, "-u", str(app)], check=True)


def check(binary, root, electron=None):
    app = root / "GreendaleProbe.app"
    executable = app / "Contents/MacOS/Probe"
    executable.parent.mkdir(parents=True)
    subprocess.run(
        [
            "xcrun", "swiftc",
            str(Path(__file__).parent / "fixtures/BackgroundProbe.swift"),
            "-o", str(executable),
        ],
        check=True,
    )
    info = {
        "CFBundleIdentifier": f"com.rselbach.llama-cu.probe.{uuid.uuid4().hex}",
        "CFBundleName": "GreendaleProbe",
        "CFBundleExecutable": "Probe",
        "CFBundlePackageType": "APPL",
        "LSUIElement": True,
    }
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
    env = dict(
        os.environ,
        LLAMA_CU_STATE_DIR=str(root / "session"),
        LLAMA_CU_SESSION="probe",
    )
    env.pop("LLAMA_CU_BACKGROUND", None)

    def read():
        path = root / "state.json"
        return json.loads(path.read_text()) if path.exists() else None

    def call(*args, error=None):
        before = read()
        started = time.time()
        result = subprocess.run(
            [str(binary), "--json", *map(str, args)], env=env,
            text=True, capture_output=True, timeout=15,
        )
        if error:
            assert result.returncode == 1, result.stdout
            body = json.loads(result.stderr)
            assert body["error"]["code"] == error, body
        else:
            assert result.returncode == 0, result.stderr
            body = json.loads(result.stdout)
        if before:
            finished = time.time()
            after = wait_for(
                read, lambda s: s["samples"][-1]["time"] > finished + 0.05,
                "probe is live",
            )
            assert not after["everActive"], "probe became the active app"
            baseline = before["samples"][-1]
            samples = [s for s in after["samples"] if s["time"] >= started]
            assert samples, "no input monitor samples"
            assert all(s["front"] == baseline["front"] for s in samples), (
                args, "foreground changed", baseline["front"],
                sorted({s["front"] for s in samples}),
            )
            assert all(s["pointer"] == baseline["pointer"] for s in samples), (
                args, "pointer moved", baseline["pointer"],
                sorted({tuple(s["pointer"]) for s in samples}),
            )
            assert all(s["clipboard"] == baseline["clipboard"] for s in samples), (
                args, "clipboard changed",
            )
        return body

    def state_for(index=0):
        return read()["panels"][index]

    pid = None
    try:
        details = call("get-app", app)
        pid = details["app"]["pid"]
        initial = wait_for(read, lambda s: len(s["panels"]) == 2, "probe launched")
        assert not initial["everActive"], "background launch activated the app"
        if electron:
            check_electron(electron, root, call)
            return
        first, second = [p["window"] for p in initial["panels"]]
        call("click", "--at", "100,200", error="background_unavailable")
        call("type-text", "no window", error="background_unavailable")
        tree = call("get-ax-state", "--window", first)
        capture = call("get-screenshot")
        assert Path(capture["path"]).read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
        nodes = tree["nodes"]
        field = next(n for n in nodes if n["role"] == "text field")
        button = next(n for n in nodes if n.get("title") == "Greendale")
        window = next(n for n in nodes if n["role"] == "window")

        # Coordinate clicks hit a view that accepts the first mouse, so the
        # check sees each delivered event.
        height = tree["window"]["frame"]["height"]
        buttons = [("left", [1, 2]), ("right", [3, 4]), ("middle", [25, 26])]
        for button_name, kinds in buttons:
            offset = len(state_for()["events"])
            call("click", "--at", "100,200", "--button", button_name)
            wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 2, button_name)
            events = state_for()["events"][offset:]
            assert [e["type"] for e in events] == kinds, events
            assert events[0]["point"] == [100, height - 200], events
            assert [e["pressure"] for e in events] == [1, 0], events
        # The agent cursor shows the click, stacked right above its window.
        origin = state_for()["origin"]
        wait_for(
            read,
            lambda s: s["cursor"]
            and s["cursor"]["below"] == first
            and s["cursor"]["tip"] == [origin[0] + 100, origin[1] + 200],
            "cursor at the click",
        )
        offset = len(state_for()["events"])
        call("click", "--at", "100,200", "--count", 2)
        wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 4, "double click")
        assert [e["count"] for e in state_for()["events"][offset:]] == [1, 1, 2, 2]

        offset = len(state_for()["events"])
        call("drag", "--from", "100,200", "--to", "200,300")
        wait_for(read, lambda s: s["panels"][0]["events"][-1]["type"] == 2, "drag")
        # AppKit merges drag events an app has not handled yet, as it does
        # for a real mouse, so only the press, release, and path are fixed.
        events = state_for()["events"][offset:]
        types = [e["type"] for e in events]
        assert types[0] == 1 and types[-1] == 2 and set(types[1:-1]) == {6}, types
        assert events[-2]["point"] == events[-1]["point"] == [200, height - 300], events
        # A real scroll view starts at its top left, so scrolling the wrong
        # way leaves it there.
        area = next(n for n in nodes if n["role"] == "scroll area")["frame"]
        scroll_at = f'{area["x"] + area["width"] / 2},{area["y"] + area["height"] / 2}'
        call("scroll", "--at", scroll_at, "--direction", "down", "--amount", 3)
        wait_for(read, lambda s: s["panels"][0]["scroll"][1] > 0, "scroll down")
        call("scroll", "--at", scroll_at, "--direction", "right", "--amount", 3)
        wait_for(read, lambda s: s["panels"][0]["scroll"][0] > 0, "scroll right")

        # Standard views decline the first mouse, so an inactive app drops
        # their left clicks. The README documents this limitation.
        call("click", "--at", f"230,{height - 305}")
        time.sleep(0.3)
        assert state_for()["plainClicks"] == 0, "standard view took a background click"

        # Both AX actions and raw clicks work on real AppKit controls.
        call("click", "--element", button["id"])
        wait_for(read, lambda s: s["panels"][0]["presses"] == 1, "AX button")
        frame = button["frame"]
        center = f'{frame["x"] + frame["width"] / 2},{frame["y"] + frame["height"] / 2}'
        call("click", "--at", center)
        wait_for(read, lambda s: s["panels"][0]["presses"] == 2, "coordinate button")
        call("click", "--element", field["id"])
        call("type-text", "Troy Barnes 👩🏽‍💻")
        wait_for(read, lambda s: s["panels"][0]["text"] == "Troy Barnes 👩🏽‍💻", "Unicode typing")
        call("press-key", "cmd+a", error="background_unavailable")
        call("press-key", "shift+left", "backspace")
        call("type-text", "!")
        wait_for(read, lambda s: s["panels"][0]["text"] == "Troy Barnes !", "Shift selection")
        call("set-value", "--element", field["id"], "Greendale")
        wait_for(read, lambda s: s["panels"][0]["text"] == "Greendale", "AX value")

        # Typing stops when Return moves the keyboard focus to a new window.
        call("type-text", "\nTroy Barnes", error="background_unavailable")
        assert state_for()["dialog"], "Return did not open the dialog"

        # A second window must not accidentally receive the first window's input.
        call("get-ax-state", "--window", second)
        call("type-text", "wrong window", error="background_unavailable")
        # Observing shows the window that took the input, not the focused one.
        scrolled = call("scroll", "--direction", "down", "--observe")
        assert scrolled["observed"]["state"]["window"]["id"] == second, scrolled["observed"]
        offset = len(state_for(1)["events"])
        call("click", "--at", "100,200")
        wait_for(read, lambda s: len(s["panels"][1]["events"]) == offset + 2, "second window click")
        wait_for(read, lambda s: s["cursor"] and s["cursor"]["below"] == second, "cursor moved")
        call("get-ax-state", "--window", first)
        call("paste", "shared clipboard", error="background_unavailable")
        for raise_name in ["raise", "AXRaise", "axraise"]:
            call(
                "perform-secondary-action", "--element", window["id"], raise_name,
                error="background_unavailable",
            )
        call("click", "--at", "900,200", error="background_unavailable")
        call(
            "drag", "--from", "100,200", "--to", "900,200",
            error="background_unavailable",
        )
        assert state_for()["text"] == "Greendale"
        assert state_for(1)["text"] == ""
        # Hidden apps do not handle background events, so input fails.
        os.kill(pid, signal.SIGUSR1)
        wait_for(read, lambda s: s["hidden"], "probe hidden")
        wait_for(read, lambda s: not s["cursor"], "cursor hidden with the app")
        call("click", "--at", "100,200", error="background_unavailable")
        call("type-text", "hidden", error="background_unavailable")
        os.kill(pid, signal.SIGUSR2)
        wait_for(read, lambda s: not s["hidden"], "probe shown")
        offset = len(state_for()["events"])
        call("click", "--at", "100,200")
        wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 2, "shown click")
        # Closing the selected window must not redirect later input.
        tree = call("get-ax-state", "--window", second)
        close = next(n for n in tree["nodes"] if n["role"] == "close button")
        closed = call("click", "--element", close["id"], "--observe")
        assert closed["observeError"]["code"] == "window_not_found", closed
        call("click", "--at", "100,200", error="window_not_found")
        call("get-ax-state", error="window_not_found")
        call("get-screenshot", error="window_not_found")
        call("type-text", "closed window", error="window_not_found")
        tree = call("get-ax-state", "--window", first)
        minimize = next(n for n in tree["nodes"] if n["role"] == "minimize button")
        call("click", "--element", minimize["id"])
        call("type-text", "minimized", error="background_unavailable")
        print("PASS: background launch, AX/raw clicks, buttons, double/right/middle clicks,")
        print("      pressure, drag, scroll direction, Unicode typing, Shift selection,")
        print("      focus loss while typing, raise and missing/hidden/minimized-window")
        print("      guards, agent cursor placement; standard views drop background left")
        print("      clicks; foreground, pointer and clipboard unchanged during commands.")
    finally:
        if pid is None and read():
            pid = read()["pid"]
        if pid is not None:
            stop(pid)
            # Launching registered the bundle; forget it before it is deleted.
            subprocess.run([LSREGISTER, "-u", str(app)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="target/debug/llama-cu", type=Path)
    parser.add_argument("--electron", type=Path, help="check Chromium instead, using a copy of this stock Electron.app")
    args = parser.parse_args()
    binary = args.binary.resolve()
    doctor = subprocess.run([str(binary), "--json", "doctor"], text=True,
                            capture_output=True, check=True)
    permissions = json.loads(doctor.stdout)
    if not permissions["accessibility"] or not permissions["screenRecording"]:
        sys.exit("Accessibility and Screen Recording are required; run llama-cu doctor.")
    with tempfile.TemporaryDirectory(prefix="llama-cu-background-") as directory:
        check(binary, Path(directory), args.electron.resolve() if args.electron else None)


if __name__ == "__main__":
    main()
