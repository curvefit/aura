# Aura SDK Errors

SDK APIs return `aura_codec::Result<T>`, an alias for `Result<T, AuraError>`.

Common validation errors:

- duplicate schema field names
- duplicate field IDs
- unsupported physical types such as floats, UTF-8, and binary payloads
- nullable fields, until the format has explicit presence/offset support
- missing, extra, or type-mismatched columns in `AuraColumnBatch`
- row values outside the declared integer width
- unsupported Aura0 byte-lane selection or codec
- corrupt footer, unsupported schema block, truncated input, or malformed sealed container

Unsupported features are rejected before writing whenever the SDK can know the schema or value shape. Reader and converter errors are intentionally explicit; the SDK does not silently reinterpret a schema as a different physical layout.

For v1, these are deliberate limitations rather than partial implementations:

- variable-width `Utf8` and `Binary`
- nullable fields
- `F32` and `F64`
- true zero-copy/block-streaming `AuraReader`

The reader exposes a batch-iteration API today, but internally it still decodes through the existing generic row engine before serving batches.

The standalone V3 exact-value block reports the same typed `AuraError` family.
It rejects stale schema IDs, grouped/repeated schemas in flat block v1, wrong
stable slots or exact types, unknown flags, noncanonical validity
lengths/padding, nonzero fixed null placeholders, nonempty variable null
ranges, malformed/nonmonotonic offsets, invalid UTF-8 or DecimalTextV1,
inconsistent plane/total lengths, truncation, trailing bytes, and configured or
hard resource-limit violations. Lengths and arithmetic are checked before
fallible allocation. A caller may use lower `V3ValueLimits`; values above the
hard 1 GiB block, 16 MiB raw value, and 16,777,216-row ceilings never raise
them.

`AuraV3Column::value_ref` returns `Ok(None)` only for a valid logical null. A
row index at or beyond the column length returns
`InvalidValue("v3 value row index")`; malformed validity, offset, or UTF-8 data
also returns an error rather than being presented as null.
