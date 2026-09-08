# Contributing to Aura

Start with the [README](README.md) and [architecture/format map](docs/FORMAT.md).
This is one Rust package. The default integration surface is the V2 SDK; V3
APIs remain explicit development formats. Keep source adapters and operational
retention policy outside the codec.

## Development environment

Use stable Rust/Cargo with rustfmt and Clippy, a C/C++ compiler and native build
tools, Git, Bash, and Python 3. Commit `Cargo.lock` when dependencies change.
An MSRV is not currently promised; CI checks stable Rust. No `.env`, private
fixture, external service, or Python package install is required for the normal
checks. Cargo needs registry access once, or a populated dependency cache for
`CARGO_NET_OFFLINE=true`.

```bash
cargo build --locked --release --lib --bins --examples -j 2
bash scripts/check-onboarding.sh
cargo test --locked -j 2 -- --test-threads=2
cargo clippy --locked --all-targets -j 2 -- -D warnings
cargo fmt --all -- --check
cargo package --locked --list
```

The smoke script preserves its tiny synthetic output directory for inspection.
The tests include historical fixture decoding, CLI behavior, corruption/bounds,
exact events, schema validation, and current experimental formats. Ignored
fixture-regeneration tests are maintenance operations, not normal checks; do
not regenerate fixtures to hide an incompatibility. See
[COMPATIBILITY.md](docs/COMPATIBILITY.md). When sharing a campaign host, acquire
the registry's host-wide benchmark lock before builds, tests, and benchmarks;
the [benchmark guide](docs/BENCHMARKING.md) shows the command.

## Making a change

Use a focused branch and describe the concrete problem, resulting behavior,
commands run, and any compatibility/performance limits. Add a targeted
regression test for a behavioral bug; keep documentation/examples changes small.
Avoid global formatting changes and new abstraction layers for small helpers.
Preserve historical readers and reference paths unless their consumers and
compatibility obligations have been checked. Maintain optional execution
choices as explicit options, with errors or reported fallback.

For codec changes compare exact decoded source values, order, event/child
boundaries, scales, and null validity as applicable. Hash identity alone is not
independent transformation evidence. For performance claims record workload,
input/output bytes, threads, guard mode, and work included using the
[reproduction guide](docs/BENCHMARKING.md).

Update the authoritative API/format/benchmark page with behavior changes;
research notes should link to it rather than repeat a second current contract.
Do not commit private payloads, credentials, `.env` files, target directories,
or generated benchmark output. Keep small synthetic compatibility fixtures and
their manifests. The existing [Apache-2.0 license](LICENSE) applies.

## Tools

| Command | Scope |
| --- | --- |
| `aura` | Schema validation/canonicalization and explicit V3/shadow developer operations; see `--help` and the shadow protocol. |
| `aura-bench` | V2 codec/parse/transcode measurements. |
| `aura-sdk-bench` | Generated SDK matrix; larger than the onboarding smoke. |
| `aura-fixture-gen` | Source-neutral benchmark fixtures and manifests. |
| `aura-verify-random` | Randomized stabilization runner. |
| `aura-json-i64` | Caller-supplied integer rows/schema conversion utility. |
| `aura-parquet-ohlcv` | Specific OHLCV Parquet normalization utility; it sorts timestamps and is not a general lossless Parquet importer. |
| `aura-size` | Historical `legacy` synthetic layout comparison, separate from V2 SDK benchmarking. |

These executables are retained because they have different contracts. The
public onboarding path uses the SDK examples and `aura-bench`.
