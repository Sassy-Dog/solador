// Serves app/ui plus a dumped view-model, under the same CSP header the
// shipped app enforces (see csp_server.py) -- plain `http.server` sends no
// CSP at all, which validates layout under a policy the app doesn't ship.
// No build step: the frontend is static.
export default {
  testDir: ".",
  use: { baseURL: "http://127.0.0.1:4173" },
  webServer: {
    command: "python3 csp_server.py 4173",
    url: "http://127.0.0.1:4173/index.html",
    reuseExistingServer: true,
  },
};
