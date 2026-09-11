#!/usr/bin/env python3
"""Serves app/ui with the exact Content-Security-Policy header tauri.conf.json
ships, so the Playwright suite validates layout under the same policy the
built app enforces -- not under no policy at all (plain `http.server` sends
no CSP header, which is what let a CSP-breaking regression through green).

The policy string lives in exactly one place, app/src-tauri/tauri.conf.json;
this reads it rather than duplicating it, so the two cannot drift.
"""
import time

# Taken before the imports below, which are part of startup, so the two
# lines this prints on the way up can say how long each phase took. Process
# spawn and interpreter boot are outside the number (45ms on the hosted
# runner, measured). That interval is what Playwright's 60s webServer
# deadline is spent against; printing it on every run is what makes a
# creep-back diagnosable from any green run's `[WebServer]` line, and the
# pre-bind line is what names the phase reached if a run times out.
STARTED = time.perf_counter()

import functools
import http.server
import json
import pathlib
import socketserver
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


class LoopbackServer(http.server.ThreadingHTTPServer):
    """ThreadingHTTPServer minus the reverse-DNS lookup the stock
    `HTTPServer.server_bind` performs on the bind address.

    The stdlib sets `server_name = socket.getfqdn(host)` after binding. On
    GitHub's hosted macOS runners that PTR query for 1.0.0.127.in-addr.arpa
    goes to the VM's NAT resolver, which never answers, and the call returns
    only when the resolver gives up: 35.0s of a 36.6s readiness window,
    measured in place (#401), and reproduced four times in isolation against
    0.06s for the same bind without the lookup. `socket.getfqdn()` of the
    runner's own hostname took 70s on the same resolver, which is the shape
    of the two 60s webServer timeouts that opened the issue. Nothing this
    server uses reads `server_name` -- in the stdlib only the CGI handler and
    `wsgiref.simple_server` do -- so the address is recorded as given and
    nothing is resolved. csp_server_test.py asserts exactly that, because a
    `super().server_bind()` "cleanup" here would be green on every laptop
    and put the 35s back on the runner.
    """

    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        host, port = self.server_address[:2]
        self.server_name = host
        self.server_port = port


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    handler = functools.partial(CspHandler, directory=str(ROOT))
    # Said BEFORE binding, so a run that times out inside the bind still
    # names the phase it reached -- before this line existed, a stall there
    # produced zero server-side output, indistinguishable from a process
    # that never started.
    print(
        f"csp_server: binding 127.0.0.1:{port} "
        f"(imports done {time.perf_counter() - STARTED:.2f}s after script start)",
        flush=True,
    )
    server = LoopbackServer(("127.0.0.1", port), handler)
    # Say what was bound and what is being served, BEFORE serving. Playwright
    # waits for a 2xx from `/index.html` and treats anything else -- a 404
    # included -- as "not ready yet", so a server that starts perfectly while
    # serving the wrong directory is indistinguishable from one that never
    # started: both are a silent 60s timeout with no stderr. This line is what
    # tells those two apart. `flush` because stdout is a pipe here, not a tty.
    print(
        f"csp_server: listening on 127.0.0.1:{port} serving {ROOT} "
        f"(index.html present: {(ROOT / 'index.html').is_file()}, "
        f"ready {time.perf_counter() - STARTED:.2f}s after script start)",
        flush=True,
    )
    server.serve_forever()
