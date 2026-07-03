use aura_codec::{
    AuraProfile, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions,
};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("hybrid_profile_example")
        .field("ts", AuraType::TimestampNanos)
        .field("symbol", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        vec![
            vec![
                AuraValue::I64(1_000),
                AuraValue::U64(1),
                AuraValue::I64(123_4500),
            ],
            vec![
                AuraValue::I64(2_000),
                AuraValue::U64(1),
                AuraValue::I64(123_4600),
            ],
        ],
    )?;

    let mut bytes = Vec::new();
    let options = WriterOptions::aura0_compact().profile(AuraProfile::Hybrid);
    let mut writer = AuraWriter::try_new(&mut bytes, schema, options)?;
    writer.write_batch(batch)?;
    let summary = writer.finish()?;

    println!(
        "wrote hybrid rows={} bytes={}",
        summary.row_count, summary.output_bytes
    );
    Ok(())
}
