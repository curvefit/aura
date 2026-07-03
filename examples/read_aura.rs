use std::io::Cursor;

use aura_codec::{
    AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions,
};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("read_example")
        .field("ts", AuraType::TimestampMicros)
        .field("venue", AuraType::U16)
        .field("price_ticks", AuraType::I64)
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        vec![
            vec![
                AuraValue::I64(1_000_000),
                AuraValue::U64(12),
                AuraValue::I64(25_000),
            ],
            vec![
                AuraValue::I64(1_001_000),
                AuraValue::U64(12),
                AuraValue::I64(25_010),
            ],
        ],
    )?;

    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1())?;
    writer.write_batch(batch)?;
    writer.finish()?;

    let reader = AuraReader::open(Cursor::new(&bytes))?;
    let batches = reader.read_batches()?;

    println!(
        "read format={} fields={} rows={}",
        reader.format().as_str(),
        reader.schema().field_count(),
        batches[0].row_count()
    );
    Ok(())
}
