# Contributing

## Prerequisites

Solador targets **macOS and Windows**; the [agent](agent/) is the Linux piece.
[README.md](README.md#prerequisites) has the full list with rationale — the
short version:

| | |
|---|---|
| **Rust** | via [rustup](https://rustup.rs). Don't pick a version — `rust-toolchain.toml` pins one and rustup installs it, with `rustfmt` and `clippy`, on first `cargo` run. |
| **macOS** | Xcode Command Line Tools (`xcode-select --install`). Required for `cc`/`ld` — Rust links against Apple's SDK — plus `codesign` and `iconutil`. Full Xcode is not needed. |
| **Windows** | MSVC C++ Build Tools, and the WebView2 runtime on Windows 10. |
| **Node 22** | only for `tests/frontend`. Not needed to build or run. |

If `./dev` fails with a linker error that never mentions Xcode, the Command
Line Tools are what's missing.

## The loop

```bash
./dev             # build and run
./dev test        # cargo test --locked --workspace + the Playwright suite
./dev lint        # cargo fmt --check + clippy -D warnings — exactly what CI runs
./dev format      # fix formatting
./Scripts/install-hooks.sh   # one-time: run lint before every push
```

CI is three jobs, and their names are load-bearing (a branch ruleset requires
them by string):

| Job | Runs |
|---|---|
| `Rust agent` | fmt, clippy, build and test inside `agent/` |
| `Rust workspace + frontend e2e` | the root workspace, then Playwright |
| `Windows workspace tests` | the workspace on `windows-latest` |

`agent/` is a **separate Cargo workspace** with its own lockfile, toolchain and
CI job. `./dev test` does not touch it — run `cargo test` inside `agent/` if you
change it, and redeploy it to the host running it.

## What the code expects of you

A few conventions here are load-bearing rather than stylistic, and a change
that breaks one will be sent back:

**Never fabricate a value.** If something could not be measured, it is `None`
all the way to the screen and renders as `—`. `0` means *measured zero*. This
extends to state: `Configured` is `Unknown` / `Absent` / `Present`, and only
`Absent` — a pass that looked and found nothing — may print a setup
instruction. A defaulted state is as much a fabrication as a defaulted number,
and this codebase has shipped that bug twice and does not intend to again.

**Rust decides what a panel says; the frontend lays it out.** Every string and
colour is produced in `crates/viewmodel` or `app/src-tauri` and published to
the frontend. If you find yourself composing a label or picking a colour in
JavaScript, that belongs in Rust.

**Colours come from `crates/viewmodel/src/color.rs`.** `app/ui/app.css` and one
Playwright spec mirror a few constants because they cannot read them at
runtime; a test parses both files and fails if either drifts. Change the
constant, not the mirror.

**Tests assert roles, not values.** Prefer `color::hex(color::RED)` over
`"#e0614f"`. A re-palette should not break a test that was never about a hex
code.

**Comments explain why.** The existing ones record what went wrong and what the
fix has to keep true. Matching that is more useful than matching the formatting.

## Pull requests

- Conventional commits: `feat:`, `fix:`, `chore:`, `docs:`.
- One logical change per PR.
- New behaviour comes with a test. Bug fixes come with the test that fails
  first — if it passes before your change, it isn't testing the bug.
- Say what you verified, and what you didn't. "Tests pass" and "I ran it" are
  different claims and both are useful.

## Reporting bugs

Use the issue templates. The one field that matters most is **which panel** —
the panels fail independently and a report without one is usually unactionable.

Security issues go through [SECURITY.md](SECURITY.md), not the issue tracker.
