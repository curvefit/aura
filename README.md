# Aura

Aura is a binary event-file library and format laboratory. It provides a
generic, versioned container for normalized facts, compact cold storage, and
replay-oriented storage without tying the format to a venue or dataset name.

The current implementation has two deliberately separate surfaces:

* V2 is the default production container used by the existing SDK writer,
  reader, and conversion paths. It remains the compatibility baseline for
  `.aura`, `.aura0`, and `.aura1` files.
* V3 has two complete, explicitly bounded, uncompressed Aura0 SDK flavors:
  flat event-scoped values in `AURAV3VB` blocks and grouped exact events in
  `AURAV3EB` chunks. Both have self-contained schema/footers, checksums,
  hashes, and seekable writer/reader APIs. The flat flavor also has the
  developer CLI; there is no grouped CLI complete-seal command yet. Neither
  V3 flavor is production-ready or the default.

The public model is intentionally generic:

- `.aura` is the V2 ingest/preservation profile: normalized logical facts and
  seal-time statistics where available;
- a versioned schema header can declare positional fields, relationships,
  derived-expression references, repeated groups, timestamps, and opaque
  streams;
- `.aura0` is a compact cold-storage profile. The established V2 writer uses
  compiled instructions; the explicit V3 APIs use exact-value or exact-event
  blocks;
- `.aura1` is the V2 replay-oriented fixed/block profile.

## Format Levels

| File | Current container | Purpose |
|---:|---|---|
| `.aura` | V2 ingest | Normalized facts plus seal-time optimization stats when known. |
| `.aura0` | V2 or explicit V3 | V2 compact instruction streams, or the supported V3 flat/grouped exact subset. |
| `.aura1` | V2 | Replay-oriented fixed/block encoding compiled from the V2 plan. |

The V2 levels trade disk for parsing speed. The generic V2 writer emits V2
containers even when its profile is `.aura0` or `.aura1`; V3 is selected only
through an explicit V3 API or the flat V3 CLI. V3 flat and grouped Aura0 files
are self-contained and are not converted by the V2 compiled-profile conversion
path.

## Repository Scope

Aura contains generic binary codec mechanics and research prototypes:

- varint and zigzag delta encoding,
- fixed-width replay records,
- dynamic padded level blocks,
- chunk directories for independent compression frames,
- ingest-to-compiled conversion paths,
- synthetic benchmark inputs.

It does not include venue-specific adapters, private source semantics, capture
daemons, or production data. A caller or external schema author supplies the
logical schema; Aura validates it and the V2 planner, where applicable, selects
physical instructions from declared relationships and observed values. The V3
exact profiles do not run a physical relationship planner, compression, or
Plan v2. Dataset names and venue labels are not part of a V3 seal decision.


## Docs

- [SDK](docs/SDK.md) explains the public schema, writer, reader, and converter API.
- [Schema API](docs/SCHEMA.md), [writer API](docs/WRITER.md), [reader API](docs/READER.md), [conversion API](docs/CONVERSION.md), and [errors](docs/ERRORS.md) document the current library surface.
- [Format levels](docs/tiers.md) explains ingest, Aura0, and Aura1.
- [Aura container](docs/container.md) explains the header/body/footer shape.
- [Format specification](docs/FORMAT.md) records the V2 and V3 wire layouts,
  compatibility boundary, and current limits.
- [Field programs](docs/field-programs.md) explains compact decode instructions.
- [Schemas](docs/schemas.md) explains logical schema construction.
- [Shadow Arrow protocol](docs/SHADOW_PROTOCOL.md) specifies the offline external-compiler boundary and safe reference-block publication.
- [Chunked storage](docs/chunking.md) explains independent compression chunks.
- [Compression policy](docs/compression.md) explains why chunks beat whole-file streams.
- [Aura1 block padding](docs/hot-padding.md) explains fixed-width replay blocks.
- [Conversion flow](docs/conversion.md) explains compiled materialization.
- [Naming](docs/naming.md) lists prototype file extensions and magic values.

## Quick checks

```bash
cargo test
cargo run --bin aura -- schema validate --input schema.json
cargo run --bin aura -- schema canonicalize --input schema.json --output canonical-schema.json
cargo run --bin aura -- shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
cargo run --release --bin aura -- v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema <canonical-schema.json> \
  --output <new-file.aura0> --json < <arrow-ipc-stream.bin>
cargo run --release --bin aura -- v3 aura0 verify \
  --input <new-file.aura0> --json
cargo test --test v3_aura0_container
cargo test --test v3_grouped_container --test v3_grouped_writer_reader
cargo run --bin aura-size -- 10000 1 8
cargo run --example roundtrip
```

The flat V3 seal command reads one Arrow IPC stream from standard input and requires
the schema file to already be Aura's canonical external schema JSON. It writes
an absent `.aura0` destination through a temporary file, syncs it, and
publishes it atomically. Verification reopens the complete file and checks the
header, footer, schema fingerprint, per-chunk stored and logical hashes, body
hash, statistics, global logical hash, and exact file length. A write that fails
before the complete seal, or any writer/flush/sync error, is a failed and
uncommitted result that must not be published even if bytes happen to end in a
seal. The flat CLI syncs a temporary file before atomic publication and never
silently replaces an existing destination. The grouped SDK writer takes a
seekable stream and has no corresponding CLI complete-seal command yet.

The supported V3 flat subset is intentionally narrow: event-scoped fields,
exact fixed and variable values, and optional validity bitmaps. It rejects
groups, repeated fields, derived expressions, byte-200 dual-domain execution,
and V3 Aura1 conversion. The grouped subset accepts exactly one V3 repeated
group with two domains: its byte-200 non-null U8 side field, all repeated child
slots, authoritative event-to-child offsets, exact null validity, and stable
source order are encoded and hashed. Grouped batches and chunks preserve event
and child ranges; rechunking does not change the global logical hash.

Flat hard ceilings are a 16 MiB front header, 64 MiB footer, 1 TiB body, 65,536
chunks, 16,777,216 rows, 1 GiB per exact-value block, and 16 MiB per variable
value. Flat defaults are 256 MiB body/block, 4,194,304 rows, and 4,096 chunks.
Grouped hard ceilings are a 16 MiB front header, 64 MiB footer, 1 TiB body,
65,536 chunks, 16,777,216 events, 67,108,864 children, and 16 MiB schema;
grouped defaults are 256 MiB body, 4,096 chunks, 1,048,576 events, and
4,194,304 children. Grouped event-block hard/default ceilings are 1 GiB /
256 MiB block, 4,194,304 / 1,048,576 events, 16,777,216 / 4,194,304
children, and 67,108,864 / 16,777,216 values. `V3FlatLimits::HARD` and
`V3GroupedLimits::HARD` are explicit opt-ins;
caller limits are clamped to these format ceilings. These limits and checks are
safety ceilings, not performance claims.

Both complete V3 writers reject non-empty derived-expression tables. Grouped
Flag 200 is exact logical discriminator execution only; V3 does not select
physical relationship transforms, compression, or Plan v2. Failed or
interrupted SDK writes are discarded and re-written rather than recovered;
writer/flush/sync failures must not be published even when bytes end in a seal.
Readers reject bad trailer lengths, hashes, ranges, schema,
null/value planes, and trailing bytes; they do not promise concurrent-mutation
exclusion. Aura0 is therefore an explicit SDK/shadow surface, not a blanket
replacement for Parquet in Grimoire.
