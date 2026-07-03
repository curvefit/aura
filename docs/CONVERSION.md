# Aura Conversion API

Use `convert_aura(input, output, ConvertOptions::new(target_format))` for SDK-level format conversion.

```rust
let summary = aura_codec::convert_aura(
    input,
    output,
    aura_codec::ConvertOptions::new(aura_codec::AuraFormat::Aura1).verify(true),
)?;
println!("rows={}", summary.record_count);
# Ok::<(), aura_codec::AuraError>(())
```

Supported v1 conversions:

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

The conversion API builds and exposes the same schema-derived plan used by the writer and reader. Verification is off by default. Unsupported source formats, target formats, schemas, and byte-lane requests return `AuraError` instead of falling back silently.
