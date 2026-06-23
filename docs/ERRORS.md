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
