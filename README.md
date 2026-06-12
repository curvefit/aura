# Aura

Aura is an experimental binary event-file format lab. It explores how to write
normalized ingest files once, then compile them into compact storage or fast
replay layouts without tying the format to any specific data source.

The public model is intentionally generic:

- an ingest file stores normalized logical facts with generous integer fields,
- a schema defines what a record means and what stats should be tracked,
- `.aura0` is the compact compiled level,
- `.aura1` is the replay-optimized compiled level,
- compiled levels are rebuildable from canonical `.aura` files.

## Format Levels

| File | Level | Purpose |
|---:|---|---|
| `.aura` | Ingest | Canonical normalized facts plus seal-time optimization stats. |
| `.aura0` | Aura0 | Compact cold encoding compiled from ingest stats. |
| `.aura1` | Aura1 | Replay-optimized fixed/block encoding compiled from ingest stats. |

The levels trade disk for parsing speed. `.aura` should be retained as source of
truth; `.aura0` and `.aura1` are derived representations that can be
regenerated.

## Repository Scope

Aura documents and prototypes generic binary codec mechanics:

- varint and zigzag delta encoding,
- fixed-width replay records,
- dynamic padded level blocks,
- chunk directories for independent compression frames,
- ingest-to-compiled conversion paths,
- synthetic benchmark inputs.

It does not include venue-specific adapters, private source semantics, real
payload samples, or production capture logic.


## Docs

- [Format levels](docs/tiers.md) explains ingest, Aura0, and Aura1.
- [Aura container](docs/container.md) explains the header/body/footer shape.
- [Schemas](docs/schemas.md) explains logical schema construction.
- [Chunked storage](docs/chunking.md) explains independent compression chunks.
- [Compression policy](docs/compression.md) explains why chunks beat whole-file streams.
- [Aura1 block padding](docs/hot-padding.md) explains fixed-width replay blocks.
- [Conversion flow](docs/conversion.md) explains compiled materialization.
- [Naming](docs/naming.md) lists prototype file extensions and magic values.

## Quick checks

```bash
cargo test
cargo run --bin aura-size -- 10000 1 8
cargo run --example roundtrip
```

The current code is a prototype skeleton. It is meant to preserve the important
format ideas and make future benchmarking straightforward, not to claim a stable
wire format yet.
