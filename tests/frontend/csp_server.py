#!/usr/bin/env python3
"""Serves app/ui with the exact Content-Security-Policy header tauri.conf.json
ships, so the Playwright suite validates layout under the same policy the
built app enforces -- not under no policy at all (plain `http.server` sends
no CSP header, which is what let a CSP-breaking regression through green).

The policy string lives in exactly one place, app/src-tauri/tauri.conf.json;
this reads it rather than duplicating it, so the two cannot drift.
"""
import functools
import http.server
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2] / "app" / "ui"
CONF = pathlib.Path(__file__).resolve().parents[2] / "app" / "src-tauri" / "tauri.conf.json"
CSP = json.loads(CONF.read_text())["app"]["security"]["csp"]


class CspHandler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Content-Security-Policy", CSP)
        super().end_headers()

    def log_message(self, fmt, *args):
        # Keep test output readable; Playwright already reports pass/fail.
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    handler = functools.partial(CspHandler, directory=str(ROOT))
    http.server.ThreadingHTTPServer(("127.0.0.1", port), handler).serve_forever()
