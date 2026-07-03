use aura_codec::{
    AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions,
};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("replay_example")
        .field("ts", AuraType::TimestampNanos)
        .field("symbol", AuraType::U32)
        .field("price", AuraType::I64)
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        vec![
            vec![
                AuraValue::I64(100),
                AuraValue::U64(1),
                AuraValue::I64(10_000),
            ],
            vec![
                AuraValue::I64(200),
                AuraValue::U64(2),
                AuraValue::I64(10_005),
            ],
        ],
    )?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1())?;
    writer.write_batch(batch)?;
    writer.finish()?;

    let reader = AuraReader::open(std::io::Cursor::new(&bytes))?;
    let mut last_price = 0;
    let rows = reader.replay_i64(|row| {
        last_price = row[2];
        Ok(())
    })?;

    println!("replayed rows={rows} last_price={last_price}");
    Ok(())
}
