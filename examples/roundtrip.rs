use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use aura_codec::sdk::{
    convert_aura, AuraFormat, AuraMetadata, AuraReader, AuraRecordBatch, AuraSchema, AuraType,
    AuraValue, AuraWriter, ConvertOptions, SymbolMap, WriterOptions,
};

fn sdk_schema() -> AuraSchema {
    AuraSchema::named("maintainability_roundtrip")
        .field("ts_event", AuraType::TimestampNanos)
        .field("instrument_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("signed_delta", AuraType::I64)
        .field("event_flags", AuraType::FlagsU32)
        .build()
        .expect("valid synthetic schema")
}

fn sdk_rows() -> Vec<Vec<AuraValue>> {
    vec![
        vec![
            1_000_i64.into(),
            7_u64.into(),
            (-1_012_500_i64).into(),
            (-3_i64).into(),
            1_u64.into(),
        ],
        // Keep an identical consecutive row so both repetitions and order are visible.
        vec![
            1_000_i64.into(),
            7_u64.into(),
            (-1_012_500_i64).into(),
            (-3_i64).into(),
            1_u64.into(),
        ],
        vec![
            900_i64.into(),
            2_u64.into(),
            421_250_i64.into(),
            8_i64.into(),
            4_u64.into(),
        ],
        vec![
            1_000_i64.into(),
            7_u64.into(),
            (-1_012_499_i64).into(),
            (-3_i64).into(),
            1_u64.into(),
        ],
    ]
}

fn sdk_metadata() -> AuraMetadata {
    AuraMetadata::new()
        .with_dataset("maintainability-roundtrip")
        .with_source("synthetic")
        .with_venue("example")
        .with_writer_version("roundtrip-example")
        .with_symbol_map(SymbolMap::new().insert(2, "MSFT").insert(7, "AAPL"))
        .custom("purpose", "sdk-example")
        .expect("valid metadata")
}

fn write_sdk_file(
    schema: AuraSchema,
    rows: &[Vec<AuraValue>],
    options: WriterOptions,
) -> aura_codec::Result<Vec<u8>> {
    let batch = AuraRecordBatch::new(schema.clone(), rows.to_vec())?;
    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, options)?;
    writer.write_batch(batch)?;
    writer.finish()?;
    Ok(bytes)
}

fn assert_sdk_file(
    bytes: &[u8],
    expected_format: AuraFormat,
    schema: &AuraSchema,
    metadata: &AuraMetadata,
    rows: &[Vec<AuraValue>],
) -> aura_codec::Result<()> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    assert_eq!(expected_format, reader.format());
    assert_eq!(schema, reader.schema());
    assert_eq!(metadata, reader.metadata());

    let batches = reader.read_batches()?;
    assert_eq!(1, batches.len());
    assert_eq!(rows, batches[0].rows());
    Ok(())
}

fn convert_sdk_file(
    source: &[u8],
    target: AuraFormat,
    schema: &AuraSchema,
    metadata: &AuraMetadata,
    rows: &[Vec<AuraValue>],
) -> aura_codec::Result<Vec<u8>> {
    let mut output = Vec::new();
    let summary = convert_aura(
        Cursor::new(source),
        &mut output,
        ConvertOptions::new(target).verify(true),
    )?;
    assert_eq!(target, summary.target_format);
    assert_eq!(rows.len(), summary.record_count);
    assert!(summary.verified);
    assert_sdk_file(&output, target, schema, metadata, rows)?;
    Ok(output)
}

fn write_fixtures(output_dir: &Path, source: &[u8], aura0: &[u8], aura1: &[u8]) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("create fixture directory {}", output_dir.display()))?;
    for (name, bytes) in [
        ("roundtrip.aura", source),
        ("roundtrip.aura0", aura0),
        ("roundtrip.aura1", aura1),
    ] {
        let path = output_dir.join(name);
        let mut file = fs::File::create_new(&path)
            .with_context(|| format!("create fixture {}", path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write fixture {}", path.display()))?;
    }
    Ok(())
}

fn output_dir() -> Result<Option<PathBuf>> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let output_dir = args.next().map(PathBuf::from);
    if args.next().is_some() {
        bail!("usage: cargo run --example roundtrip [output-dir]");
    }
    Ok(output_dir)
}

fn main() -> Result<()> {
    let output_dir = output_dir()?;
    let schema = sdk_schema();
    let rows = sdk_rows();
    let metadata = sdk_metadata();
    let source = write_sdk_file(
        schema.clone(),
        &rows,
        WriterOptions::new(AuraFormat::Aura).metadata(metadata.clone()),
    )?;
    assert_sdk_file(&source, AuraFormat::Aura, &schema, &metadata, &rows)?;

    let aura0 = convert_sdk_file(&source, AuraFormat::Aura0, &schema, &metadata, &rows)?;
    let aura1 = convert_sdk_file(&source, AuraFormat::Aura1, &schema, &metadata, &rows)?;

    // Check both compiled directions as well as ingest-to-profile conversion.
    let _aura1_from_aura0 = convert_sdk_file(&aura0, AuraFormat::Aura1, &schema, &metadata, &rows)?;
    let _aura0_from_aura1 = convert_sdk_file(&aura1, AuraFormat::Aura0, &schema, &metadata, &rows)?;

    if let Some(output_dir) = output_dir.as_deref() {
        write_fixtures(output_dir, &source, &aura0, &aura1)?;
        println!("fixtures_dir={}", output_dir.display());
    }

    println!(
        "sdk_roundtrip rows={} source_bytes={} aura0_bytes={} aura1_bytes={}",
        rows.len(),
        source.len(),
        aura0.len(),
        aura1.len()
    );
    Ok(())
}
