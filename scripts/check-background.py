#!/usr/bin/env python3
"""Exercise background input against a temporary AppKit app; leave no artifacts."""

import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def wait_for(read, predicate, description):
    deadline = time.monotonic() + 3
    state = None
    while time.monotonic() < deadline:
        state = read()
        if state and predicate(state):
            return state
        time.sleep(0.02)
    panels = None if state is None else state["panels"]
    if panels:
        panels = [dict(p, events=len(p["events"])) for p in panels]
    raise AssertionError(f"{description}: {panels}")


def check(binary, root):
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
        LLAMA_CU_BACKGROUND="true",
        LLAMA_CU_SESSION="probe",
    )

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
        if before and not args[0].startswith("get-"):
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
        return body

    def state_for(index=0):
        return read()["panels"][index]

    pid = None
    try:
        details = call("get-app", app)
        pid = details["app"]["pid"]
        initial = wait_for(read, lambda s: len(s["panels"]) == 2, "probe launched")
        assert not initial["everActive"], "background launch activated the app"
        first, second = [p["window"] for p in initial["panels"]]
        call("click", "--at", "100,200", error="background_unavailable")
        call("type-text", "no window", error="background_unavailable")
        tree = call("get-ax-state", "--window", first)
        nodes = tree["nodes"]
        field = next(n for n in nodes if n["role"] == "text field")
        button = next(n for n in nodes if n.get("title") == "Greendale")
        window = next(n for n in nodes if n["role"] == "window")

        # Coordinate clicks hit a custom view, proving actual event delivery.
        buttons = [("left", [1, 2]), ("right", [3, 4]), ("middle", [25, 26])]
        for button_name, kinds in buttons:
            offset = len(state_for()["events"])
            call("click", "--at", "100,200", "--button", button_name)
            wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 2, button_name)
            events = state_for()["events"][offset:]
            assert [e["type"] for e in events] == kinds, events
            assert events[0]["point"] == [100, tree["window"]["frame"]["height"] - 200], events
        offset = len(state_for()["events"])
        call("click", "--at", "100,200", "--count", 2)
        wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 4, "double click")
        assert [e["count"] for e in state_for()["events"][offset:]] == [1, 1, 2, 2]

        offset = len(state_for()["events"])
        call("drag", "--from", "100,200", "--to", "200,300")
        wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 22, "drag")
        assert [e["type"] for e in state_for()["events"][offset:]] == [1] + [6] * 20 + [2]
        offset = len(state_for()["events"])
        call("scroll", "--at", "100,200", "--direction", "down", "--amount", 3)
        wait_for(read, lambda s: len(s["panels"][0]["events"]) >= offset + 3, "scroll")
        assert [e["type"] for e in state_for()["events"][offset:]] == [22] * 3

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

        # A second window must not accidentally receive the first window's input.
        call("get-ax-state", "--window", second)
        call("type-text", "wrong window", error="background_unavailable")
        call("click", "--at", "100,200")
        wait_for(read, lambda s: len(s["panels"][1]["events"]) == 2, "second window click")
        call("get-ax-state", "--window", first)
        call("paste", "shared clipboard", error="background_unavailable")
        call(
            "perform-secondary-action", "--element", window["id"], "raise",
            error="background_unavailable",
        )
        call("click", "--at", "900,200", error="background_unavailable")
        call(
            "drag", "--from", "100,200", "--to", "900,200",
            error="background_unavailable",
        )
        assert state_for()["text"] == "Greendale"
        assert state_for(1)["text"] == ""
        # Closing the selected window must not redirect later input.
        tree = call("get-ax-state", "--window", second)
        close = next(n for n in tree["nodes"] if n["role"] == "close button")
        call("click", "--element", close["id"])
        call("click", "--at", "100,200", error="window_not_found")
        tree = call("get-ax-state", "--window", first)
        minimize = next(n for n in tree["nodes"] if n["role"] == "minimize button")
        call("click", "--element", minimize["id"])
        call("type-text", "minimized", error="background_unavailable")
        print("PASS: background launch, AX/raw clicks, buttons, double/right/middle clicks,")
        print("      drag, scroll, Unicode typing, Shift selection, missing/minimized-window guards;")
        print("      foreground and pointer unchanged during commands.")
    finally:
        if pid is None and read():
            pid = read()["pid"]
        if pid is not None:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/llama-cu").resolve()
    with tempfile.TemporaryDirectory(prefix="llama-cu-background-") as directory:
        check(binary, Path(directory))


if __name__ == "__main__":
    main()
