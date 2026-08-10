// Serves app/ui plus a dumped view-model, under the same CSP header the
// shipped app enforces (see csp_server.py) -- plain `http.server` sends no
// CSP at all, which validates layout under a policy the app doesn't ship.
// No build step: the frontend is static.
import path from "node:path";

// Org-wide port allocation (root CLAUDE.md, "Local Development Environment"):
// local dev/test stacks derive a stable port from a hash of the worktree path
// within the shared 3000-3999 range instead of pinning one -- a sibling project's
// `derive_worktree_ports` (docs/PORT-ALLOCATION.md) is the reference shell
// implementation this is a small JS equivalent of. A hardcoded 4173 here
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

export default {
  testDir: ".",
  use: { baseURL: `http://127.0.0.1:${PORT}` },
  webServer: {
    command: `python3 csp_server.py ${PORT}`,
    url: `http://127.0.0.1:${PORT}/index.html`,
    // CI never reuses a server left over from a prior run -- each job starts
    // and owns its own, so a stale/foreign server can never be mistaken for
    // this run's. Local iterative dev still reuses one already running.
    reuseExistingServer: !process.env.CI,
  },
};
