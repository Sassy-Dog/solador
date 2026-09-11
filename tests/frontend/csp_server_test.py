#!/usr/bin/env python3
"""The one property of csp_server.py the Playwright suite cannot observe: the
bind resolves no name.

`LoopbackServer.server_bind` exists to skip the `socket.getfqdn()` the stdlib
performs on the bind address, which cost 35s of a 36.6s readiness window on
GitHub's hosted macOS runners (#401). That is invisible on a laptop, where
the lookup answers in milliseconds, so a `super().server_bind()` "cleanup"
would pass every local run and put the 35s back in CI. This asserts the
behaviour rather than the timing: every resolver entry point a reverse lookup
could take is patched to raise, and the server must still bind.

Dependency-free (stdlib `unittest`), the `agent/deploy/lib_test.sh` precedent:

    python3 -m unittest -v csp_server_test      # from tests/frontend
"""
import http.server
import socket
import unittest
from unittest import mock

import csp_server

# The reverse-lookup entry points. `getfqdn` is what HTTPServer.server_bind
# calls; the rest are what a rewrite of that line might reach for instead.
RESOLVERS = ("getfqdn", "gethostbyaddr", "getnameinfo", "gethostbyname")


class _NameLookup(AssertionError):
    pass


def _forbid_name_lookups():
    def refuse(name):
        def _refuse(*args, **kwargs):
            raise _NameLookup(f"socket.{name}{args} during bind")

        return _refuse

    patches = [mock.patch.object(socket, name, refuse(name)) for name in RESOLVERS]
    for patch in patches:
        patch.start()
    return patches


class BindResolvesNoName(unittest.TestCase):
    def setUp(self):
        self._patches = _forbid_name_lookups()

    def tearDown(self):
        for patch in self._patches:
            patch.stop()

    def test_loopback_server_binds_without_a_name_lookup(self):
        server = csp_server.LoopbackServer(("127.0.0.1", 0), csp_server.CspHandler)
        try:
            # Recorded as given: the literal address, and the port the kernel
            # actually handed out for `0` -- the stdlib sets `server_port`
            # in the same override this replaces, so it is asserted too.
            self.assertEqual(server.server_name, "127.0.0.1")
            self.assertEqual(server.server_port, server.server_address[1])
            self.assertNotEqual(server.server_port, 0)
        finally:
            server.server_close()

    def test_the_stock_server_would_have_looked_the_name_up(self):
        # Negative control: proves the patches above sit on the path the
        # stdlib takes, so the test above passes for the right reason. The
        # stdlib's own `TCPServer.__init__` closes the socket on the way out.
        with self.assertRaises(_NameLookup):
            http.server.ThreadingHTTPServer(("127.0.0.1", 0), csp_server.CspHandler)


if __name__ == "__main__":
    unittest.main()
