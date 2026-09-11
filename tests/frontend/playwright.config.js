// Serves app/ui plus a dumped view-model, under the same CSP header the
// shipped app enforces (see csp_server.py) -- plain `http.server` sends no
// CSP at all, which validates layout under a policy the app doesn't ship.
// No build step: the frontend is static.
import path from "node:path";

// Port allocation: derive a stable port from a hash of the worktree path,
// within the 3000-3999 range, instead of pinning one. A hardcoded 4173 here
// meant two concurrent PR runs on the shared self-hosted Mac could serve or
// reuse *each other's* app/ui and silently test the wrong worktree; hashing
// the checkout path below gives each worktree its own port instead.
//
// `__dirname` (not `import.meta.url`): this file is loaded as ESM syntax
// through Playwright's own CJS-transpiling loader (no "type": "module" in
// package.json, no .mjs extension -- same as the plain `export default {}`
// this file already used before this change), and `import.meta` has no
// meaning once transpiled to CommonJS.
const WORKTREE_ROOT = path.resolve(__dirname, "..", "..");

function derivePort(seed) {
  let hash = 2166136261; // FNV-1a offset basis
  for (let i = 0; i < seed.length; i++) {
    hash ^= seed.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return 3000 + (Math.abs(hash) % 1000);
}

const PORT = derivePort(WORKTREE_ROOT);

const SCREENSHOTS = process.env.SCREENSHOTS === "1";

export default {
  testDir: ".",
  // The screenshot generator shares this harness -- same server, same CSP,
  // same fixtures -- but is not a test: it makes no claim about what is
  // correct. `npm test` skips it; `npm run screenshots` runs only it.
  testMatch: SCREENSHOTS ? ["**/screenshots.spec.js"] : ["**/*.spec.js"],
  testIgnore: SCREENSHOTS ? [] : ["**/screenshots.spec.js"],
  use: { baseURL: `http://127.0.0.1:${PORT}` },
  webServer: {
    // `-u`: stdout is a pipe here, not a tty, so Python would otherwise buffer
    // the startup line past the point it is useful.
    command: `python3 -u csp_server.py ${PORT}`,
    // Playwright ignores webServer stdout by default. That default is why two
    // consecutive 60s timeouts on this branch produced ZERO diagnostic text:
    // the server says what it bound and what it is serving, and nobody was
    // listening. stderr is piped by default; it is named here so the pair is
    // visible together rather than one being an unstated default.
    stdout: "pipe",
    stderr: "pipe",
    url: `http://127.0.0.1:${PORT}/index.html`,
    // The default 60s deadline is left alone on purpose. It was nearly spent
    // on every hosted-macOS run -- 36-38s to the readiness line, two runs
    // past 60s -- and all but ~2s of that was a reverse-DNS lookup inside the
    // stdlib's bind, which csp_server.py's LoopbackServer removes (#401).
    // The server's own readiness line prints how long it took; read that
    // before touching this number, because a larger deadline with no
    // explanation turns a loud failure into a slow one.
    //
    // CI never reuses a server left over from a prior run -- each job starts
    // and owns its own, so a stale/foreign server can never be mistaken for
    // this run's. Local iterative dev still reuses one already running.
    reuseExistingServer: !process.env.CI,
  },
};
