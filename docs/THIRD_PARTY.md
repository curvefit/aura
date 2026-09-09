# License and fixture provenance

Aura retains its established [Apache-2.0 license](../LICENSE). The source tree
contains authored Rust, schemas and synthetic fixtures; Cargo retrieves
third-party crates instead of vendoring them. The upstream Apache Arrow notices
present in the locked Arrow/Parquet crates are preserved in [NOTICE](../NOTICE).

The direct locked dependencies declare:

| Dependency | License |
| --- | --- |
| anyhow, serde, serde_json, sha2 | MIT OR Apache-2.0 |
| arrow, parquet, flatbuffers | Apache-2.0 |
| lz4_flex, zstd Rust wrapper | MIT |

Cargo.lock identifies transitive dependencies as well. The native Zstandard
sources used by zstd-sys carry their upstream licenses; a binary distributor
must include the applicable dependency licenses, not just Aura's license.
This repository does not publish a bundled binary or change upstream terms.

`tests/fixtures` and the public examples contain generated, invented values.
Their generators and manifests identify the intended formats and hashes; no
retained provider data was copied into those fixtures. These original fixtures
are covered by the repository license. Private candle/book inputs used for
bounded measurements remain host-only and are not redistributed by this tree.
Historical reports identify those measurements separately from public smoke
fixtures.
