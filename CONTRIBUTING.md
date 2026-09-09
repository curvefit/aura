# Contributing to Aura

Start with [README](README.md), [SDK](docs/SDK.md), and the component map in
[FORMAT](docs/FORMAT.md). Rust 1.97.1 is the CI-validated toolchain; no MSRV is
promised. Install rustfmt, Clippy, a C/C++ toolchain, Bash and Python 3. Normal
checks need no private data, sibling checkout, credentials or `.env`.

## Focused changes

| Change | Read and check |
| --- | --- |
| Add an SDK round trip | `src/sdk.rs`, your schema/batch, `tests/sdk_api.rs`; run `cargo test --locked --test sdk_api`. |
| Change an encoding candidate | Its codec and selection policy, with its inverse/corruption tests. Grouped V3 uses `GroupedSearch` in `src/v3_planned_grouped.rs`; run `cargo test --locked --test v3_plan_v2_planned_grouped`. |
| Change derived arithmetic | `src/expressions.rs` and its callers' access contract; run `cargo test --locked --test generic_planner`. |
| Run a comparison | Use the existing commands in [BENCHMARKING](docs/BENCHMARKING.md); research source formats live independently in Aura-AR. |

The V2 facade is `aura_codec::sdk`; existing root imports remain supported.
V3/shadow types and planners live in `aura_codec::experimental`. Retained
Grimoire flat-reader root imports remain compatible. Do not turn a test-only
forced candidate into another public helper. Research boundaries share one
complete-file selector; strict size ties keep the earlier candidate.

## Broader gate

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -j 2 -- -D warnings
cargo test --locked -j 2 -- --test-threads=1
cargo build --locked --release --lib --bins --examples -j 2
bash scripts/check-onboarding.sh
cargo package --locked --list
```

Use `--offline` after dependencies are cached. The Unix publication test needs
local socket permission and a short `TMPDIR`, such as `/tmp`. Ignored fixture
regeneration tests are deliberate maintenance actions, not a way to conceal
incompatibility. See [COMPATIBILITY](docs/COMPATIBILITY.md).

On a shared host, acquire its established benchmark lock before builds/tests;
measurement runners that acquire it themselves must not be wrapped again.
Record exact source/build/input identities, work boundaries and actual peak
versus sampled RSS. Preserve negative results and complete metadata costs.

Preserve decoded values, nulls, scales, overflow fallback, source order and
event/child boundaries. A hash binds validated facts; it cannot restore omitted
facts. Keep source adapters, retention and publication policy outside Aura.
Include focused regression evidence in PRs. Keep private inputs, credentials,
targets and generated experiment output out of Git. Apache-2.0 applies; preserve
third-party notices and synthetic fixture provenance.
