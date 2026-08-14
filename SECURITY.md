# Security

## Reporting a vulnerability

Use GitHub's **[private vulnerability reporting](https://github.com/Sassy-Dog/solador/security/advisories/new)**
for anything exploitable. Please don't open a public issue for it first.

Expect an acknowledgement within a week. This is a personal project, not a
staffed product — if a fix needs longer than that, you'll be told so rather
than left waiting.

## What the threat model actually is

Worth stating plainly, because the interesting surface is not where it usually
is for a desktop app.

**The agent is the part to look at.** `agent/` is an HTTP service that reports
host metrics and container lists, guarded by a single bearer token. It is
designed to be reachable **only over a private tailnet**, never on a public
interface, and it binds accordingly. If you can make it answer from somewhere
it shouldn't, or make it leak beyond the metrics it is meant to serve, that is
a report worth filing.

**Credentials.** Every token lives in the OS credential store — Keychain on
macOS, Credential Manager on Windows — and never in `store.json`. There is a
test asserting the settings file holds no secret material. A path that lands a
credential on disk in plaintext, or into a log line, or into an error string
shown in the UI, is a vulnerability. The Azure blob client deliberately strips
URLs out of transport errors for exactly this reason: a SAS token is a URL
query string, and error messages get pasted into issues.

**The frontend runs under a real CSP** (`app/src-tauri/tauri.conf.json`) and the
Playwright suite serves it under the same policy. Panel content — repository
names, container names, incident text — reaches the DOM as text, never as
markup. An injection through any of those is in scope.

**No telemetry, and crash reporting is opt-in.** The app ships no analytics of
any kind. It does carry a crash reporter (`crates/crashreport`), and it is
**off** until you turn it on in Settings → General — a fresh store, and every
store written before the feature existed, reports nothing. With it off no client
is created, no panic hook is installed and no network code is reachable. With it
on, a panic is rebuilt from an allow-list of fields before it is sent:
`server_name`, user, request, breadcrumbs, tags, extra, contexts, module list,
absolute paths, source lines and local variables are dropped by construction,
and the free text that remains is redacted down to a positive word rule. No
credential is ever collected — tokens live only in your OS credential store.
A scrubbing miss is in scope and we want to hear about it. If you find the app
talking to a host you didn't configure, that is a bug and a serious one.

## What is out of scope

- Anything requiring an attacker who already has your unlocked machine and your
  credential store.
- Findings against a dependency with no demonstrated path through this code.
