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

    def log_request(self, code="-", size="-"):
        # The ONE exception to the silence above: a non-2xx is the thing that
        # makes Playwright's readiness poll spin until it times out, and
        # `log_message` being a no-op is what hid it. 177 passing tests stay
        # quiet; a 404 says so.
        try:
            failed = int(code) >= 400
        except (TypeError, ValueError):
            failed = True
        if failed:
            print(
                f"csp_server: {self.requestline} -> {code}",
                file=sys.stderr,
                flush=True,
            )


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    handler = functools.partial(CspHandler, directory=str(ROOT))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    # Say what was bound and what is being served, BEFORE serving. Playwright
    # waits for a 2xx from `/index.html` and treats anything else -- a 404
    # included -- as "not ready yet", so a server that starts perfectly while
    # serving the wrong directory is indistinguishable from one that never
    # started: both are a silent 60s timeout with no stderr. This line is what
    # tells those two apart. `flush` because stdout is a pipe here, not a tty.
    print(
        f"csp_server: listening on 127.0.0.1:{port} serving {ROOT} "
        f"(index.html present: {(ROOT / 'index.html').is_file()})",
        flush=True,
    )
    server.serve_forever()
