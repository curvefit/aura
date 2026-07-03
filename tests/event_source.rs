use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aura_codec::{
    AuraEventBatch, AuraEventSource, AuraFileSource, AuraFormat, AuraLiveFrameSource,
    AuraLiveSource, AuraMemorySource, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter,
    OrderBookDeltaSpec, Result, WriterOptions,
};

fn market_schema() -> AuraSchema {
    AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 9 })
        .field("size", AuraType::U64)
        .field("side", AuraType::EnumU8)
        .field("flags", AuraType::FlagsU32)
        .build()
        .unwrap()
}

fn market_rows() -> Vec<Vec<AuraValue>> {
    vec![
        vec![
            1_700_000_000_000_000_000_i64.into(),
            10_u64.into(),
            101_000_000_000_i64.into(),
            25_u64.into(),
            1_u64.into(),
            0_u64.into(),
        ],
        vec![
            1_700_000_000_001_000_000_i64.into(),
            10_u64.into(),
            101_250_000_000_i64.into(),
            33_u64.into(),
            2_u64.into(),
            4_u64.into(),
        ],
        vec![
            1_700_000_000_002_000_000_i64.into(),
            22_u64.into(),
            99_500_000_000_i64.into(),
            10_u64.into(),
            1_u64.into(),
            1_u64.into(),
        ],
    ]
}

fn write_aura1(schema: AuraSchema, rows: Vec<Vec<AuraValue>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let batch = AuraRecordBatch::new(schema.clone(), rows).unwrap();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1()).unwrap();
    writer.write_batch(batch).unwrap();
    writer.finish().unwrap();
    bytes
}

fn write_aura_ingest(schema: AuraSchema, rows: Vec<Vec<AuraValue>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let batch = AuraRecordBatch::new(schema.clone(), rows).unwrap();
    let mut writer =
        AuraWriter::try_new(&mut bytes, schema, WriterOptions::new(AuraFormat::Aura)).unwrap();
    writer.write_batch(batch).unwrap();
    writer.finish().unwrap();
    bytes
}

fn write_temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aura_event_source_{name}_{}_{}.aura1",
        std::process::id(),
        nanos
    ));
    fs::write(&path, bytes).unwrap();
    path
}

fn aura1_body(bytes: &[u8]) -> Vec<u8> {
    let layout = aura_codec::records::aura1_fixed_layout_info(bytes).unwrap();
    bytes[layout.body_offset..layout.body_offset + layout.body_bytes].to_vec()
}

fn consume_source<S>(source: &mut S) -> Result<(usize, u64)>
where
    S: AuraEventSource,
    for<'a> S::Batch<'a>: AuraEventBatch,
{
    let mut rows = 0usize;
    let mut checksum = 0u64;
    while let Some(batch) = source.next_batch()? {
        rows = rows.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.checksum_all_fields()?);
    }
    Ok((rows, checksum))
}

#[test]
fn historical_memory_and_file_sources_share_event_source_api() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_aura1(schema.clone(), rows);
    let path = write_temp_file("historical", &aura1);

    let mut memory = AuraMemorySource::try_new(aura1.clone(), 2).unwrap();
    assert_eq!(schema.fields(), memory.schema().fields());
    assert_eq!(schema.hash(), memory.compiled_plan().schema_hash);
    let record_width = memory.compiled_plan().aura1_record_width;
    assert!(record_width > 0);
    let memory_result = consume_source(&mut memory).unwrap();

    let mut file = AuraFileSource::open_path(&path, 2).unwrap();
    assert_eq!(schema.fields(), file.schema().fields());
    assert_eq!(schema.hash(), file.compiled_plan().schema_hash);
    assert_eq!(record_width, file.compiled_plan().aura1_record_width);
    let file_result = consume_source(&mut file).unwrap();

    assert_eq!(memory_result, file_result);
    assert_eq!(3, memory_result.0);
    assert_ne!(0, memory_result.1);
    fs::remove_file(path).unwrap();
}

#[test]
fn live_source_consumes_aura1_body_with_same_event_loop() {
    let schema = market_schema();
    let rows = market_rows();
    let aura1 = write_aura1(schema.clone(), rows);
    let body = aura1_body(&aura1);

    let mut historical = AuraMemorySource::try_new(aura1, 2).unwrap();
    let live_plan = historical.compiled_plan().clone();
    let mut live = AuraLiveSource::with_plan(Cursor::new(body), schema, live_plan, 2).unwrap();

    assert!(historical.compiled_plan().aura1_record_width > 0);
    assert_eq!(
        historical.compiled_plan().aura1_record_width,
        live.compiled_plan().aura1_record_width
    );
    assert_eq!(
        consume_source(&mut historical).unwrap(),
        consume_source(&mut live).unwrap()
    );
}

#[test]
fn event_sources_reject_invalid_profiles_and_batch_sizes() {
    let schema = market_schema();
    let rows = market_rows();
    let ingest = write_aura_ingest(schema.clone(), rows);

    assert!(AuraMemorySource::try_new(ingest, 2).is_err());
    assert!(AuraLiveSource::try_new(Cursor::new(Vec::<u8>::new()), schema, 0).is_err());
}

#[test]
fn event_source_file_zero_copy_stats() {
    let schema = market_schema();
    let aura1 = write_aura1(schema, market_rows());
    let path = write_temp_file("file_zero_copy", &aura1);

    let mut source = AuraFileSource::open_path(&path, 2).unwrap();
    let result = consume_source(&mut source).unwrap();
    let stats = source.source_stats();

    assert_eq!(3, result.0);
    assert_eq!("file_range", stats.source_kind);
    assert_eq!(0, stats.rows_materialized);
    assert!(!stats.full_file_materialized);
    assert_eq!(0, stats.full_file_bytes_copied);
    assert_eq!(0, stats.bytes_copied);
    assert!(stats.bytes_borrowed_or_ranged > 0);
    fs::remove_file(path).unwrap();
}

#[test]
fn event_source_memory_zero_copy_stats() {
    let schema = market_schema();
    let aura1 = write_aura1(schema, market_rows());
    let aura1_len = aura1.len();

    let mut source = AuraMemorySource::try_new(aura1, 2).unwrap();
    let result = consume_source(&mut source).unwrap();
    let stats = source.source_stats();

    assert_eq!(3, result.0);
    assert_eq!("memory", stats.source_kind);
    assert_eq!(0, stats.rows_materialized);
    assert!(!stats.full_file_materialized);
    assert_eq!(aura1_len, stats.full_file_bytes_copied);
    assert_eq!(aura1_len, stats.bytes_copied);
    assert!(stats.bytes_borrowed_or_ranged > 0);
}

#[test]
fn event_source_live_bounded_buffer_stats() {
    let schema = market_schema();
    let aura1 = write_aura1(schema.clone(), market_rows());
    let body = aura1_body(&aura1);
    let historical = AuraMemorySource::try_new(aura1, 2).unwrap();

    let mut source =
        AuraLiveFrameSource::from_body_chunks(body, schema, historical.compiled_plan().clone(), 2)
            .unwrap();
    let result = consume_source(&mut source).unwrap();
    let stats = source.source_stats();

    assert_eq!(3, result.0);
    assert_eq!("live_frame", stats.source_kind);
    assert!(stats.buffer_reuse_enabled);
    assert_eq!(0, stats.rows_materialized);
    assert!(!stats.full_file_materialized);
    assert_eq!(0, stats.full_file_bytes_copied);
    assert_eq!(0, stats.bytes_copied);
    assert!(stats.bytes_borrowed_or_ranged > 0);
    assert_eq!(0, stats.allocations_proxy);
}

#[test]
fn orderbook_delta_batch_no_row_materialization() {
    let schema = market_schema();
    let aura1 = write_aura1(schema.clone(), market_rows());
    let reader = aura_codec::AuraReader::open(Cursor::new(aura1)).unwrap();
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("ts_event")
        .instrument("symbol_id")
        .side("side")
        .price("price")
        .size("size")
        .flags_optional("flags")
        .build(&schema)
        .unwrap();
    let mut rows = 0usize;
    let mut checksum = 0u64;
    reader
        .replay_orderbook_deltas(2, &spec, |batch| {
            rows = rows.saturating_add(batch.row_count());
            checksum = checksum.wrapping_add(batch.checksum_payload()?);
            Ok(())
        })
        .unwrap();
    let stats = reader.stats();

    assert_eq!(3, rows);
    assert_ne!(0, checksum);
    assert_eq!(0, stats.max_rows_materialized_at_once);
    assert_eq!(0, stats.open_decoded_row_count);
    assert_eq!(0, stats.full_file_materialized as usize);
}

#[test]
fn borrowed_batch_rejects_truncated_body() {
    let schema = market_schema();
    let aura1 = write_aura1(schema.clone(), market_rows());
    let mut body = aura1_body(&aura1);
    body.pop();
    let historical = AuraMemorySource::try_new(aura1, 2).unwrap();

    assert!(AuraLiveFrameSource::from_body_chunks(
        body,
        schema,
        historical.compiled_plan().clone(),
        2
    )
    .is_err());
}
