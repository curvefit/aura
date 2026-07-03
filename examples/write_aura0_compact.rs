use aura_codec::{AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("order_events")
        .field("ts_event", AuraType::TimestampNanos)
        .field("instrument_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("quantity", AuraType::U64)
        .field("event_flags", AuraType::FlagsU32)
        .build()?;
    let rows = vec![
        vec![
            AuraValue::I64(1_700_000_000_000_000_000),
            AuraValue::U64(7),
            AuraValue::I64(101_2500),
            AuraValue::U64(50),
            AuraValue::U64(0),
        ],
        vec![
            AuraValue::I64(1_700_000_000_001_000_000),
            AuraValue::U64(7),
            AuraValue::I64(101_2600),
            AuraValue::U64(75),
            AuraValue::U64(4),
        ],
    ];
    let batch = AuraRecordBatch::new(schema.clone(), rows)?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura0_compact())?;
    writer.write_batch(batch)?;
    let summary = writer.finish()?;

    println!(
        "wrote format={} rows={} bytes={}",
        summary.format.as_str(),
        summary.row_count,
        summary.output_bytes
    );
    Ok(())
}
