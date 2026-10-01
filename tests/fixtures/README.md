# Shared wire-contract fixtures

JSON fixtures that lock the wire contract between the agent (`agent/`) and the
app's decoder (`crates/wire`, `crates/localhost`) so the two cannot silently
drift. They live here, above both, because `agent/` is a separate Cargo
workspace — neither side can own a file the other must agree with.

Each fixture is decoded by tests on **both** sides. Changing the shape requires
updating both, and both suites fail until they agree.

## `battery_contract.json`

The canonical battery wire shape emitted under `snapshot.battery` by the agent.
It is the minimal cross-platform contract — `level` (0–100) and `isCharging` —
the only two fields a generic host agent (Linux/`sysinfo`) can produce. A
macOS-local collector additionally populates richer optional fields (`health`,
`cycleCount`, `wattage`, …); those are decode-optional and never part of this
floor.

- Agent lock: `agent/src/metrics.rs` (`battery_*` tests)
- App lock: `crates/wire/tests/wire.rs`
  (`battery_deserialises_from_shared_contract_fixture`)

## `snapshot_unknowns.json`

A post-#183 snapshot from a producer that cannot measure everything: the keys it
has no reading for are **omitted**, never sent as `null` and never faked as `0`.
Here that is `cpu.thermalState`, `memory.pressure`, both `disk` rates and every
`gpu` field (both objects present but empty), plus a zero-capacity volume. Both
decoders must read an absent key as *unknown* — the distinction that keeps a
green `Pressure: 0%` off a card for a figure Linux never reports.

`snapshot.json`'s counterpart (the all-keys-present payload, including the
literal zeros pre-#183 agents send) stays the backward-compatibility case.

- App lock: `crates/wire/tests/wire.rs`, via a byte-identical copy at
  `crates/wire/tests/fixtures/snapshot-unknowns.json` — that crate's tests
  `include_str!` their fixtures, so the copy has to exist.

  **The copy is guarded by
  `the_local_unknowns_fixture_is_byte_identical_to_the_shared_one`.** That guard
  is new, and it exists because the previous one was a original test that left CI
  when the original app was frozen (2026-08-04) and vanished when it was deleted.
  In the gap the two files did drift — the rename updated this file's
  `solador-agent` process name and left the copy on `devcanopy-agent`, and
  nothing failed, because nothing was still checking. Folding the two onto one
  file remains the real fix (#192).

## `updater/`, `agent/` and `agent-v/` — the signed fixtures

Not wire-contract fixtures: these are the byte-exact inputs to the update
feeds' verifiers (`crates/updatefeed` for the first two and for `agent/`'s
producer test; `agent/src/update.rs` for `agent-v/`), and `.gitattributes` pins
all three directories `-text` because a checkout that rewrites a line ending
rewrites the bytes a signature covers.

- `updater/` — the desktop feed's Tauri-convention fixtures (base64-wrapped
  `.sig` and `.pub`), regenerated with `cargo tauri signer`; see the comment in
  `crates/updatefeed/src/signature.rs`.
- `agent/` — the agent feed's **plain minisign** fixtures (#391). A throwaway
  keypair whose private half was never committed; four short text files
  standing in for `solador-agent-2026.9.9-<triple>` (the verifier does not care
  what the bytes are, only that they are the bytes signed); their `.minisig`
  files, made by the pinned `rsign2` with the same flags
  `scripts/agent-signing.sh` uses; and the `agent-latest.json` +
  `agent-latest.json.minisig` pair that `solador-agent-feed build` produced
  from them and `rsign` then signed. The tests assert `build()` reproduces the
  committed document byte for byte, so **regenerate the pair together, never
  edit either half**. This directory is read by `crates/updatefeed`
  (`src/agent.rs`, `tests/agent_feed_cli.rs`) and, for one guard test, by
  `agent/src/update.rs` (see `agent-v/` below): its URLs are what the producer
  writes today, on the legacy `v<version>` release. The **consumer** moved to
  `agent-v/` below (#488), because its URLs name a release the producer cannot
  yet write.

  ```sh
  cd tests/fixtures/agent   # everything below is relative to here
  rsign generate -W -f -p test-agent-key.pub -s test-agent.key   # -f: the .pub exists
  for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin; do
    n="solador-agent-2026.9.9-$t"
    printf 'not a real solador-agent: a %s stand-in for the feed fixtures (#391)\n' "$t" > "$n"
    rsign sign -W -s test-agent.key -x "$n.minisig" -t "$n" -c "solador-agent release signature" "$n"
  done
  cargo run --manifest-path ../../../Cargo.toml -p solador-updatefeed --bin solador-agent-feed -- build \
    --version 2026.9.9 --tag v2026.9.9 --asset-dir . \
    --download-base https://github.com/Sassy-Dog/solador/releases/download \
    --pubkey test-agent-key.pub --out agent-latest.json
  rsign sign -W -s test-agent.key -x agent-latest.json.minisig -t agent-latest.json \
    -c "solador-agent release signature" agent-latest.json
  rm test-agent.key   # never committed (and `*.key` under tests/fixtures/ is ignored)
  ```

- `agent-v/` — **the consumer's copy of `agent/`** (#488), read by
  `agent/src/update.rs`'s tests: they accept its feed under
  `test-agent-key.pub`, refuse it after one character moves or the final
  newline goes, refuse it under the production key, and hold every target's
  `url` to `.../releases/download/agent-v2026.9.9/solador-agent-2026.9.9-<triple>`.
  The same four stand-in binaries (byte-identical to `agent/`'s, so the
  `sha256` values match) under a **fresh** throwaway keypair — `agent/`'s
  private half was never committed, so its `.minisig` files cannot be remade —
  and a feed whose URLs are on the `agent-v2026.9.9` release rather than
  `v2026.9.9`. The two directories do not share a key.

  **The rewrite step is temporary.** The producer (`solador-agent-feed build`)
  still requires `--tag v<version>` and writes `v<version>` URLs; it learns
  `agent-v` in #490. Until then this directory is assembled by the procedure
  above, run in `tests/fixtures/agent-v` with a new key, plus **one extra step
  between building the feed and signing it**. The signature covers the JSON
  bytes, so the rewrite goes first; the new `.minisig` goes last:

  ```sh
  cd tests/fixtures/agent-v   # the rest is the `agent/` procedure, up to the feed's `build`
  sed 's#/releases/download/v2026.9.9/#/releases/download/agent-v2026.9.9/#g' agent-latest.json > agent-latest.json.rewritten
  mv agent-latest.json.rewritten agent-latest.json
  rsign sign -W -s test-agent.key -x agent-latest.json.minisig -t agent-latest.json \
    -c "solador-agent release signature" agent-latest.json
  rm test-agent.key   # never committed
  ```

  #490 regenerates this directory with the producer itself and deletes the
  rewrite. Until then **no test asserts that the producer reproduces these
  bytes** (`crates/updatefeed` reads only `agent/`), so the two directories are
  tied by one guard instead:
  `the_agent_v_fixture_is_the_producers_fixture_with_only_the_release_rewritten`
  in `agent/src/update.rs` asserts the stand-ins, hashes and version are
  `agent/`'s and that the only difference in the feed's URLs is the release
  segment (signatures differ, being under another key). #490 deletes that
  guard with the rewrite. Regenerate the feed and its `.minisig` together,
  never one half.
