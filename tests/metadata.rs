use std::io::Cursor;

use aura_codec::{
    AuraMetadata, AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter,
    OrderBookDeltaSpec, SymbolMap, WriterOptions,
};

fn schema() -> AuraSchema {
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

fn rows() -> Vec<Vec<AuraValue>> {
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
            22_u64.into(),
            99_500_000_000_i64.into(),
            10_u64.into(),
            2_u64.into(),
            4_u64.into(),
        ],
    ]
}

fn metadata() -> AuraMetadata {
    AuraMetadata::new()
        .with_dataset("unit-market")
        .with_source("fixture")
        .with_venue("XNAS")
        .with_writer_version("aura-test")
        .with_symbol_map(SymbolMap::new().insert(10, "AAPL").insert(22, "MSFT"))
        .custom("session", "regular")
        .unwrap()
}

fn write(options: WriterOptions) -> Vec<u8> {
    let schema = schema();
    let batch = AuraRecordBatch::new(schema.clone(), rows()).unwrap();
    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, options).unwrap();
    writer.write_batch(batch).unwrap();
    writer.finish().unwrap();
    bytes
}

#[test]
fn metadata_roundtrips_aura1() {
    let expected = metadata();
    let bytes = write(WriterOptions::aura1().metadata(expected.clone()));
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();

    assert_eq!(&expected, reader.metadata());
    assert_eq!(Some("unit-market"), reader.metadata().dataset());
    assert_eq!(Some("fixture"), reader.metadata().source());
    assert_eq!(Some("XNAS"), reader.metadata().venue());
}

#[test]
fn metadata_roundtrips_aura0() {
    let expected = metadata();
    let bytes = write(WriterOptions::aura0_compact().metadata(expected.clone()));
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();

    assert_eq!(&expected, reader.metadata());
}

#[test]
fn symbol_map_roundtrips() {
    let bytes = write(WriterOptions::aura1().metadata(metadata()));
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
    let symbols = reader.metadata().symbol_map();

    assert_eq!(Some("AAPL"), symbols.resolve(10));
    assert_eq!(Some("MSFT"), symbols.resolve(22));
    assert_eq!(None, symbols.resolve(999));
}

#[test]
fn reader_metadata_available_before_replay() {
    let bytes = write(WriterOptions::aura1().metadata(metadata()));
    let mut reader = AuraReader::open(Cursor::new(bytes)).unwrap();

    assert_eq!(Some("unit-market"), reader.metadata().dataset());
    assert_eq!(0, reader.stats().rows_scanned);
    assert!(reader.next_fixed_batch(2).unwrap().is_some());
}

#[test]
fn orderbook_replay_does_not_string_lookup_hot_loop() {
    let bytes = write(WriterOptions::aura1().metadata(metadata()));
    let schema = schema();
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("ts_event")
        .instrument("symbol_id")
        .side("side")
        .price("price")
        .size("size")
        .flags_optional("flags")
        .build(&schema)
        .unwrap();
    let mut checksum = 0u64;
    reader
        .replay_orderbook_deltas(2, &spec, |batch| {
            for row in 0..batch.row_count() {
                checksum = checksum.wrapping_add(batch.instrument(row)? as u64);
            }
            Ok(())
        })
        .unwrap();

    assert_ne!(0, checksum);
    assert_eq!(0, reader.stats().symbol_string_lookup_count);
    assert_eq!(Some("AAPL"), reader.metadata().symbol_map().resolve(10));
}

#[test]
fn unknown_metadata_policy() {
    let bytes = write(WriterOptions::aura1());
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();

    assert!(reader.metadata().is_empty());
    assert!(!reader.header_comment().is_empty());
}
