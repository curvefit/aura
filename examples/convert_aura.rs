use std::io::Cursor;

use aura_codec::{
    convert_aura, AuraFormat, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter,
    ConvertOptions, WriterOptions,
};

fn main() -> aura_codec::Result<()> {
    let schema = AuraSchema::named("conversion_example")
        .field("ts", AuraType::TimestampNanos)
        .field("symbol", AuraType::U32)
        .field("qty", AuraType::U64)
        .build()?;
    let batch = AuraRecordBatch::new(
        schema.clone(),
        vec![
            vec![
                AuraValue::I64(1_700_000_000_000_000_000),
                AuraValue::U64(1),
                AuraValue::U64(100),
            ],
            vec![
                AuraValue::I64(1_700_000_000_001_000_000),
                AuraValue::U64(2),
                AuraValue::U64(150),
            ],
        ],
    )?;

    let mut aura0 = Vec::new();
    let mut writer = AuraWriter::try_new(&mut aura0, schema, WriterOptions::aura0_compact())?;
    writer.write_batch(batch)?;
    writer.finish()?;

    let mut aura1 = Vec::new();
    let summary = convert_aura(
        Cursor::new(&aura0),
        &mut aura1,
        ConvertOptions::new(AuraFormat::Aura1).verify(true),
    )?;

    println!(
        "converted {} -> {} rows={} bytes={}",
        summary.source_format.as_str(),
        summary.target_format.as_str(),
        summary.record_count,
        summary.output_bytes
    );
    Ok(())
}
