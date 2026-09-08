# Aura Conversion API

Use `convert_aura(input, output, ConvertOptions::new(target_format))` for the
V2 SDK-level format conversion API.

```rust
let summary = aura_codec::convert_aura(
    input,
    output,
    aura_codec::ConvertOptions::new(aura_codec::AuraFormat::Aura1).verify(true),
)?;
println!("rows={}", summary.record_count);
# Ok::<(), aura_codec::AuraError>(())
```

Supported V2 conversions:

- `.aura` to `.aura0`
- `.aura` to `.aura1`
- `.aura0` to `.aura1`
- `.aura1` to `.aura0`
- conversion back to sealed `.aura`

`ConvertOptions` controls:

- `target_format`
- Aura0 profile for Aura0 outputs
- byte-lane codec and lane selection for Aura0 profiles that support byte lanes
- verification by decoding the output and comparing rows with the input

The conversion API uses the same V2 schema and compiled conversion paths as the
writer and reader, but it does not expose a `CompiledAuraPlan` in
`ConversionSummary`. The current helper buffers the complete input and output;
`record_count`, `output_bytes`, `schema_hash`, and `verified` summarize the
operation. Verification is off by default. Unsupported source formats, target
formats, schemas, and byte-lane requests return `AuraError` instead of falling
back silently.

The explicit V3 flat, grouped, and planned Aura0 files are outside this V2
conversion matrix. They carry their own V3 header/footer and schema contracts;
the V2 converter rejects V3 schema/container inputs rather than relabeling or
transcoding them. Use the V3 CLI/API seal and verify routes described in
`docs/SHADOW_PROTOCOL.md` and `docs/FORMAT.md`.
