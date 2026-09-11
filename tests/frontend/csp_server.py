#!/usr/bin/env python3
"""Serves app/ui with the exact Content-Security-Policy header tauri.conf.json
ships, so the Playwright suite validates layout under the same policy the
built app enforces -- not under no policy at all (plain `http.server` sends
no CSP header, which is what let a CSP-breaking regression through green).

The policy string lives in exactly one place, app/src-tauri/tauri.conf.json;
this reads it rather than duplicating it, so the two cannot drift.
"""
# INSTRUMENTATION (#401, temporary): phase timestamps to stderr. `sys` and
# `time` are frozen/builtin modules, so the first mark lands before any
# stdlib import that could be the slow phase.
import sys
import time

_P0 = time.perf_counter()


def _phase(name):
    print(
        f"csp_server: phase [{name}] wall={time.time():.3f} +{time.perf_counter() - _P0:.3f}s",
        file=sys.stderr,
        flush=True,
    )


_phase("interpreter up; stdlib imports next")
import functools  # noqa: E402
import http.server  # noqa: E402
import json  # noqa: E402
import pathlib  # noqa: E402
import socket  # noqa: E402
import socketserver  # noqa: E402

_phase("stdlib imported; reading tauri.conf.json next")

ROOT = pathlib.Path(__file__).resolve().parents[2] / "app" / "ui"
CONF = pathlib.Path(__file__).resolve().parents[2] / "app" / "src-tauri" / "tauri.conf.json"
CSP = json.loads(CONF.read_text())["app"]["security"]["csp"]

_phase("csp read")


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


class _PhasedServer(http.server.ThreadingHTTPServer):
    # INSTRUMENTATION (#401, temporary): a verbatim copy of
    # HTTPServer.server_bind with a mark between the socket bind and the
    # reverse-DNS lookup it performs, so the two are timed apart.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        _phase("socket bound; getfqdn next")
        host, port = self.server_address[:2]
        self.server_name = socket.getfqdn(host)
        _phase(f"getfqdn done -> {self.server_name!r}")
        self.server_port = port


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    handler = functools.partial(CspHandler, directory=str(ROOT))
    _phase("binding next")
    server = _PhasedServer(("127.0.0.1", port), handler)
    _phase("listening")
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
