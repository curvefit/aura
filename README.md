# Aura

Aura is an experimental binary replay codec lab for sparse book update streams.
It explores how to move from compact archival storage to progressively faster
replay layouts without tying the format to any specific data source.

The public model is intentionally generic:

- an event has a timestamp, sequence, book identifier, and changed bid/ask levels,
- a level has an integer price plus one or two integer quantity fields,
- `book_a` and `book_b` are generic source-book labels,
- cold files are the canonical archive,
- faster files are rebuildable caches derived from cold files.

## Format Tiers

| Tier | Profile | Purpose |
|---:|---|---|
| 0 | Cold | Small canonical archive: deltas, varints, chunked compression. |
| 1 | Warm | Resolved replay: fixed integers, exact level counts, variable event size. |
| 2 | Grouped Hot | Automatic same-timestamp grouping for compact hot experiments. |
| 3 | Ultra Hot | One fixed event header plus fixed padded level blocks per event. |

The tiers trade disk for parsing speed. Tier 0 should be retained as source of
truth; tiers 1-3 are local or derived representations that can be regenerated.

## Repository Scope

Aura documents and prototypes generic binary codec mechanics:

- varint and zigzag delta encoding,
- fixed-width resolved records,
- dynamic padded level blocks,
- chunk directories for independent compression frames,
- cold-to-hot conversion paths,
- synthetic benchmark inputs.

It does not include venue-specific adapters, private source semantics, real
payload samples, or production capture logic.


## Docs

- [Format tiers](docs/tiers.md) explains cold, warm, grouped hot, and ultra hot.
- [Chunked cold storage](docs/chunking.md) explains independent compression chunks.
- [Dynamic hot padding](docs/hot-padding.md) explains fixed-width level blocks.
- [Conversion flow](docs/conversion.md) explains cold-to-hot materialization.

## Quick checks

```bash
cargo test
cargo run --bin aura-size -- 10000 1 8
cargo run --example roundtrip
```

The current code is a prototype skeleton. It is meant to preserve the important
format ideas and make future benchmarking straightforward, not to claim a stable
wire format yet.
