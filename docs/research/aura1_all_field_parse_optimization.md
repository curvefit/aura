# Aura1 All-Field Parse Optimization

## Current shape

The current true all-field benchmark is honest but slow. On `sdk-larger`, raw body scan
is about 6.2 GB/s, batch view construction is about 5.7 GB/s, one-field parse is
about 2.6 GB/s, and all-field parse is about 0.78 GB/s. The gap is not file I/O or
layout discovery: Aura1 already gets `body_offset`, `footer_offset`, `record_count`,
`record_width`, field offsets, and field widths from the header, footer, and
`CompiledAuraPlan`.

The all-field path currently loops row-major. For every row it creates a row slice,
then for every field it calls the fixed-width accessor. That accessor performs
offset arithmetic, checked slicing, width dispatch, endian conversion, and checksum
mixing. There is no `AuraValue` allocation in the true parse benchmark, and no field
name lookup in the hot loop. The expensive work is repeated safe access machinery
and branchy width dispatch per decoded value.

## Questions answered

1. One-field parse is faster because it performs one strided load per row and one
   checksum mix per row. All-field parse performs `field_count` loads, width checks,
   field slice checks, and checksum mixes per row.
2. The likely bottleneck is a combination of row-major nested access, per-value
   checked slicing, width dispatch, endian loads, and checksum mixing. Callback cost
   is already avoided in batch all-field parse.
3. All-field parse currently loops row-major.
4. The true parse path does not use `AuraValue`; row-batch materialization does.
5. The true parse path does not allocate per row. It constructs borrowed views and
   reads slices.
6. There is no trait-object dispatch per cell, but there is interpreted metadata
   dispatch through width branches and field recipe iteration.
7. Safe slice indexing is present at row and field level, so repeated bounds checks
   are plausible.
8. Public accessor methods may inhibit some optimization because each call redoes
   validation/error plumbing and width matching.
9. Schema lookup is not in the hot loop; plan slots are used.
10. `CompiledAuraPlan` can feed a tighter parse program: record width, body length,
    field offsets, field widths, and grouped load recipes.
11. Physical widths can be grouped into load kernels so width dispatch happens once
    per group rather than once per value.
12. Field-major decode can move field recipe dispatch outside the row loop and make
    selected-field parsing explicit.

## Ranked experiments

1. Field-major all-field checksum: dispatch once per field, then stride rows.
2. Checked-once fixed-width load helpers: validate body/fields up front, then use
   raw offsets in tight loops.
3. Width-specialized kernel groups: group fields by width and run 8/4/2/1 byte loops.
4. Parse instruction tape: prebuild compact load ops from `CompiledAuraPlan`.
5. Selected-field batch API: validate selected fields once and skip unused fields.
6. Columnar materialization field-major rewrite: build typed columns field by field.
7. Grouped true-parse optimization: compare key bytes and aggregate selected fields.
8. Bounds-check reduction in existing row-major batch checksum.
9. Unchecked unaligned loads behind one safe validation boundary.
10. Batch-size sensitivity for true parse with 128, 1024, 8192, and 65536 rows.

## Acceptance bar

The pass condition is a true all-field parse path at or above 2 GB/s on
`sdk-larger`. A true all-field path must read every schema field for every row and
feed every value into a checksum or `black_box`.
