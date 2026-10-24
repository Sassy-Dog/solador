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
designed so that, over plain HTTP, it is tailnet-only **by default** and refuses
to start without a tailnet address; an explicit `SOLADOR_AGENT_BIND` can change
that. With TLS on (the default for a fresh install) and no Tailscale it binds
**all interfaces**, and TLS plus the bearer token protect it. The cockpit, for
its part, only dials plain HTTP to loopback or Tailscale addresses, enforced by
`agentclient`'s `plain` module. That rule trusts an **address range, not tailnet
membership**: `100.64.0.0/10` is RFC 6598 CGNAT space that some ISP, hotel and
cloud-VPC networks also use, so an address there may not be on a tailnet at all
(Tailscale's 4via6 prefix, `fd7a:115c:a1e0:b1a::/64`, is excluded because its
last hop can leave the tailnet in cleartext). Pairing a host, which pins its
TLS certificate, is the stronger protection. See "Network exposure" in
`agent/README.md`. If you can make it answer from somewhere
it shouldn't, or make it leak beyond the metrics it is meant to serve, that is
a report worth filing.

**Credentials.** Every token lives in the OS credential store — Keychain on
macOS, Credential Manager on Windows — and never in `store.json`. There is a
test asserting the settings file holds no secret material. A path that lands a
credential on disk in plaintext, or into a log line, or into an error string
shown in the UI, is a vulnerability. The Azure blob client deliberately strips
URLs out of transport errors for exactly this reason: a SAS token is a URL
query string, and error messages get pasted into issues. The one credential
Solador reads that it did not store is another tool's own sign-in, for the
Usage panel's Claude and Codex plan limits: off until ticked, read for one
request per poll, never copied, written or refreshed (`docs/SECRETS.md`,
"Borrowed sign-ins"). A path that makes either one leave that request, or be
modified, is a vulnerability too.

**The command line hands credentials in, never out.** `solador-app host …`
gives the running app a host and its token over a local channel: on Unix a
socket, `<store dir>/run/cli.sock`, whose directory is created `0700` and
refused by both sides unless it belongs to the current user, with the server
checking each peer's uid; on Windows a named pipe created with an owner-only
DACL (one ACE, the current user's SID), as the first instance (a process that
took the name first makes the app's create fail rather than be served beside)
and with remote clients rejected. There the server checks each client's user
SID, and the command checks the server's before it sends a request, which
carries a token. Anything that can connect could already edit `store.json` as
that user, so the channel adds no new principal, and no reply carries a token, so it
adds no way to read one. A pin is set from it only when the app's own probe of
the address finds the fingerprint the request names, and only after the agent
accepts the token over that pinned connection. A reply that carries a token, a
peer of another user being answered (or, on Windows, a request sent to a pipe
served by another user), or a pin saved without that match is in
scope.

`solador-app host add --ssh` reads that fingerprint and that token on the host,
over the operator's own ssh, which runs with `BatchMode=yes` and
`StrictHostKeyChecking=yes` (it never accepts a host key or answers a prompt)
and never runs `sudo`. The fingerprint is only the *expected* value; the app's
own probe still has to equal it. The token travels on ssh's stdout and the
channel, never in an argument. Text the host sends back is printed with its
control characters replaced, so a host cannot repaint the operator's terminal.
A token in any argv, log or output, a host key accepted on the operator's
behalf, a change made on the host that no flag allowed (removing
`SOLADOR_AGENT_REQUIRE_TAILNET` above all), or terminal control sequences from
the host reaching the terminal is in scope.

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
