# Aura1 Parse Replay Research

Date: 2026-06-24

## Scope

This sprint is only about Aura1 fixed-width bytes to scan, replay, parse, and
grouped replay. It does not change Aura0 compression, Aura1-to-Aura0 encoding,
byte lanes, or the Aura1 file layout.

## DBN And Fixed-Width Replay Lessons

The useful DBN-style lesson is the shape of the hot path:

- Parse metadata once.
- Derive record width and field offsets once.
- Treat the body as a contiguous sequence of fixed-width records.
- Avoid per-record schema lookup.
- Avoid per-record allocation.
- Keep replay separate from materialized batch parsing.

Aura1 already has the required metadata. Header `header_len` gives the body
start. The trailer and footer length make the footer discoverable from EOF.
The compiled footer stores record count, schema, block capacity, and the Aura1
decode program. `CompiledAuraPlan::from_footer` derives record width, body
size, and field recipes.

The implementation risk is not missing metadata. It is allowing SDK APIs to
fall back to row/value materialization or callback shapes that hide the fixed
width property.

## File-Backed And Mmap Replay

Aura1 file-backed replay should:

1. read the header prefix and `header_len`;
2. seek EOF and read trailer plus seal;
3. read footer bytes only;
4. compile `CompiledAuraPlan`;
5. range-read or map `[body_offset, footer_offset)`;
6. scan fixed-width rows using plan-derived record width and field offsets.

The current v1 implementation has a file-range backend. It proves the
no-full-file-copy property with `source_kind=file_range`,
`body_bytes_read_at_open=0`, and `full_file_bytes_copied=0`.

Mmap could remove per-chunk read copies for replay and borrowed batch views.
It adds platform and safety surface: the mapped file must not be mutated while
borrowed slices are alive, and the API must make source lifetimes explicit. The
next mmap experiment should be benchmark-gated against the current file-range
backend before becoming a default.

## Arrow-Style Repeated Values And Grouping

Arrow run-end encoding demonstrates that repeated values can be exposed as runs
without changing logical rows. For Aura1 this maps cleanly to opt-in grouped
replay:

- grouping is consecutive-run only;
- row order is preserved;
- group fields are selected by schema field name or field ID;
- high-cardinality inputs degrade to one-row groups;
- grouped replay changes callback semantics, so it is not directly equivalent
  to per-row replay.

The current grouped API proves the surface. The next performance step is to
compare key bytes directly from fixed-width rows and materialize `AuraValue`
keys only once per emitted group.

## Aura1 Parse Hot Loop

Current replay is much slower than raw scan. Fresh results show raw file-range
scan around 7 GiB/s on `sdk-larger`, while per-row replay is around 0.8-0.9
GiB/s. That gap is expected because replay does more work:

- decodes each field into i64 values;
- uses a visitor callback once per row;
- forms a temporary decoded row representation for each callback;
- grouped replay currently allocates a key vector per row;
- row batches allocate row vectors and `AuraValue` cells;
- column batches allocate columns but still decode every field.

The body layout itself has headroom. The main optimization targets are callback
frequency, row-view borrowing, key comparison, and materialization strategy.

## Precomputation Opportunities

Already available through `CompiledAuraPlan`:

- record count;
- record width;
- body size;
- field count;
- field offsets and widths through the Aura1 decode program;
- conversion plan hash.

Can be compiled at reader open:

- group key field indexes;
- group key byte offsets and widths;
- batch row range bounds;
- field load recipes for typed/borrowed views.

Should not happen per row:

- schema field lookup;
- group field resolution;
- allocation of group key values;
- allocation of temporary key vectors;
- full-file buffering for file-backed replay.

Still happens per row in current replay:

- field endian loads into i64 row values;
- visitor callback;
- optional group key comparison;
- batch materialization when requested.

## Ranked Experiments

1. **Byte-key grouped replay**: compare group key bytes directly from
   fixed-width row slices; materialize group key values only at group boundary.
2. **Batch callback replay**: add an Aura1 fixed batch callback API that invokes
   one callback per row chunk instead of one callback per row.
3. **Borrowed row/batch view**: expose fixed-width body slices with typed
   accessors to avoid `Vec<i64>` row formation where callers can borrow.
4. **Operation aliases and counters**: make benchmark operation names and JSON
   counters distinguish raw scan, replay, batch replay, and grouped replay.
5. **Mmap backend prototype**: compare mmap replay against file-range replay
   after borrowed views exist.
6. **Columnar field-by-field decode**: decode one field into typed columns at a
   time and avoid row intermediate state.
7. **Bounds-check reduction**: validate range once per chunk and avoid repeated
   per-field slice checks where safe.
8. **Endian load helpers**: benchmark direct fixed-width loads against current
   generic body visitor.
9. **Batch-size sweep**: benchmark 128, 1024, 8192, 65536 rows for row batches,
   column batches, and file-range replay chunks.
10. **Footer group index prototype**: only if on-the-fly byte-key grouping is
    still too slow and repeated-run workloads justify extra index bytes.
