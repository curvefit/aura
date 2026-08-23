use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use aura_codec::instructions::GenericStreamOp;
use aura_codec::records::{self, I64FileInput};
use aura_codec::schema::{
    generic_i64_parent_schema, ohlcv_schema, AuraSchema, AuraType, SchemaDescriptor,
};
use aura_codec::Profile;
use serde_json::json;
use sha2::{Digest, Sha256};

const DEFAULT_ZSTD_LEVEL: i32 = 3;

#[derive(Debug)]
struct Args {
    output_dir: PathBuf,
    zstd_level: i32,
    sdk_full: bool,
}

#[derive(Debug)]
struct FixtureInput {
    name: &'static str,
    schema: SchemaDescriptor,
    rows: Vec<Vec<i64>>,
    symbol_count: usize,
    expected_huffman: Option<bool>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("create {}", args.output_dir.display()))?;

    let fixtures = fixture_inputs(args.sdk_full)?;
    let mut metadata = Vec::with_capacity(fixtures.len() + 1);
    for fixture in fixtures {
        metadata.push(write_fixture(&args.output_dir, args.zstd_level, fixture)?);
    }
    metadata.insert(3, huffman_blocker_metadata(args.zstd_level));

    let metadata_path = args.output_dir.join("fixtures.json");
    fs::write(&metadata_path, serde_json::to_vec_pretty(&metadata)?)
        .with_context(|| format!("write {}", metadata_path.display()))?;
    let smoke_path = args.output_dir.join("sdk_bench_smoke.json");
    fs::write(
        &smoke_path,
        serde_json::to_vec_pretty(&sdk_bench_smoke_matrix(&metadata, args.zstd_level)?)?,
    )
    .with_context(|| format!("write {}", smoke_path.display()))?;
    println!("fixtures={}", metadata.len());
    println!("metadata={}", metadata_path.display());
    println!("sdk_bench_smoke={}", smoke_path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut output_dir = None;
    let mut zstd_level = DEFAULT_ZSTD_LEVEL;
    let mut sdk_full = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output-dir" => output_dir = Some(next_path(&mut args, "--output-dir")?),
            "--zstd-level" => zstd_level = next_parse(&mut args, "--zstd-level")?,
            "--sdk-full" => sdk_full = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            value => bail!("unknown argument {value}"),
        }
    }
    Ok(Args {
        output_dir: output_dir.context("missing --output-dir")?,
        zstd_level,
        sdk_full,
    })
}

fn next_path(args: &mut impl Iterator<Item = String>, flag: &'static str) -> Result<PathBuf> {
    args.next()
        .map(PathBuf::from)
        .with_context(|| format!("missing {flag} value"))
}

fn next_parse<T>(args: &mut impl Iterator<Item = String>, flag: &'static str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    args.next()
        .with_context(|| format!("missing {flag} value"))?
        .parse()
        .with_context(|| format!("invalid {flag} value"))
}

fn print_usage() {
    eprintln!("usage: aura-fixture-gen --output-dir <dir> [--zstd-level N] [--sdk-full]");
}

fn fixture_inputs(sdk_full: bool) -> Result<Vec<FixtureInput>> {
    let sdk_narrow_count = if sdk_full { 10_000 } else { 64 };
    let sdk_wide_count = if sdk_full { 10_000 } else { 512 };
    let sdk_reordered_count = if sdk_full { 10_000 } else { 256 };
    let sdk_dense_count = if sdk_full { 100_000 } else { 4096 };
    let sdk_sparse_count = if sdk_full { 100_000 } else { 2048 };
    let sdk_edge_count = if sdk_full { 10_000 } else { 3 };
    let sdk_larger_count = if sdk_full { 500_000 } else { 32_768 };
    let group_count = if sdk_full { 100_000 } else { 2048 };
    Ok(vec![
        FixtureInput {
            name: "tiny",
            schema: ohlcv_schema()?,
            rows: dense_ohlcv_rows(16, 1),
            symbol_count: 1,
            expected_huffman: None,
        },
        FixtureInput {
            name: "dense-few-symbol",
            schema: ohlcv_schema()?,
            rows: dense_ohlcv_rows(4096, 3),
            symbol_count: 3,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sparse-many-symbol",
            schema: generic_i64_parent_schema(
                "fixture_sparse_many_symbol",
                &[100, 0, 0, 205, 4, 5, 5, 5],
            )?,
            rows: sparse_many_symbol_rows(256),
            symbol_count: 256,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-tiny",
            schema: sdk_tiny_schema(),
            rows: sdk_tiny_rows(),
            symbol_count: 2,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-narrow",
            schema: sdk_narrow_schema(),
            rows: sdk_narrow_rows(sdk_narrow_count),
            symbol_count: 4,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-wide",
            schema: sdk_wide_schema(),
            rows: sdk_wide_rows(sdk_wide_count),
            symbol_count: 32,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-reordered",
            schema: sdk_reordered_schema(),
            rows: sdk_reordered_rows(sdk_reordered_count),
            symbol_count: 16,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-dense",
            schema: sdk_dense_schema(),
            rows: sdk_dense_rows(sdk_dense_count, 4),
            symbol_count: 4,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-sparse",
            schema: sdk_sparse_schema(),
            rows: sdk_sparse_rows(sdk_sparse_count, 512),
            symbol_count: 512,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-edge-case",
            schema: sdk_edge_schema(),
            rows: sdk_edge_rows(sdk_edge_count),
            symbol_count: 3,
            expected_huffman: None,
        },
        FixtureInput {
            name: "sdk-larger",
            schema: sdk_dense_schema(),
            rows: sdk_dense_rows(sdk_larger_count, 64),
            symbol_count: 64,
            expected_huffman: None,
        },
        FixtureInput {
            name: "repeated-timestamp",
            schema: sdk_group_schema(),
            rows: sdk_group_rows(group_count, GroupPattern::RepeatedTimestamp),
            symbol_count: 512,
            expected_huffman: None,
        },
        FixtureInput {
            name: "repeated-symbol",
            schema: sdk_group_schema(),
            rows: sdk_group_rows(group_count, GroupPattern::RepeatedSymbol),
            symbol_count: 64,
            expected_huffman: None,
        },
        FixtureInput {
            name: "repeated-timestamp-symbol",
            schema: sdk_group_schema(),
            rows: sdk_group_rows(group_count, GroupPattern::RepeatedTimestampSymbol),
            symbol_count: 512,
            expected_huffman: None,
        },
        FixtureInput {
            name: "high-cardinality",
            schema: sdk_group_schema(),
            rows: sdk_group_rows(group_count, GroupPattern::HighCardinality),
            symbol_count: group_count.min(4096),
            expected_huffman: None,
        },
        FixtureInput {
            name: "mixed-burst",
            schema: sdk_group_schema(),
            rows: sdk_group_rows(group_count, GroupPattern::MixedBurst),
            symbol_count: 128,
            expected_huffman: None,
        },
        FixtureInput {
            name: "nohuff",
            schema: generic_i64_parent_schema("fixture_nohuff", &[0])?,
            rows: nohuff_rows(),
            symbol_count: 6,
            expected_huffman: Some(false),
        },
        FixtureInput {
            name: "larger",
            schema: ohlcv_schema()?,
            rows: dense_ohlcv_rows(32_768, 8),
            symbol_count: 8,
            expected_huffman: None,
        },
    ])
}

fn huffman_blocker_metadata(zstd_level: i32) -> serde_json::Value {
    json!({
        "dataset_name": "huff",
        "coverage_status": "blocked_by_specific_format_issue",
        "blocker": "current public fixture writer/planner did not produce a HuffmanDictionary stream for generated rows under the required 2x Huffman speed gate; grimoire-50mb-huff remains an external compatible artifact, not repo-generated coverage",
        "paths": serde_json::Value::Null,
        "record_count": 0,
        "field_count": 0,
        "symbol_count": 0,
        "aura_sha256": null,
        "aura0_sha256": null,
        "aura1_sha256": null,
        "aura1_zst_sha256": null,
        "aura_bytes": 0,
        "aura0_bytes": 0,
        "aura1_bytes": 0,
        "aura1_zst_bytes": 0,
        "zstd_level": zstd_level,
        "generic_stream_count": 0,
        "generic_group_count": 0,
        "huffman_stream_count": 0,
        "row_equality_verified": false,
    })
}

fn write_fixture(
    output_dir: &Path,
    zstd_level: i32,
    fixture: FixtureInput,
) -> Result<serde_json::Value> {
    let ingest = records::encode_ingest_i64_file(I64FileInput {
        schema: fixture.schema.clone(),
        rows: fixture.rows.clone(),
        stream_id: 17,
        dictionary_id: 29,
        header_comment: Some(format!("generated benchmark fixture {}", fixture.name)),
    })?;
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0)?;
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1)?;
    let aura1_zst = zstd::stream::encode_all(Cursor::new(aura1.as_slice()), zstd_level)
        .context("zstd-compress generated Aura1 fixture")?;

    let decoded_aura0 = records::decode_i64_file(&aura0)?;
    let decoded_aura1 = records::decode_i64_file(&aura1)?;
    if decoded_aura0.rows != fixture.rows || decoded_aura1.rows != fixture.rows {
        bail!("generated fixture {} failed row equality", fixture.name);
    }
    let footer = decoded_aura0
        .compiled_footer
        .as_ref()
        .context("generated Aura0 fixture missing compiled footer")?;
    let plan = footer
        .generic_aura0_plan
        .as_ref()
        .context("generated Aura0 fixture missing generic plan")?;
    let huffman_stream_count = plan
        .streams
        .iter()
        .filter(|stream| matches!(stream.op, GenericStreamOp::HuffmanDictionary { .. }))
        .count();
    if let Some(expected) = fixture.expected_huffman {
        if (huffman_stream_count > 0) != expected {
            bail!(
                "generated fixture {} huffman expectation failed: expected {expected}, got {huffman_stream_count}",
                fixture.name
            );
        }
    }

    let stem = fixture.name;
    let aura_path = output_dir.join(format!("{stem}.aura"));
    let aura0_path = output_dir.join(format!("{stem}.aura0"));
    let aura1_path = output_dir.join(format!("{stem}.aura1"));
    let aura1_zst_path = output_dir.join(format!("{stem}.aura1.zst"));
    fs::write(&aura_path, &ingest).with_context(|| format!("write {}", aura_path.display()))?;
    fs::write(&aura0_path, &aura0).with_context(|| format!("write {}", aura0_path.display()))?;
    fs::write(&aura1_path, &aura1).with_context(|| format!("write {}", aura1_path.display()))?;
    fs::write(&aura1_zst_path, &aura1_zst)
        .with_context(|| format!("write {}", aura1_zst_path.display()))?;

    Ok(json!({
        "dataset_name": fixture.name,
        "schema_name": fixture.schema.name,
        "schema_hash": fixture.schema.schema_id,
        "field_names": fixture.schema.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        "field_physical_types": fixture.schema.fields.iter().map(|field| field.field_type.name()).collect::<Vec<_>>(),
        "record_width": schema_record_width(&fixture.schema)?,
        "paths": {
            "aura": aura_path,
            "aura0": aura0_path,
            "aura1": aura1_path,
            "aura1_zst": aura1_zst_path,
        },
        "record_count": fixture.rows.len(),
        "field_count": fixture.schema.fields.len(),
        "symbol_count": fixture.symbol_count,
        "aura_sha256": sha256_hex(&ingest),
        "aura0_sha256": sha256_hex(&aura0),
        "aura1_sha256": sha256_hex(&aura1),
        "aura1_zst_sha256": sha256_hex(&aura1_zst),
        "aura_bytes": ingest.len(),
        "aura0_bytes": aura0.len(),
        "aura1_bytes": aura1.len(),
        "aura1_zst_bytes": aura1_zst.len(),
        "zstd_level": zstd_level,
        "generic_stream_count": plan.streams.len(),
        "generic_group_count": plan.groups.len(),
        "huffman_stream_count": huffman_stream_count,
        "row_equality_verified": true,
    }))
}

fn sdk_bench_smoke_matrix(
    fixtures: &[serde_json::Value],
    zstd_level: i32,
) -> Result<serde_json::Value> {
    let mut entries = Vec::new();
    for fixture in fixtures {
        let Some(name) = fixture["dataset_name"].as_str() else {
            continue;
        };
        if !name.starts_with("sdk-") {
            continue;
        }
        let aura0 = fixture["paths"]["aura0"]
            .as_str()
            .context("sdk fixture aura0 path")?;
        let aura1 = fixture["paths"]["aura1"]
            .as_str()
            .context("sdk fixture aura1 path")?;
        for operation in [
            "aura0-to-aura1-bytes",
            "aura0-to-aura1-bytes-verify",
            "zstd-aura1-to-aura1-bytes",
        ] {
            entries.push(json!({
                "dataset_kind": name,
                "operation": operation,
                "schema_hash": fixture["schema_hash"],
                "schema_name": fixture["schema_name"],
                "field_count": fixture["field_count"],
                "field_names": fixture["field_names"],
                "field_physical_types": fixture["field_physical_types"],
                "record_width": fixture["record_width"],
                "record_count": fixture["record_count"],
                "compiled_plan_used": true,
                "command": [
                    "target/release/aura-bench",
                    "--operation", operation,
                    "--dataset", name,
                    "--input", if operation.starts_with("zstd") { aura1 } else { aura0 },
                    "--reference-aura0", aura0,
                    "--reference-aura1", aura1,
                    "--zstd-level", zstd_level.to_string(),
                    "--iterations", "1",
                    "--warmups", "0",
                    "--format", "json"
                ]
            }));
        }
    }
    Ok(json!({
        "matrix_kind": "sdk-generic-smoke",
        "zstd_level": zstd_level,
        "entries": entries,
    }))
}

fn schema_record_width(schema: &SchemaDescriptor) -> Result<usize> {
    schema
        .fields
        .iter()
        .map(|field| match field.field_type {
            aura_codec::FieldType::I8 | aura_codec::FieldType::U8 => Ok(1),
            aura_codec::FieldType::I16 | aura_codec::FieldType::U16 => Ok(2),
            aura_codec::FieldType::I32 | aura_codec::FieldType::U32 => Ok(4),
            aura_codec::FieldType::I64
            | aura_codec::FieldType::U64
            | aura_codec::FieldType::TimestampNs => Ok(8),
            aura_codec::FieldType::I128 | aura_codec::FieldType::Opaque16 => Ok(16),
            aura_codec::FieldType::TimestampMs
            | aura_codec::FieldType::Utf8
            | aura_codec::FieldType::DecimalText => {
                bail!("v3-only type reached v2 fixture generator")
            }
        })
        .sum()
}

fn dense_ohlcv_rows(count: usize, symbol_mod: i64) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).expect("fixture index fits i64");
            let symbol = index % symbol_mod.max(1);
            let ts = 1_700_000_000_000_000_000 + index * 1_000_000;
            let open = 10_000 + symbol * 100 + index % 17;
            let high = open + 25 + index % 5;
            let low = open - 20 - index % 3;
            let close = open + index % 7 - 3;
            let volume = 1_000 + symbol * 10 + (index * 13) % 2_000;
            vec![ts, open, high, low, close, volume]
        })
        .collect()
}

fn sparse_many_symbol_rows(events: u16) -> Vec<Vec<i64>> {
    (0..events)
        .flat_map(|event| {
            let run_sizes = [
                2 + usize::from(event % 3 == 0),
                3 + usize::from(event % 4 == 0),
            ];
            run_sizes
                .into_iter()
                .enumerate()
                .flat_map(move |(partition, run_len)| {
                    (0..run_len).map(move |level| {
                        let base_price = 2_000_000 + i64::from(event) * 10;
                        let first_price = if partition == 0 {
                            base_price - 100
                        } else {
                            base_price + 8_000_000 + 100
                        };
                        let price = first_price + i64::try_from(level).unwrap() * 5;
                        let has_qty = (usize::from(event) + level + partition).is_multiple_of(11);
                        vec![
                            1_000_000 + i64::from(event / 2),
                            10_000 + i64::from(event),
                            20_000 + i64::from(event / 3),
                            partition as i64,
                            price,
                            if has_qty { 1_000 + i64::from(event) } else { 0 },
                            if has_qty {
                                2_000 + i64::try_from(level).unwrap()
                            } else {
                                0
                            },
                            i64::from(has_qty),
                        ]
                    })
                })
        })
        .collect()
}

fn nohuff_rows() -> Vec<Vec<i64>> {
    [0, 1_000_000_000_000, 0, 17, 0, 999_999_999_937]
        .into_iter()
        .cycle()
        .take(30)
        .map(|bucket| vec![9_000_000_000_000 + bucket])
        .collect()
}

fn sdk_tiny_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_tiny_schema")
        .field("event_time", AuraType::TimestampNanos)
        .field("venue", AuraType::U16)
        .field("quantity", AuraType::I32)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_tiny_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000, 1, 10],
        vec![2_000, 1, -3],
        vec![3_000, 2, 0],
        vec![4_000, 2, 99],
    ]
}

fn sdk_narrow_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_narrow_schema")
        .field("micro_time", AuraType::TimestampMicros)
        .field("signed_qty", AuraType::I16)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_narrow_rows(count: usize) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            vec![10_000 + index * 250, (index % 17) - 8]
        })
        .collect()
}

fn sdk_wide_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_wide_schema")
        .field("alpha_ts", AuraType::TimestampNanos)
        .field("beta_id", AuraType::U16)
        .field("gamma_price", AuraType::PriceI64Scaled { scale: 4 })
        .field("delta_qty", AuraType::U32)
        .field("epsilon_active", AuraType::Bool)
        .field("zeta_flags", AuraType::FlagsU32)
        .field("eta_change", AuraType::I16)
        .field("theta_side", AuraType::EnumU8)
        .field("iota_count", AuraType::U8)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_wide_rows(count: usize) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            vec![
                1_700_000_000_000_000_000 + index * 1_000_000,
                index % 32,
                100_000 + (index % 1_000) - 500,
                10 + index % 500,
                i64::from(index % 2 == 0),
                index % 16,
                (index % 31) - 15,
                index % 3,
                index % 250,
            ]
        })
        .collect()
}

fn sdk_reordered_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_reordered_schema")
        .field("flags", AuraType::FlagsU32)
        .field("side", AuraType::EnumU8)
        .field("px", AuraType::I64Scaled { scale: 4 })
        .field("ts", AuraType::TimestampNanos)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_reordered_rows(count: usize) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            vec![
                index % 4,
                index % 2 + 1,
                10_000 + index % 97,
                1_000_000 + index,
            ]
        })
        .collect()
}

fn sdk_dense_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_dense_schema")
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price", AuraType::PriceI64Scaled { scale: 9 })
        .field("size", AuraType::U64)
        .field("side", AuraType::EnumU8)
        .field("flags", AuraType::FlagsU32)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_dense_rows(count: usize, symbols: i64) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            let symbol = index % symbols.max(1);
            vec![
                1_700_000_000_000_000_000 + index * 1_000_000,
                symbol,
                101_000_000_000 + symbol * 10_000_000 + index % 257,
                1 + index % 100,
                index % 2 + 1,
                index % 8,
            ]
        })
        .collect()
}

fn sdk_sparse_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_sparse_schema")
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("price_tick", AuraType::I64)
        .field("quantity", AuraType::U32)
        .field("condition", AuraType::EnumU8)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_sparse_rows(count: usize, symbols: i64) -> Vec<Vec<i64>> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            let symbol = (index * 37) % symbols.max(1);
            vec![
                1_700_100_000_000_000_000 + (index / 3) * 1_000_000,
                symbol,
                2_000_000 + symbol * 3 + index % 13,
                if index % 7 == 0 { 0 } else { 1 + index % 20 },
                index % 5,
            ]
        })
        .collect()
}

fn sdk_edge_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_edge_schema")
        .field("ts_event", AuraType::TimestampNanos)
        .field("min_i32", AuraType::I32)
        .field("max_u32", AuraType::U32)
        .field("small_i8", AuraType::I8)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_edge_rows(count: usize) -> Vec<Vec<i64>> {
    let base = [
        [
            1_700_000_000_000_000_000,
            i64::from(i32::MIN),
            0,
            i64::from(i8::MIN),
        ],
        [1_700_000_000_000_000_000, -1, i64::from(u32::MAX), 0],
        [
            1_700_000_000_999_000_000,
            i64::from(i32::MAX),
            42,
            i64::from(i8::MAX),
        ],
    ];
    (0..count)
        .map(|index| {
            let mut row = base[index % base.len()].to_vec();
            row[0] = row[0].saturating_add(i64::try_from(index).unwrap_or(0));
            row
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum GroupPattern {
    RepeatedTimestamp,
    RepeatedSymbol,
    RepeatedTimestampSymbol,
    HighCardinality,
    MixedBurst,
}

fn sdk_group_schema() -> SchemaDescriptor {
    AuraSchema::named("sdk_group_schema")
        .field("event_time", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("event_type", AuraType::EnumU8)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("quantity", AuraType::U32)
        .build()
        .unwrap()
        .into_descriptor()
}

fn sdk_group_rows(count: usize, pattern: GroupPattern) -> Vec<Vec<i64>> {
    let mut rows = Vec::with_capacity(count);
    let mut index = 0usize;
    while index < count {
        let run_len = match pattern {
            GroupPattern::RepeatedTimestamp => 32,
            GroupPattern::RepeatedSymbol => 32,
            GroupPattern::RepeatedTimestampSymbol => 48,
            GroupPattern::HighCardinality => 1,
            GroupPattern::MixedBurst => match (index / 17) % 5 {
                0 => 1,
                1 => 4,
                2 => 16,
                3 => 32,
                _ => 8,
            },
        }
        .min(count - index);
        let run = index / run_len.max(1);
        let repeated_ts = matches!(
            pattern,
            GroupPattern::RepeatedTimestamp
                | GroupPattern::RepeatedTimestampSymbol
                | GroupPattern::MixedBurst
        );
        let repeated_symbol = matches!(
            pattern,
            GroupPattern::RepeatedSymbol
                | GroupPattern::RepeatedTimestampSymbol
                | GroupPattern::MixedBurst
        );
        let run_ts = 1_700_200_000_000_000_000 + i64::try_from(run).unwrap() * 1_000_000;
        let run_symbol = i64::try_from(run % 512).unwrap();
        for offset in 0..run_len {
            let row_index = index + offset;
            let row_index_i64 = i64::try_from(row_index).unwrap();
            let ts = if repeated_ts {
                run_ts
            } else {
                1_700_200_000_000_000_000 + row_index_i64 * 1_000_000
            };
            let symbol = if repeated_symbol {
                run_symbol
            } else {
                row_index_i64 % 4096
            };
            rows.push(vec![
                ts,
                symbol,
                row_index_i64 % 5,
                10_000_000 + symbol * 100 + row_index_i64 % 97,
                1 + row_index_i64 % 1_000,
            ]);
        }
        index += run_len;
    }
    rows
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").expect("write hex");
    }
    out
}

#[allow(dead_code)]
fn distinct_values(rows: &[Vec<i64>], field_index: usize) -> usize {
    rows.iter()
        .filter_map(|row| row.get(field_index).copied())
        .collect::<BTreeSet<_>>()
        .len()
}
