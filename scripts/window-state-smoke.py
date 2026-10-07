#!/usr/bin/env python3
"""Exercise native geometry across processes; requires an unlocked desktop session.

First build: cargo build --locked -p solador-app --example window_state_smoke
This harness uses the production registration with a blank webview, no credentials.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
(root / "tmp").mkdir(exist_ok=True)
exe = root / "target" / "debug" / "examples" / ("window_state_smoke.exe" if os.name == "nt" else "window_state_smoke")
with tempfile.TemporaryDirectory(prefix="window-state-", dir=root / "tmp") as directory:
    scratch = Path(directory)
    env = dict(os.environ, HOME=directory, APPDATA=directory, XDG_CONFIG_HOME=directory,
               SOLADOR_WINDOW_SMOKE_DIR=directory)
    def launch(mode, *args):
        subprocess.run([str(exe), mode, *map(str, args)], env=env, check=True, timeout=30)
        return json.loads((scratch / f"{mode}.json").read_text())

    defaults = launch("read")
    assert defaults["visible"], "first launch must be visible"
    expected = launch("write")
    restored = launch("read")
    assert restored == expected, f"window geometry did not survive restart: {expected} -> {restored}"
    assert restored["visible"], "reopened window must be visible"
    launch("minimize")
    assert launch("read") == expected, "minimizing must preserve normal bounds and reopen restored"
    maximized = launch("maximize")
    assert maximized["maximized"], "native window did not maximize"
    assert launch("read")["maximized"], "maximized state must survive restart"
    assert launch("unmaximize") == expected, "unmaximizing after restart must restore normal bounds"
    assert launch("read") == expected, "unmaximized bounds must survive restart"
    state = list(scratch.rglob(".window-state.json"))
    assert len(state) == 1, f"expected one isolated persistence file: {state}"
    saved = json.loads(state[0].read_text())
    saved["main"].update(x=1000000, y=1000000, visible=False, maximized=False)
    state[0].write_text(json.dumps(saved))
    recovered = launch("read")
    assert recovered["visible"], "saved hidden state must not hide the next launch"
    assert recovered["x"] != 1000000 and recovered["y"] != 1000000, "disconnected monitor placement must be ignored"
    state[0].write_text("{ invalid JSON")
    fallback = launch("read")
    assert fallback["visible"]
    assert (fallback["width"], fallback["height"]) == (defaults["width"], defaults["height"]), "corrupt state must restore configured size"
    monitors = json.loads((scratch / "monitors.json").read_text())
    for index, monitor in enumerate(monitors):
        expected = launch("write", index, "quit")
        for restart in range(3):
            restored = launch("read")
            assert restored == expected, f"monitor {index} ({monitor}), restart {restart}: {expected} -> {restored}"
        maximized = launch("maximize")
        assert launch("read") == maximized, f"maximized bounds must reopen on monitor {index}"
        assert launch("unmaximize") == expected, f"normal bounds must survive maximized restart on monitor {index}"
    print(f"PASS: three restarts on each of {len(monitors)} connected displays preserve bounds and scale")
    print("PASS: real native size/position and maximize survive restart; minimize preserves bounds; off-screen/hidden and corrupt state recover visibly")
