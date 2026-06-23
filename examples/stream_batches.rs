use aura_codec::{
    AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions,
};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("stream_batches_example")
        .field("ts", AuraType::TimestampMicros)
        .field("symbol", AuraType::U32)
        .field("qty", AuraType::U64)
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        (0..5)
            .map(|index| {
                let index_u64 = index as u64;
                vec![
                    AuraValue::I64(1_000_000 + i64::from(index) * 1000),
                    AuraValue::U64(index_u64 % 2),
                    AuraValue::U64(10 + index_u64),
                ]
            })
            .collect(),
    )?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1())?;
    writer.write_batch(batch)?;
    writer.finish()?;

    let mut reader = AuraReader::open(std::io::Cursor::new(&bytes))?;
    let mut batches = 0;
    let mut rows = 0;
    while let Some(batch) = reader.next_batch(2)? {
        batches += 1;
        rows += batch.row_count();
    }
    println!("streamed batches={batches} rows={rows}");
    Ok(())
}
