# Aura

Aura is a Rust library for storing normalized facts as versioned binary event
files. Callers supply schemas, exact values, units, event boundaries, and source
semantics. Aura encodes and validates those facts; capture and venue adapters
live outside this repository.

The supported default SDK uses **container V2**:

| Profile | Use |
| --- | --- |
| `.aura` | Sealed ingest values, statistics, and physical plans. |
| `.aura0` | Compact storage; the SDK defaults to the semantic `compact` profile. |
| `.aura1` | Fixed-width replay derived from the compiled field plan. |

V2 supports conversion between these profiles. Aura0 favors archive size;
Aura1 favors predictable field access. Optional Aura0 `fast`/`hybrid` profiles
store an Aura1 byte lane and have different size and fallback tradeoffs.
Container V3 APIs and the `aura v3` CLI are explicit development surfaces, with
separate exact/planned flat and grouped formats; they are not the V2 default or
an Aura1 conversion route. File extensions alone do not select a version.

## Start here

Install Rust/Cargo (stable), a C/C++ toolchain for native compression libraries,
and Python 3 for the smoke report check. No dataset, credentials, or `.env` is
needed. From the repository root:

```bash
cargo build --locked --release --lib --bins --examples -j 2
cargo run --locked --release --example roundtrip
cargo run --locked --release --example order_book_aura0
bash scripts/check-onboarding.sh
```

`roundtrip` writes synthetic facts in memory, converts V2 profiles, and checks
decoded rows against the supplied values, including repeated rows and exact
scaled integers. `order_book_aura0` checks explicit event/child boundaries.
The smoke script also saves a tiny fixture and runs `aura-bench` conversion and
parse checks; it prints the output directory. Its timings are smoke evidence,
not a throughput claim.

Use the crate as `aura_codec`; the package name is `aura-codec`. The
[SDK guide](docs/SDK.md) covers schemas, writers, readers, and conversion.
For larger Aura1 files, use file-backed `open_path`/`open_file` and bounded
batches; the generic `open(Read)` interface buffers its input.

## Documentation

- [Architecture and format](docs/FORMAT.md): supported components, caller
  responsibilities, V2/V3 boundaries, and links to wire details.
- [Benchmarks and reproduction](docs/BENCHMARKING.md): runnable synthetic
  commands, measurement boundaries, and labeled historical evidence.
- [Compatibility](docs/COMPATIBILITY.md): frozen fixtures, historical readers,
  and explicit development limits.
- [Contributing](CONTRIBUTING.md): development commands and required checks.

The 0.1 API remains experimental. Exactness applies to the declared, supported
schema and retained facts; Aura cannot reconstruct source fields an adapter
omitted. Unsupported types and versions reject. Hashes check identity and
integrity; first-time conversions still need decoded comparisons to retained
source facts. See the format and compatibility guides before choosing an
archival contract.

Licensed under [Apache-2.0](LICENSE).
