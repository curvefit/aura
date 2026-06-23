use aura_codec::{AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 9 })
        .field("size", AuraType::U64)
        .field("side", AuraType::EnumU8)
        .field("flags", AuraType::FlagsU32)
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        vec![
            vec![
                AuraValue::I64(1_700_000_000_000_000_000),
                AuraValue::U64(101),
                AuraValue::I64(42_100_000_000),
                AuraValue::U64(10),
                AuraValue::U64(1),
                AuraValue::U64(0),
            ],
            vec![
                AuraValue::I64(1_700_000_000_000_500_000),
                AuraValue::U64(101),
                AuraValue::I64(42_150_000_000),
                AuraValue::U64(12),
                AuraValue::U64(2),
                AuraValue::U64(1),
            ],
        ],
    )?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1())?;
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
