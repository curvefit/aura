# Aura format levels

Aura uses one canonical ingest file and two compiled physical levels. Each level
stores the same logical event stream, but optimizes for a different point in the
disk-versus-replay-speed tradeoff.

## `.aura`: ingest

`.aura` is the canonical normalized file written by collectors.

- values are stored as generous logical integers such as `i64`/`u64`,
- records are grouped by timestamp when the logical schema benefits from it,
- the writer tracks field ranges, deltas, shape histograms, and group sizes,
- the final stats and physical-layout decisions are written in a footer when the
  file is sealed.

The ingest file should be easy for collectors to write correctly. It is not the
smallest or fastest replay shape.

## `.aura0`: compact

`.aura0` is compiled from `.aura` stats for cold storage. The compiled footer
stores the resulting decode program, not the full stats table.

- integer widths are selected from observed ranges and deltas,
- signed deltas use zigzag-compatible planning,
- repeated values may be represented as offsets from bases or previous values,
- chunk-local decode state keeps conversion parallelizable.

`.aura0` is the small file. It should remain reversible to `.aura1` through the
logical stream.

## `.aura1`: replay

`.aura1` is compiled from `.aura` stats for fast replay. The compiled footer
uses the same field-program idea, but the body favors fixed or block-friendly
records over maximum compression.

- excessive `i64` fields are shrunk when stats prove a smaller width is safe,
- records use fixed-width headers and slots,
- same-timestamp events can be packed into fixed-width blocks,
- repeated timestamps across adjacent blocks are valid when a run exceeds the
  chosen block capacity.

For example, a timestamp with seven updates and a block capacity of four is
encoded as two `.aura1` blocks with the same timestamp: one full block of four
slots and one block with three real slots plus one padding slot. Logical order is
file order, then slot order.

`.aura1` should be bigger than `.aura0`, but it should be much easier to parse
with predictable pointer arithmetic.
