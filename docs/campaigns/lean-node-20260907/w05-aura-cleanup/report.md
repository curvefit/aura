# Aura maintainability handoff

Owner: `w05-aura-cleanup`, campaign `lean-node-20260907`.

Implemented and locally integrated. The clean example/fixture checks and full
release build pass. W01 reports the integrated all-target suite passed on the
host after environment corrections; the passing raw log was requested but not
delivered before closeout. Clippy and the complete release smoke script are
unconfirmed. Publication remains blocked by automatic approval review, with an
explicit approval request for this exact campaign branch pending.

## Revisions and state

- Original main: `420052ceb50ea7a720dbf6bf46ebd6e4d3d61634`, clean but stale/diverged.
- Verified remote/deployed baseline: `cc04f75c9217e6c6a575145ab0ed98df232f622d`.
- Final implementation: `d8afc37878f3bf6f3520ce601d01481f63bf8b74`.
- Local integration observed: `e88f8ea4a596788e4e7921f7a34543c7436adb2f`.
- Branch: `campaign/lean-node-20260907/w05-aura-cleanup`.
- Remote: `https://github.com/curvefit/aura.git`; no successful w05 push yet.

The untouched isolated branch moved to current remote main before edits.
Original main, other owners' worktrees, historical readers, fixture bytes,
formats, and algorithm implementations were preserved. The integration branch
contains the cleanup commit and identical package/example/smoke files. Integration
of w03/w04 code is a separate result and must use their correctness evidence.
No service, dataset retention policy, release, or repository visibility changed
in this lane. The CI workflow is implemented locally and is not remotely active.

## What changed and why

- Disabled unused Arrow CSV/JSON/pretty-print features while retaining IPC
  compression used by the shadow APIs and tests. Locked packages fall from
  143 to 132. All ten direct dependencies and eight existing binaries have
  consumers and remain. `package-audit.json` maps them; no dynamic/configured
  entry point was removed on text-search evidence alone.
- Replaced the legacy-only `roundtrip` entrypoint with exact synthetic V2 SDK
  write/read and bidirectional conversion checks. Its optional fixture output
  creates new files and reports path/OS errors. The `legacy` APIs/tests and
  `aura-size` remain available, with their separate purpose documented.
- Added one reproducible smoke script, a contributor path, and CI checks.
  Test harnesses run serially because an existing concurrency test starts four
  encoding threads itself. Report-only campaign updates do not rerun CI.
- Made `docs/BENCHMARKING.md` the current reproduction owner, with a lowercase
  URL pointer. Fixed the already-renamed `aura-sdk-bench` command and selected
  tiny-fixture operations that fit its schema. Preserved historical numbers
  in that one page and labeled unavailable private inputs and workload limits.
- Shortened README; added the component/caller map to `docs/FORMAT.md`;
  corrected Aura0 full-column materialization versus Aura1 range reads;
  documented buffered conversion summaries, V1-header-only recognition, and
  the V2/V3 boundary. Compact is the actual SDK default. No universal size or
  speed win is claimed.
- Corrected the repository metadata; ignored local dotenv credentials;
  preserved the existing Apache-2.0 license. No package/release was published.

Prior cleanup was retained, including `955af3d`'s explicit SDK benchmark target
and `7b23ffc`'s production/development surface distinction. Removed historical
prototype documents were not reintroduced; remaining research evidence stays.

## Supported component map

See [FORMAT.md](../../../FORMAT.md#supported-components-and-ownership) for the
maintained map. The normal path is `AuraSchema` + `AuraWriter`, `AuraReader`,
and `convert_aura` (V2). Explicit events use `AuraI64EventWriter/Reader`;
`source`/`orderbook` handle replay under caller-supplied semantics. `records`,
`generic_planner`, and field programs own physical implementation. Explicit
`v3_*` and shadow APIs remain development surfaces. No source adapter or
retention contract is inferred by the codec.

Aura owns `docs/SDK.md`, `docs/FORMAT.md`, and `docs/BENCHMARKING.md`. W08
confirmed Aura-ar's README and `docs/research-index.md` as its research entry
points; its work is not duplicated here. W04 accepted explicit registration of
its new `aura-replay-bench` target as a separate integration change.

## Validation and reproduction

The clean detached checkout was `/tmp/aura-w05-clean-validation` at `d8afc37`.
Commands ran with `/usr/bin/env -i`, a minimal PATH, an isolated Cargo home that
reused only the public registry cache, offline dependencies, two build jobs,
and one test harness thread. No private data, `.env`, or service was needed.
All final heavy w05 work acquired
`/tmp/lean-node-20260907-1000/bench.lock` directly.

Passed:

- `cargo fmt --all -- --check`, `git diff --check`, Bash syntax and maintained
  local Markdown links.
- Locked Cargo metadata/tree and package inventory: 228 files, no packaged
  dotenv/target/Git directory, no file over 2 MiB, no common credential-pattern
  match. Checked-in compatibility fixture bytes match `cc04f75` exactly.
- `cargo build --locked --example roundtrip --example order_book_aura0 -j 2`
  completed in 29.30 seconds in the final short slot.
- Both freshly built examples ran. Four source rows, including repetitions,
  backwards timestamps, and signed scaled integers, retained exact values,
  schema, and metadata through ingest/Aura0/Aura1 and both compiled directions.
  Files were 1,275 / 639 / 581 bytes. Explicit event/child equality passed on
  the 584-byte order-book example.
- The frozen baseline `aura-bench` ran a tiny conversion on the newly generated
  fixture: decoded rows, record count, schema/footer validation, and output
  preservation all passed. This is a correctness smoke, not a speed result.

`validation.json` records commands, source identity, fixture hashes, and the
verification fields. Hashes describe identity; the example independently
compares decoded values to its supplied source rows. Raw local transcripts are
under `/tmp/aura-w05-validation-logs/`.

The first isolated build used its full 60-second slot compiling dependencies
and timed out; it is not counted as a passing build. Its continuation held the
lock from 01:13:03 to 01:13:32 UTC and passed. One early delegated debug check
mistakenly ran unlocked at 00:16:13 UTC; peers were notified and overlapping
performance observations were excluded. No performance claim uses that run.

The combined release build at `e88f8ea4` passed:

```bash
cargo build --offline --locked --release --lib --bins --examples -j 2
```

It completed in 277.176 seconds in w01's writable campaign target. The all-target
suite initially recorded 650 passes and four ignored maintenance tests before
one environment failure: first Unix-socket permission, then an overlong socket
path. W01's 01:50:59 UTC closeout reports this full command passed on the host
after correction:

```bash
cargo test --offline --locked --release --all-targets -j 2
```

The passing raw host log was requested but not delivered before closeout; the
report identifies that result as owner-reported. The actual build and earlier
test logs are hashed in `validation.json`. The developer guide now records the
observed Unix-socket permission/short-TMPDIR requirement.

Unconfirmed checks remain visible for the next approved validation run:

```bash
bash scripts/check-onboarding.sh
cargo clippy --locked --all-targets -j 2 -- -D warnings
```

The script's individual operations were exercised through the fresh debug
examples and frozen release benchmark binary; the complete release script is
not claimed as executed. Final package inventory is 229 files, including the
new validation JSON.

The meaningful reduction is 11 unused locked packages. Runtime performance is
unmeasured here. Public smoke timings are deliberately not used as throughput
claims. The user requested a bounded one-to-two-hour finish; this handoff
preserves the remaining verification limits instead of running new heavy suites.

## Resume

After explicit approval for the previously blocked external export, push this
reviewable branch (no release or visibility change):

```bash
git -C /home/anton/Downloads/lean-node-20260907/aura-w05 push -u origin HEAD:refs/heads/campaign/lean-node-20260907/w05-aura-cleanup
```

Automatic approval review rejected the earlier push because it did not find
trusted destination-specific authorization for exporting this branch. No
alternate transport or indirect push was attempted. A verified local bundle at
`/home/anton/Downloads/lean-node-20260907/w05-aura-cleanup.bundle` preserves the
work with prerequisite `cc04f75`; it is a local handoff, not remote receipt.
