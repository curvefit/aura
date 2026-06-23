use aura_codec::{AuraColumnBatch, AuraReader, AuraSchema, AuraType, AuraWriter, WriterOptions};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("columnar_market_batch")
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 9 })
        .field("size", AuraType::U64)
        .field("side", AuraType::EnumU8)
        .field("flags", AuraType::FlagsU32)
        .build()?;
    let batch = AuraColumnBatch::builder(schema.clone())
        .u32("flags", vec![0, 1, 0])
        .u8("side", vec![1, 2, 1])
        .u64("size", vec![10, 20, 30])
        .i64(
            "price",
            vec![42_100_000_000, 42_120_000_000, 42_110_000_000],
        )
        .u32("symbol_id", vec![101, 101, 202])
        .i64(
            "ts_event",
            vec![
                1_700_000_000_000_000_000,
                1_700_000_000_000_500_000,
                1_700_000_000_001_000_000,
            ],
        )
        .build()?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1())?;
    writer.write_batch(batch)?;
    writer.finish()?;

    let reader = AuraReader::open(std::io::Cursor::new(&bytes))?;
    println!(
        "columnar write fields={} rows={}",
        reader.schema().field_count(),
        reader.read_batches()?[0].row_count()
    );
    Ok(())
}
