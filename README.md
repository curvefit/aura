# Aura

Aura is an experimental binary event-file format lab. It explores how to write
normalized ingest files once, then compile them into compact storage or fast
replay layouts without tying the format to any specific data source.

The public model is intentionally generic:

- `.aura` stores normalized logical facts with generous integer fields and
  footer optimization stats when available,
- a compact v1 schema header defines positional fields, direct parent refs,
  derived expression refs, repeated groups, booleans/enums, timestamps, and
  opaque streams,
- `.aura0` is the compact compiled level with code-only decode instructions,
- `.aura1` is the replay-optimized compiled level with code-only decode
  instructions.

## Format Levels

| File | Level | Purpose |
|---:|---|---|
| `.aura` | Intermediate | Normalized facts plus seal-time optimization stats when known. |
| `.aura0` | Aura0 | Compact cold encoding compiled from ingest stats into per-field instructions. |
| `.aura1` | Aura1 | Replay-optimized fixed/block encoding compiled from ingest stats into per-field instructions. |

The levels trade disk for parsing speed. Live collectors write stamped `.aura`
first because that is where footer stats and physical plans are collected.
Compiled `.aura0` and `.aura1` files follow those stamped plans; they are not
used as optimization sources for each other.

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

- [SDK](docs/SDK.md) explains the public schema, writer, reader, and converter API.
- [Schema API](docs/SCHEMA.md), [writer API](docs/WRITER.md), [reader API](docs/READER.md), [conversion API](docs/CONVERSION.md), and [errors](docs/ERRORS.md) document the current library surface.
- [Format levels](docs/tiers.md) explains ingest, Aura0, and Aura1.
- [Aura container](docs/container.md) explains the header/body/footer shape.
- [Field programs](docs/field-programs.md) explains compact decode instructions.
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
