# Aura format tiers

Aura uses one generic event model and several physical profiles. Each profile
stores the same logical facts, but trades disk size for parsing speed.

## Logical event

```text
BookEvent
  ts_event
  sequence
  book_id = book_a | book_b
  bids: [LevelChange]
  asks: [LevelChange]

LevelChange
  price: scaled integer
  qty_a: scaled integer
  qty_b: scaled integer
```

`book_a` and `book_b` are neutral labels for any pair of related book streams.
The format does not assume any venue, product, or provider-specific semantics.

## Tier 0: cold

Cold is the canonical archive profile.

- timestamp and sequence are delta encoded,
- prices are zigzag delta encoded inside each side of each event,
- quantities are stored as absolute scaled integers,
- counts use varints,
- files should be chunked and compressed independently.

Cold is smaller and still far faster to decode than text formats, but it is not
intended to be the fastest replay shape.

## Tier 1: warm

Warm resolves deltas into fixed-width integer fields while keeping exact event
lengths.

- one event header per event,
- fixed `i64` level fields,
- exact bid/ask counts,
- no level padding.

Warm is useful as an intermediate representation and as a simple replay format
when disk still matters.

## Tier 2: grouped hot

Grouped hot is an optional hot experiment for repeated timestamps.

- the encoder detects consecutive events sharing `ts_event`,
- groups are emitted in powers of two such as 1, 2, 4, or 8,
- group size falls back to 1 when grouping does not help.

This tier is a compact-hot idea, not the default max-speed shape. It should earn
its place with benchmarks.

## Tier 3: ultra hot

Ultra hot is the maximum parsing-speed profile.

- one fixed-size event header per event,
- timestamps and sequences are repeated,
- levels are fixed-width records,
- bid and ask sections are padded to a per-file block size.

The parser can walk bytes with predictable pointer arithmetic. Ultra hot is a
rebuildable cache, not the source of truth.
