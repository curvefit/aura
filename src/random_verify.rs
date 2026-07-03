use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::Instant;

use serde_json::json;

use crate::{
    convert_aura, AuraFormat, AuraProfile, AuraReader, AuraRecordBatch, AuraSchema, AuraType,
    AuraValue, AuraWriter, BookSide, BookStateHash, ConvertOptions, GroupBy, OrderBookApplyMode,
    OrderBookDelta, OrderBookDeltaSpec, OrderBookEngineKind, PreparedOrderBookApplyPlan,
    PreparedOrderBookEngine, Result, WriterOptions,
};

pub type VerifyResult<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub struct RandomVerifyConfig {
    pub seed: u64,
    pub cases: usize,
    pub max_records: usize,
    pub check_cross_format: bool,
    pub check_streaming: bool,
    pub check_replay: bool,
    pub check_orderbook: bool,
}

impl Default for RandomVerifyConfig {
    fn default() -> Self {
        Self {
            seed: 12_345,
            cases: 100,
            max_records: 1_024,
            check_cross_format: true,
            check_streaming: true,
            check_replay: true,
            check_orderbook: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomCaseReport {
    pub seed: u64,
    pub dataset_kind: String,
    pub schema_hash: u32,
    pub record_count: usize,
    pub field_count: usize,
    pub row_hash: u64,
}

#[derive(Debug, Clone)]
pub struct RandomVerifyReport {
    pub seed: u64,
    pub cases_requested: usize,
    pub cases_passed: usize,
    pub orderbook_cases_passed: usize,
    pub elapsed_ms: u128,
    pub failures: Vec<String>,
    pub cases: Vec<RandomCaseReport>,
}

impl RandomVerifyReport {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "seed": self.seed,
            "cases_requested": self.cases_requested,
            "cases_passed": self.cases_passed,
            "orderbook_cases_passed": self.orderbook_cases_passed,
            "elapsed_ms": self.elapsed_ms,
            "failures": self.failures,
            "cases": self.cases.iter().map(|case| json!({
                "seed": case.seed,
                "dataset_kind": case.dataset_kind,
                "schema_hash": case.schema_hash,
                "record_count": case.record_count,
                "field_count": case.field_count,
                "row_hash": format!("{:016x}", case.row_hash),
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedCase {
    pub seed: u64,
    pub dataset_kind: String,
    pub schema: AuraSchema,
    pub rows: Vec<Vec<AuraValue>>,
    pub row_hash: u64,
}

#[derive(Debug, Clone)]
pub struct GeneratedOrderBookCase {
    pub case: GeneratedCase,
    pub timestamp_name: String,
    pub instrument_name: String,
    pub side_name: String,
    pub price_name: String,
    pub size_name: String,
    pub action_name: Option<String>,
    pub sequence_name: Option<String>,
    pub order_id_name: Option<String>,
    pub flags_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Lcg {
    state: u64,
}

impl Lcg {
    const fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state ^ (self.state >> 29)
    }

    fn usize(&mut self, upper_exclusive: usize) -> usize {
        if upper_exclusive == 0 {
            0
        } else {
            (self.next_u64() as usize) % upper_exclusive
        }
    }

    fn range_usize(&mut self, min: usize, max_inclusive: usize) -> usize {
        if max_inclusive <= min {
            min
        } else {
            min + self.usize(max_inclusive - min + 1)
        }
    }

    fn chance(&mut self, numerator: usize, denominator: usize) -> bool {
        denominator != 0 && self.usize(denominator) < numerator
    }

    fn shuffle<T>(&mut self, values: &mut [T]) {
        for index in (1..values.len()).rev() {
            let swap = self.usize(index + 1);
            values.swap(index, swap);
        }
    }
}

pub fn run_random_verification(config: RandomVerifyConfig) -> RandomVerifyReport {
    let start = Instant::now();
    let mut report = RandomVerifyReport {
        seed: config.seed,
        cases_requested: config.cases,
        cases_passed: 0,
        orderbook_cases_passed: 0,
        elapsed_ms: 0,
        failures: Vec::new(),
        cases: Vec::new(),
    };

    for index in 0..config.cases {
        let case_seed = config.seed.wrapping_add(index as u64);
        match generate_case(case_seed, config.max_records)
            .and_then(|case| verify_case(&case, &config).map(|()| case))
        {
            Ok(case) => {
                report.cases_passed += 1;
                report.cases.push(case_report(&case));
            }
            Err(error) => report.failures.push(format!("seed {case_seed}: {error}")),
        }

        if config.check_orderbook && index % 5 == 0 {
            let orderbook_seed = config.seed.wrapping_add(0x0b00_0000 + index as u64);
            match generate_orderbook_case(orderbook_seed, config.max_records)
                .and_then(|case| verify_orderbook_case(&case).map(|()| case))
            {
                Ok(case) => {
                    report.orderbook_cases_passed += 1;
                    report.cases.push(case_report(&case.case));
                }
                Err(error) => report
                    .failures
                    .push(format!("orderbook seed {orderbook_seed}: {error}")),
            }
        }
    }

    report.elapsed_ms = start.elapsed().as_millis();
    report
}

pub fn generate_case(seed: u64, max_records: usize) -> VerifyResult<GeneratedCase> {
    let mut rng = Lcg::new(seed);
    let kind_index = (seed as usize) % 10;
    let dataset_kind = match kind_index {
        0 => "tiny",
        1 => "narrow",
        2 => "wide",
        3 => "reordered",
        4 => "dense-few-symbol",
        5 => "sparse-many-symbol",
        6 => "edge-integer",
        7 => "non-market-names",
        8 => "extra-irrelevant-fields",
        _ => "mixed-random",
    }
    .to_string();
    let record_count = record_count_for(seed, max_records);
    let mut types = field_types_for_kind(kind_index, &mut rng);
    if kind_index == 3 || kind_index == 8 || kind_index == 9 {
        rng.shuffle(&mut types);
    }
    let schema = schema_from_types(&dataset_kind, seed, &types)?;
    let rows = rows_for_schema(&schema, record_count, &dataset_kind, seed)?;
    let row_hash = row_hash_for_schema(&schema, &rows)?;
    Ok(GeneratedCase {
        seed,
        dataset_kind,
        schema,
        rows,
        row_hash,
    })
}

pub fn generate_orderbook_case(
    seed: u64,
    max_records: usize,
) -> VerifyResult<GeneratedOrderBookCase> {
    let mut rng = Lcg::new(seed);
    let record_count = record_count_for(seed, max_records).max(1);
    let mut fields = vec![
        (
            "clock_ns".to_string(),
            AuraType::TimestampNanos,
            Role::Timestamp,
        ),
        (
            "instrument_key".to_string(),
            AuraType::U32,
            Role::Instrument,
        ),
        ("book_side_code".to_string(), AuraType::EnumU8, Role::Side),
        (
            "limit_px".to_string(),
            AuraType::PriceI64Scaled { scale: 4 },
            Role::Price,
        ),
        ("qty_units".to_string(), AuraType::I64, Role::Size),
        ("event_action".to_string(), AuraType::U8, Role::Action),
        ("seq_no".to_string(), AuraType::I64, Role::Sequence),
        ("order_ref".to_string(), AuraType::I64, Role::OrderId),
        ("bitset".to_string(), AuraType::FlagsU32, Role::Flags),
        ("venue_bucket".to_string(), AuraType::U16, Role::Extra),
        ("route_code".to_string(), AuraType::EnumU8, Role::Extra),
    ];
    rng.shuffle(&mut fields);

    let mut builder = AuraSchema::named(format!("random_orderbook_{seed:016x}"));
    for (name, aura_type, _) in &fields {
        builder = builder.field(name.clone(), *aura_type);
    }
    let schema = builder.build().map_err(|error| error.to_string())?;

    let mut rows = Vec::with_capacity(record_count);
    for row_index in 0..record_count {
        let row_index_i64 = i64::try_from(row_index).map_err(|_| "record index".to_string())?;
        let mut row = Vec::with_capacity(fields.len());
        for (_, aura_type, role) in &fields {
            let value = match role {
                Role::Timestamp => AuraValue::I64(
                    1_700_000_000_000_000_000_i64
                        .saturating_add(row_index_i64.saturating_mul(1_000))
                        .saturating_add((rng.next_u64() % 3) as i64),
                ),
                Role::Instrument => AuraValue::U64((rng.next_u64() % 97) + 1),
                Role::Side => AuraValue::U64(match rng.usize(8) {
                    0 => 0,
                    1..=3 => 1,
                    4..=6 => 2,
                    _ => 3,
                }),
                Role::Price => AuraValue::I64(100_000 + (rng.next_u64() % 10_000) as i64),
                Role::Size => AuraValue::I64(if rng.chance(1, 11) {
                    0
                } else {
                    1 + (rng.next_u64() % 250) as i64
                }),
                Role::Action => AuraValue::U64(if rng.chance(1, 13) { 2 } else { 0 }),
                Role::Sequence => AuraValue::I64(row_index_i64),
                Role::OrderId => AuraValue::I64(10_000 + row_index_i64),
                Role::Flags => AuraValue::U64(rng.next_u64() % 16),
                Role::Extra => value_for_type(*aura_type, row_index, &mut rng, "orderbook-extra"),
            };
            row.push(value);
        }
        rows.push(row);
    }

    let name_for = |role| {
        fields
            .iter()
            .find_map(|(name, _, candidate)| (*candidate == role).then(|| name.clone()))
            .ok_or_else(|| "missing orderbook role".to_string())
    };
    let case = GeneratedCase {
        seed,
        dataset_kind: "random-orderbook-compatible".to_string(),
        schema,
        row_hash: 0,
        rows,
    };
    let row_hash = row_hash_for_schema(&case.schema, &case.rows)?;
    Ok(GeneratedOrderBookCase {
        case: GeneratedCase { row_hash, ..case },
        timestamp_name: name_for(Role::Timestamp)?,
        instrument_name: name_for(Role::Instrument)?,
        side_name: name_for(Role::Side)?,
        price_name: name_for(Role::Price)?,
        size_name: name_for(Role::Size)?,
        action_name: Some(name_for(Role::Action)?),
        sequence_name: Some(name_for(Role::Sequence)?),
        order_id_name: Some(name_for(Role::OrderId)?),
        flags_name: Some(name_for(Role::Flags)?),
    })
}

pub fn verify_case(case: &GeneratedCase, config: &RandomVerifyConfig) -> VerifyResult<()> {
    let aura1 = write_case(case, WriterOptions::aura1()).map_err(|error| {
        format!(
            "write aura1 failed for {} seed {}: {error}",
            case.dataset_kind, case.seed
        )
    })?;
    let aura0 = write_case(case, WriterOptions::aura0_compact()).map_err(|error| {
        format!(
            "write aura0 failed for {} seed {}: {error}",
            case.dataset_kind, case.seed
        )
    })?;

    verify_bytes("aura1", case, &aura1)?;
    verify_bytes("aura0", case, &aura0)?;

    if config.check_cross_format {
        let aura0_to_aura1 = convert_bytes(&aura0, AuraFormat::Aura1)?;
        verify_bytes("aura0->aura1", case, &aura0_to_aura1)?;

        let aura1_to_aura0 = convert_bytes(&aura1, AuraFormat::Aura0)?;
        verify_bytes("aura1->aura0", case, &aura1_to_aura0)?;
    }

    if config.check_streaming {
        for batch_size in [1, 2, 7, 127, 128, 129, 8192] {
            verify_streaming("aura1", case, &aura1, batch_size)?;
            verify_streaming("aura0", case, &aura0, batch_size)?;
        }
    }

    if config.check_replay {
        verify_replay(case, &aura1)?;
        verify_grouped_replay(case, &aura1)?;
    }

    Ok(())
}

pub fn verify_orderbook_case(case: &GeneratedOrderBookCase) -> VerifyResult<()> {
    let bytes = write_case(&case.case, WriterOptions::aura1())?;
    let reader = AuraReader::open(Cursor::new(&bytes)).map_err(|error| error.to_string())?;
    let spec = orderbook_spec(case, reader.schema())?;
    let rows = case.case.case_i64_rows()?;
    let index = |name: &str| -> VerifyResult<usize> {
        case.case
            .schema
            .fields()
            .iter()
            .position(|field| field.name == name)
            .ok_or_else(|| format!("missing field {name}"))
    };
    let timestamp_index = index(&case.timestamp_name)?;
    let instrument_index = index(&case.instrument_name)?;
    let side_index = index(&case.side_name)?;
    let price_index = index(&case.price_name)?;
    let size_index = index(&case.size_name)?;
    let action_index = case.action_name.as_deref().map(index).transpose()?;
    let sequence_index = case.sequence_name.as_deref().map(index).transpose()?;
    let order_id_index = case.order_id_name.as_deref().map(index).transpose()?;
    let flags_index = case.flags_name.as_deref().map(index).transpose()?;

    let mut selected_hash = 0xcbf2_9ce4_8422_2325u64;
    let mut deltas = Vec::with_capacity(rows.len());
    reader
        .replay_orderbook_deltas(7, &spec, |batch| {
            for row in 0..batch.row_count() {
                let observed = [
                    batch.timestamp(row)?,
                    batch.instrument(row)?,
                    batch.side(row)?,
                    batch.price(row)?,
                    batch.size(row)?,
                    batch.action(row)?.unwrap_or(0),
                    batch.sequence(row)?.unwrap_or(0),
                    batch.order_id(row)?.unwrap_or(0),
                    batch.flags(row)?.unwrap_or(0),
                ];
                for value in observed {
                    selected_hash = mix_checksum(selected_hash, value);
                }
                deltas.push(OrderBookDelta::try_new(
                    batch.timestamp(row)?,
                    batch.instrument(row)?,
                    batch.side(row)?,
                    batch.price(row)?,
                    batch.size(row)?,
                    batch.flags(row)?.unwrap_or(0),
                    batch.action(row)?.unwrap_or(0),
                    batch.sequence(row)?.unwrap_or(0),
                    batch.order_id(row)?.unwrap_or(0),
                )?);
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;

    let mut expected_selected = 0xcbf2_9ce4_8422_2325u64;
    for row in &rows {
        for index in [
            Some(timestamp_index),
            Some(instrument_index),
            Some(side_index),
            Some(price_index),
            Some(size_index),
            action_index,
            sequence_index,
            order_id_index,
            flags_index,
        ]
        .into_iter()
        .flatten()
        {
            expected_selected = mix_checksum(expected_selected, row[index]);
        }
    }
    if selected_hash != expected_selected {
        return Err("orderbook selected-field hash mismatch".to_string());
    }

    let expected_state = reference_book_hash(&deltas);
    let plan = PreparedOrderBookApplyPlan::build(&deltas, OrderBookEngineKind::Optimized)
        .map_err(|error| error.to_string())?;
    let mut engine = PreparedOrderBookEngine::with_capacity(&plan).map_err(|e| e.to_string())?;
    engine.reset_for_replay();
    let replay_reader = AuraReader::open(Cursor::new(&bytes)).map_err(|error| error.to_string())?;
    let replay = replay_reader
        .replay_orderbook_deltas_fused(11, &spec, &mut engine)
        .map_err(|error| error.to_string())?;
    let stats = engine
        .finish_profiled(
            OrderBookApplyMode::Verify,
            Some(expected_state),
            replay.apply_update_ns,
            0,
        )
        .map_err(|error| error.to_string())?;
    if stats.state_hash != expected_state || replay.rows_materialized != 0 {
        return Err("prepared orderbook replay mismatch".to_string());
    }

    engine.reset_for_replay();
    let replay_reader = AuraReader::open(Cursor::new(&bytes)).map_err(|error| error.to_string())?;
    let replay = replay_reader
        .replay_orderbook_deltas_fused(13, &spec, &mut engine)
        .map_err(|error| error.to_string())?;
    let stats = engine
        .finish_profiled(
            OrderBookApplyMode::Verify,
            Some(expected_state),
            replay.apply_update_ns,
            0,
        )
        .map_err(|error| error.to_string())?;
    if stats.state_hash != expected_state {
        return Err("prepared orderbook reset/reuse mismatch".to_string());
    }
    Ok(())
}

pub fn verify_corruption_rejects(case: &GeneratedCase, format: AuraFormat) -> VerifyResult<()> {
    let options = match format {
        AuraFormat::Aura1 => WriterOptions::aura1(),
        AuraFormat::Aura0 => WriterOptions::aura0_compact(),
        AuraFormat::Aura => WriterOptions::new(AuraFormat::Aura),
    };
    let bytes = write_case(case, options)?;
    for (name, corrupted) in corruptions(&bytes, format)? {
        if corrupted_file_reads(&corrupted) {
            return Err(format!("{name} corruption was accepted"));
        }
    }
    Ok(())
}

pub fn verify_unsupported_features_reject() -> VerifyResult<()> {
    for (name, result) in [
        (
            "utf8",
            AuraSchema::builder()
                .field("payload", AuraType::Utf8)
                .build(),
        ),
        (
            "binary",
            AuraSchema::builder()
                .field("payload", AuraType::Binary)
                .build(),
        ),
        (
            "f32",
            AuraSchema::builder().field("ratio", AuraType::F32).build(),
        ),
        (
            "f64",
            AuraSchema::builder().field("ratio", AuraType::F64).build(),
        ),
        (
            "nullable",
            AuraSchema::builder()
                .nullable_field("maybe_value", AuraType::I64)
                .build(),
        ),
    ] {
        if result.is_ok() {
            return Err(format!("unsupported feature accepted: {name}"));
        }
    }
    Ok(())
}

impl GeneratedCase {
    pub fn i64_rows(&self) -> VerifyResult<Vec<Vec<i64>>> {
        self.case_i64_rows()
    }

    fn case_i64_rows(&self) -> VerifyResult<Vec<Vec<i64>>> {
        AuraRecordBatch::new(self.schema.clone(), self.rows.clone())
            .and_then(|batch| batch.to_i64_rows())
            .map_err(|error| error.to_string())
    }
}

fn case_report(case: &GeneratedCase) -> RandomCaseReport {
    RandomCaseReport {
        seed: case.seed,
        dataset_kind: case.dataset_kind.clone(),
        schema_hash: case.schema.hash(),
        record_count: case.rows.len(),
        field_count: case.schema.field_count(),
        row_hash: case.row_hash,
    }
}

fn record_count_for(seed: u64, max_records: usize) -> usize {
    let fixed = [0, 1, 2, 3, 16, 127, 128, 129, 1024];
    let mut rng = Lcg::new(seed ^ 0xa11c_e55e_d00d);
    let value = if (seed as usize) % 12 < fixed.len() {
        fixed[(seed as usize) % 12]
    } else {
        rng.range_usize(1, max_records.max(1))
    };
    value.min(max_records)
}

fn field_types_for_kind(kind_index: usize, rng: &mut Lcg) -> Vec<AuraType> {
    match kind_index {
        0 => vec![AuraType::I64],
        1 => vec![AuraType::TimestampMicros, AuraType::I32],
        2 => {
            let base = supported_types();
            (0..32).map(|index| base[index % base.len()]).collect()
        }
        3 => vec![
            AuraType::FlagsU32,
            AuraType::EnumU8,
            AuraType::PriceI64Scaled { scale: 4 },
            AuraType::TimestampNanos,
            AuraType::U64,
            AuraType::U32,
            AuraType::I16,
            AuraType::Bool,
        ],
        4 => vec![
            AuraType::TimestampNanos,
            AuraType::U16,
            AuraType::U16,
            AuraType::I64Scaled { scale: 2 },
            AuraType::EnumU8,
            AuraType::FlagsU32,
        ],
        5 => vec![
            AuraType::TimestampNanos,
            AuraType::U32,
            AuraType::U32,
            AuraType::I64,
            AuraType::I32,
            AuraType::Bool,
        ],
        6 => vec![
            AuraType::Bool,
            AuraType::U8,
            AuraType::U16,
            AuraType::U32,
            AuraType::U64,
            AuraType::I8,
            AuraType::I16,
            AuraType::I32,
            AuraType::I64,
            AuraType::TimestampNanos,
            AuraType::TimestampMicros,
            AuraType::PriceI64Scaled { scale: 6 },
            AuraType::EnumU8,
            AuraType::FlagsU32,
        ],
        7 => vec![
            AuraType::I16,
            AuraType::U32,
            AuraType::I64Scaled { scale: 3 },
            AuraType::Bool,
            AuraType::TimestampMicros,
        ],
        8 => {
            let base = supported_types();
            (0..12)
                .map(|index| base[(index * 3 + 1) % base.len()])
                .collect()
        }
        _ => {
            let base = supported_types();
            let count = rng.range_usize(1, 32);
            (0..count).map(|_| base[rng.usize(base.len())]).collect()
        }
    }
}

fn supported_types() -> [AuraType; 14] {
    [
        AuraType::Bool,
        AuraType::U8,
        AuraType::U16,
        AuraType::U32,
        AuraType::U64,
        AuraType::I8,
        AuraType::I16,
        AuraType::I32,
        AuraType::I64,
        AuraType::TimestampNanos,
        AuraType::TimestampMicros,
        AuraType::I64Scaled { scale: 2 },
        AuraType::PriceI64Scaled { scale: 4 },
        AuraType::EnumU8,
    ]
}

fn schema_from_types(
    dataset_kind: &str,
    seed: u64,
    types: &[AuraType],
) -> VerifyResult<AuraSchema> {
    let mut builder = AuraSchema::named(format!("{dataset_kind}_{seed:016x}"));
    for (index, aura_type) in types.iter().copied().enumerate() {
        builder = builder.field(field_name(dataset_kind, index), aura_type);
    }
    builder.build().map_err(|error| error.to_string())
}

fn field_name(dataset_kind: &str, index: usize) -> String {
    const ROOTS: [&str; 32] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra",
        "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "atlas", "beacon",
        "cobalt", "drift", "ember", "flux",
    ];
    format!(
        "{}_{}_{}",
        dataset_kind.replace('-', "_"),
        ROOTS[index % ROOTS.len()],
        index
    )
}

fn rows_for_schema(
    schema: &AuraSchema,
    record_count: usize,
    dataset_kind: &str,
    seed: u64,
) -> VerifyResult<Vec<Vec<AuraValue>>> {
    let mut rng = Lcg::new(seed ^ 0x5eed_ba7c_0123_4567);
    let mut rows = Vec::with_capacity(record_count);
    for row_index in 0..record_count {
        let row = schema
            .fields()
            .iter()
            .map(|field| value_for_type(field.aura_type, row_index, &mut rng, dataset_kind))
            .collect::<Vec<_>>();
        rows.push(row);
    }
    Ok(rows)
}

fn value_for_type(
    aura_type: AuraType,
    row_index: usize,
    rng: &mut Lcg,
    dataset_kind: &str,
) -> AuraValue {
    let row = row_index as i64;
    match aura_type {
        AuraType::Bool => AuraValue::Bool((row_index + rng.usize(3)).is_multiple_of(2)),
        AuraType::U8 => AuraValue::U64(boundary_unsigned(rng, u8::MAX as u64, row_index)),
        AuraType::U16 => {
            let value = if dataset_kind.contains("dense") {
                (row_index % 4) as u64
            } else {
                boundary_unsigned(rng, u16::MAX as u64, row_index)
            };
            AuraValue::U64(value)
        }
        AuraType::U32 | AuraType::FlagsU32 => {
            let value = if dataset_kind.contains("sparse") {
                (row_index as u64).wrapping_mul(65_537) % 1_000_003
            } else {
                boundary_unsigned(rng, u32::MAX as u64, row_index)
            };
            AuraValue::U64(value)
        }
        AuraType::U64 => AuraValue::U64(boundary_unsigned(rng, i64::MAX as u64, row_index)),
        AuraType::I8 => AuraValue::I64(boundary_signed(rng, i8::MIN as i64, i8::MAX as i64, row)),
        AuraType::I16 => {
            AuraValue::I64(boundary_signed(rng, i16::MIN as i64, i16::MAX as i64, row))
        }
        AuraType::I32 => {
            AuraValue::I64(boundary_signed(rng, i32::MIN as i64, i32::MAX as i64, row))
        }
        AuraType::I64 => AuraValue::I64(match rng.usize(17) {
            0 if dataset_kind.contains("edge") => i64::MIN + row_index as i64,
            1 if dataset_kind.contains("edge") => i64::MAX - row_index as i64,
            2 => 0,
            3 => -row,
            _ => row
                .saturating_mul(((rng.next_u64() % 17) as i64) - 8)
                .saturating_add((rng.next_u64() % 101) as i64 - 50),
        }),
        AuraType::TimestampNanos => AuraValue::I64(1_700_000_000_000_000_000_i64.saturating_add(
            row.saturating_mul(match row_index % 5 {
                0 => 0,
                1 => 1,
                2 => 1_000,
                3 => 1_000_000,
                _ => 1_000_000_000,
            }),
        )),
        AuraType::TimestampMicros => {
            AuraValue::I64(1_700_000_000_000_000_i64.saturating_add(row.saturating_mul(17)))
        }
        AuraType::I64Scaled { .. } => AuraValue::I64(row.saturating_mul(10) - 500),
        AuraType::PriceI64Scaled { .. } => {
            AuraValue::I64(100_000_000 + row.saturating_mul(3) + (rng.usize(7) as i64))
        }
        AuraType::EnumU8 => AuraValue::U64((rng.next_u64() % 6) + (row_index % 3) as u64),
        AuraType::F32 | AuraType::F64 | AuraType::Binary | AuraType::Utf8 => AuraValue::I64(0),
    }
}

fn boundary_unsigned(rng: &mut Lcg, max: u64, row_index: usize) -> u64 {
    match (row_index + rng.usize(23)) % 11 {
        0 => 0,
        1 => 1,
        2 => max,
        3 => max.saturating_sub(1),
        _ => rng.next_u64() % max.min(1_000_000).saturating_add(1),
    }
}

fn boundary_signed(rng: &mut Lcg, min: i64, max: i64, row: i64) -> i64 {
    match rng.usize(13) {
        0 => min,
        1 => max,
        2 => 0,
        3 => -1,
        4 => 1,
        _ => {
            let span = (max as i128 - min as i128 + 1).min(1_000_000) as i64;
            min.saturating_add((row.abs() + rng.usize(span as usize) as i64) % span)
        }
    }
}

fn row_hash_for_schema(schema: &AuraSchema, rows: &[Vec<AuraValue>]) -> VerifyResult<u64> {
    let rows = AuraRecordBatch::new(schema.clone(), rows.to_vec())
        .and_then(|batch| batch.to_i64_rows())
        .map_err(|error| error.to_string())?;
    Ok(row_hash_i64(&rows))
}

fn row_hash_i64(rows: &[Vec<i64>]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    hash = mix_checksum(hash, rows.len() as i64);
    for row in rows {
        hash = mix_checksum(hash, row.len() as i64);
        for value in row {
            hash = mix_checksum(hash, *value);
        }
    }
    hash
}

fn write_case(case: &GeneratedCase, options: WriterOptions) -> VerifyResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let batch = AuraRecordBatch::new(case.schema.clone(), case.rows.clone())
        .map_err(|error| error.to_string())?;
    let mut writer = AuraWriter::try_new(&mut bytes, case.schema.clone(), options)
        .map_err(|error| error.to_string())?;
    writer
        .write_batch(batch)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn verify_bytes(label: &str, case: &GeneratedCase, bytes: &[u8]) -> VerifyResult<()> {
    let reader = AuraReader::open(Cursor::new(bytes)).map_err(|error| {
        format!(
            "{label} open failed for seed {} {}: {error}",
            case.seed, case.dataset_kind
        )
    })?;
    if reader.schema().fields() != case.schema.fields() {
        return Err(format!("{label} schema mismatch"));
    }
    let rows = read_all_i64(&reader)?;
    let expected = case.case_i64_rows()?;
    if rows != expected {
        return Err(format!("{label} row mismatch"));
    }
    let hash = row_hash_i64(&rows);
    if hash != case.row_hash {
        return Err(format!("{label} row hash mismatch"));
    }
    Ok(())
}

fn read_all_i64(reader: &AuraReader) -> VerifyResult<Vec<Vec<i64>>> {
    let batches = reader.read_batches().map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for batch in batches {
        rows.extend(batch.to_i64_rows().map_err(|error| error.to_string())?);
    }
    Ok(rows)
}

fn convert_bytes(bytes: &[u8], target_format: AuraFormat) -> VerifyResult<Vec<u8>> {
    let mut out = Vec::new();
    convert_aura(
        Cursor::new(bytes),
        &mut out,
        ConvertOptions::new(target_format)
            .profile(AuraProfile::Compact)
            .verify(true),
    )
    .map_err(|error| error.to_string())?;
    Ok(out)
}

fn verify_streaming(
    label: &str,
    case: &GeneratedCase,
    bytes: &[u8],
    batch_size: usize,
) -> VerifyResult<()> {
    let mut reader = AuraReader::open(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    while let Some(batch) = reader
        .next_batch(batch_size)
        .map_err(|error| error.to_string())?
    {
        rows.extend(batch.to_i64_rows().map_err(|error| error.to_string())?);
    }
    let expected = case.case_i64_rows()?;
    if rows != expected {
        return Err(format!(
            "{label} streaming mismatch at batch size {batch_size}"
        ));
    }
    Ok(())
}

fn verify_replay(case: &GeneratedCase, bytes: &[u8]) -> VerifyResult<()> {
    let reader = AuraReader::open(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut replayed = Vec::new();
    reader
        .replay_i64(|row| {
            replayed.push(row.to_vec());
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let expected = case.case_i64_rows()?;
    if replayed != expected {
        return Err("aura1 replay_i64 mismatch".to_string());
    }
    let mut fixed_rows = Vec::new();
    reader
        .replay_fixed_batches(17, |batch| {
            let mut row = Vec::new();
            for row_index in 0..batch.row_count() {
                row.clear();
                for field_index in 0..batch.field_count() {
                    row.push(batch.value_i64(row_index, field_index)?);
                }
                fixed_rows.push(row.clone());
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    if fixed_rows != expected {
        return Err("aura1 fixed-batch replay mismatch".to_string());
    }
    Ok(())
}

fn verify_grouped_replay(case: &GeneratedCase, bytes: &[u8]) -> VerifyResult<()> {
    if case.schema.field_count() == 0 {
        return Ok(());
    }
    let expected = case.case_i64_rows()?;
    let reader = AuraReader::open(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut groups = Vec::new();
    let stats = reader
        .grouped_replay(&GroupBy::field_ids([0]), |group| {
            groups.push((group.row_start(), group.row_count()));
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    if stats.row_count != expected.len() {
        return Err("grouped replay row count mismatch".to_string());
    }
    let mut cursor = 0usize;
    for (start, len) in groups {
        if start != cursor {
            return Err("grouped replay row order mismatch".to_string());
        }
        cursor += len;
    }
    if cursor != expected.len() {
        return Err("grouped replay concatenation mismatch".to_string());
    }
    Ok(())
}

fn orderbook_spec(
    case: &GeneratedOrderBookCase,
    schema: &AuraSchema,
) -> VerifyResult<OrderBookDeltaSpec> {
    let mut builder = OrderBookDeltaSpec::builder()
        .timestamp(case.timestamp_name.clone())
        .instrument(case.instrument_name.clone())
        .side(case.side_name.clone())
        .price(case.price_name.clone())
        .size(case.size_name.clone());
    if let Some(name) = &case.action_name {
        builder = builder.action_optional(name.clone());
    }
    if let Some(name) = &case.sequence_name {
        builder = builder.sequence_optional(name.clone());
    }
    if let Some(name) = &case.order_id_name {
        builder = builder.order_id_optional(name.clone());
    }
    if let Some(name) = &case.flags_name {
        builder = builder.flags_optional(name.clone());
    }
    builder.build(schema).map_err(|error| error.to_string())
}

fn corruptions(bytes: &[u8], format: AuraFormat) -> VerifyResult<Vec<(&'static str, Vec<u8>)>> {
    let mut out = Vec::new();

    if !bytes.is_empty() {
        let mut invalid_magic = bytes.to_vec();
        invalid_magic[0] ^= 0xff;
        out.push(("invalid magic", invalid_magic));
    }

    out.push(("truncated header", bytes[..bytes.len().min(6)].to_vec()));

    if bytes.len() > 8 {
        let mut bad_seal = bytes.to_vec();
        let last = bad_seal.len() - 1;
        bad_seal[last] ^= 0x01;
        out.push(("bad seal", bad_seal));
    }

    if bytes.len() > 6 {
        let mut unsupported_version = bytes.to_vec();
        unsupported_version[4] = 99;
        unsupported_version[5] = 0;
        out.push(("unsupported version", unsupported_version));
    }

    let metadata = crate::records::decode_i64_file_metadata(bytes).map_err(|error| {
        format!(
            "cannot build corruption offsets for {}: {error}",
            format.as_str()
        )
    })?;
    if metadata.record_count > 0 && metadata.footer_start > metadata.header_len {
        let mut truncated_body = Vec::new();
        truncated_body.extend_from_slice(&bytes[..metadata.footer_start - 1]);
        truncated_body.extend_from_slice(&bytes[metadata.footer_start..]);
        out.push(("truncated body", truncated_body));
    }
    if metadata.footer_start < metadata.footer_len_offset {
        let mut corrupt_footer = bytes.to_vec();
        corrupt_footer[metadata.footer_start] ^= 0x40;
        out.push(("corrupt footer", corrupt_footer));
    }
    if metadata.footer_len_offset + 4 <= bytes.len() {
        let mut bad_footer_len = bytes.to_vec();
        bad_footer_len[metadata.footer_len_offset] = 0xff;
        bad_footer_len[metadata.footer_len_offset + 1] = 0xff;
        bad_footer_len[metadata.footer_len_offset + 2] = 0xff;
        bad_footer_len[metadata.footer_len_offset + 3] = 0x7f;
        out.push(("invalid footer length", bad_footer_len));
    }

    Ok(out)
}

fn corrupted_file_reads(bytes: &[u8]) -> bool {
    let Ok(reader) = AuraReader::open(Cursor::new(bytes)) else {
        return false;
    };
    reader.read_batches().is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Timestamp,
    Instrument,
    Side,
    Price,
    Size,
    Action,
    Sequence,
    OrderId,
    Flags,
    Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RefKey {
    instrument: i64,
    side: i64,
    price: i64,
}

fn reference_book_hash(deltas: &[OrderBookDelta]) -> BookStateHash {
    let mut levels = BTreeMap::<RefKey, i64>::new();
    for delta in deltas {
        let side = match delta.side {
            BookSide::Bid => 1,
            BookSide::Ask => 2,
            BookSide::Other(raw) => raw as i64,
        };
        let key = RefKey {
            instrument: delta.instrument,
            side,
            price: delta.price,
        };
        if delta.is_remove() {
            levels.remove(&key);
        } else {
            levels.insert(key, delta.size);
        }
    }

    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (key, size) in levels {
        hash = mix_checksum(hash, key.instrument);
        hash = mix_checksum(hash, key.side);
        hash = mix_checksum(hash, key.price);
        hash = mix_checksum(hash, size);
    }
    BookStateHash(hash.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

fn mix_checksum(checksum: u64, value: i64) -> u64 {
    checksum.wrapping_mul(0x9e37_79b1_85eb_ca87).rotate_left(7)
        ^ (value as u64).wrapping_add(0xc2b2_ae3d_27d4_eb4f)
}

fn _assert_result_is_send_sync(_: Result<()>) {}
