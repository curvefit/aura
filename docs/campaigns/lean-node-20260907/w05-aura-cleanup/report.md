# Aura maintainability handoff

Owner: `w05-aura-cleanup`, campaign `lean-node-20260907`.

Implementation candidate; full clean-checkout validation is pending.

## Baseline and ownership

The original main checkout was clean at
`420052ceb50ea7a720dbf6bf46ebd6e4d3d61634`, but was stale/diverged. Remote
`https://github.com/curvefit/aura.git` main and the deployed SDK were verified at
`cc04f75c9217e6c6a575145ab0ed98df232f622d` using `git ls-remote`. The untouched
isolated campaign branch moved to that revision before edits. Original main
and other owners' worktrees were preserved.

This lane owns package features/metadata, examples, onboarding/CI, and the
maintained documentation entrypoints. It does not change codec algorithms,
wire formats, historical readers, fixture bytes, or service activation.

## Implemented simplifications

- Disabled unused Arrow CSV/JSON/pretty-print features while retaining IPC
  compression used by shadow APIs/tests. Locked packages fall from 143 to 132.
  All direct dependencies and eight existing binaries have consumers and stay.
- Replaced the legacy-only `roundtrip` onboarding with exact synthetic V2 SDK
  write/read and bidirectional conversion checks. The optional fixture output
  uses create-new files and errors retain path/OS context.
- Added one executable public smoke path and CI/development commands. It uses
  no private input and runs a tiny conversion and parse benchmark with decoded
  verification after timing.
- Consolidated current reproduction in `docs/BENCHMARKING.md`; the lowercase
  page keeps its URL as a link. Corrected the already-renamed SDK benchmark
  binary, labeled historical private-fixture evidence, and removed the second
  compatibility scoreboard. The SDK compact default is now explicit.
- Shortened README and added the component/caller-responsibility map to
  `docs/FORMAT.md`. Kept V3 limits and unique research/historical evidence.
  Repository metadata matches the real remote; Apache-2.0 is unchanged.

## Validation so far

`cargo fmt --all -- --check`, `git diff --check`, Bash syntax, maintained local
Markdown links, locked Cargo metadata/tree, and package inventory checks pass.
See `package-audit.json` for removal evidence. Required build/tests/Clippy and
the final public benchmark checks are pending the shared host-lock slot.

An example worker mistakenly ran one completed debug example build without the
required lock at 2026-09-08 00:16:13 UTC. The parent notified w01/w03/w04;
no performance claim uses that run. Final checks will run under the lock.

## Documentation ownership

Aura owns `docs/SDK.md` (API), `docs/FORMAT.md` (architecture/format), and
`docs/BENCHMARKING.md` (reproduction). W08 was notified through the campaign
coordination directory to link Aura-ar research to these authoritative pages.
No claim is made that a remote owner received changes before push/integration.
