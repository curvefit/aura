use aura_codec::random_verify::{
    generate_case, generate_orderbook_case, verify_case, verify_corruption_rejects,
    verify_orderbook_case, verify_unsupported_features_reject, RandomVerifyConfig,
};
use aura_codec::{AuraFormat, AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue};
use std::io::Cursor;

const QUICK_SEEDS: usize = 100;
const QUICK_MAX_RECORDS: usize = 1024;
const BASE_SEED: u64 = 0xa0a0_2026_0625;

fn seed_base() -> u64 {
    std::env::var("AURA_RANDOM_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(BASE_SEED)
}

fn quick_config() -> RandomVerifyConfig {
    RandomVerifyConfig {
        seed: seed_base(),
        cases: QUICK_SEEDS,
        max_records: QUICK_MAX_RECORDS,
        check_cross_format: true,
        check_streaming: true,
        check_replay: true,
        check_orderbook: false,
    }
}

#[test]
fn randomized_aura1_roundtrip() {
    let config = RandomVerifyConfig {
        check_cross_format: false,
        check_streaming: false,
        check_replay: false,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_aura0_roundtrip() {
    let config = RandomVerifyConfig {
        check_cross_format: false,
        check_streaming: false,
        check_replay: false,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + 10_000 + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_cross_format_roundtrip() {
    let config = RandomVerifyConfig {
        check_streaming: false,
        check_replay: false,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + 20_000 + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_streaming_batches() {
    let config = RandomVerifyConfig {
        check_cross_format: false,
        check_replay: false,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + 30_000 + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_replay_hash() {
    let config = RandomVerifyConfig {
        check_cross_format: false,
        check_streaming: false,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + 40_000 + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_orderbook_replay_if_schema_compatible() {
    for offset in 0..QUICK_SEEDS {
        let seed = seed_base() + 50_000 + offset as u64;
        let case = generate_orderbook_case(seed, QUICK_MAX_RECORDS).unwrap();
        verify_orderbook_case(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_grouped_replay_if_available() {
    let config = RandomVerifyConfig {
        check_cross_format: false,
        check_streaming: false,
        check_replay: true,
        ..quick_config()
    };
    for offset in 0..QUICK_SEEDS {
        let seed = config.seed + 60_000 + offset as u64;
        let case = generate_case(seed, config.max_records).unwrap();
        verify_case(&case, &config).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_unsupported_features_reject() {
    verify_unsupported_features_reject().unwrap();
}

#[test]
fn randomized_corrupt_aura1_rejects() {
    for offset in 0..25 {
        let seed = seed_base() + 70_000 + offset as u64;
        let case = generate_case(seed, QUICK_MAX_RECORDS).unwrap();
        verify_corruption_rejects(&case, AuraFormat::Aura1)
            .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_corrupt_aura0_rejects() {
    for offset in 0..25 {
        let seed = seed_base() + 80_000 + offset as u64;
        let case = generate_case(seed, QUICK_MAX_RECORDS).unwrap();
        verify_corruption_rejects(&case, AuraFormat::Aura0)
            .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_corrupt_footer_rejects() {
    for offset in 0..25 {
        let seed = seed_base() + 90_000 + offset as u64;
        let case = generate_case(seed, QUICK_MAX_RECORDS).unwrap();
        verify_corruption_rejects(&case, AuraFormat::Aura1)
            .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        verify_corruption_rejects(&case, AuraFormat::Aura0)
            .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn randomized_invalid_orderbook_schema_rejects() {
    let schema = AuraSchema::named("invalid_random_orderbook")
        .field("clock_ns", AuraType::TimestampNanos)
        .field("instrument_key", AuraType::U32)
        .field("book_side_code", AuraType::EnumU8)
        .field("qty_units", AuraType::I64)
        .build()
        .unwrap();
    let error = aura_codec::OrderBookDeltaSpec::builder()
        .timestamp("clock_ns")
        .instrument("instrument_key")
        .side("book_side_code")
        .price("missing_price")
        .size("qty_units")
        .build(&schema)
        .unwrap_err();
    assert!(error.to_string().contains("price"));

    let schema = AuraSchema::named("invalid_random_side")
        .field("clock_ns", AuraType::TimestampNanos)
        .field("instrument_key", AuraType::U32)
        .field("book_side_code", AuraType::I64)
        .field("limit_px", AuraType::PriceI64Scaled { scale: 4 })
        .field("qty_units", AuraType::I64)
        .build()
        .unwrap();
    let rows = vec![vec![
        AuraValue::I64(1),
        AuraValue::U64(7),
        AuraValue::I64(300),
        AuraValue::I64(100),
        AuraValue::I64(10),
    ]];
    let mut bytes = Vec::new();
    let mut writer = aura_codec::AuraWriter::try_new(
        &mut bytes,
        schema.clone(),
        aura_codec::WriterOptions::aura1(),
    )
    .unwrap();
    writer
        .write_batch(AuraRecordBatch::new(schema.clone(), rows).unwrap())
        .unwrap();
    writer.finish().unwrap();
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
    let spec = aura_codec::OrderBookDeltaSpec::builder()
        .timestamp("clock_ns")
        .instrument("instrument_key")
        .side("book_side_code")
        .price("limit_px")
        .size("qty_units")
        .build(&schema)
        .unwrap();
    let error = reader
        .replay_orderbook_deltas(1, &spec, |batch| {
            aura_codec::OrderBookDelta::try_new(
                batch.timestamp(0)?,
                batch.instrument(0)?,
                batch.side(0)?,
                batch.price(0)?,
                batch.size(0)?,
                0,
                0,
                0,
                0,
            )?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("book side"));
}

#[test]
#[ignore]
fn randomized_stress_1000_seeds() {
    let mut config = quick_config();
    config.cases = 1000;
    let report = aura_codec::random_verify::run_random_verification(config);
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
}
