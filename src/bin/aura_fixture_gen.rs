use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use aura_codec::instructions::GenericStreamOp;
use aura_codec::records::{self, I64FileInput};
use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema, SchemaDescriptor};
use aura_codec::Profile;
use serde_json::json;
use sha2::{Digest, Sha256};

const DEFAULT_ZSTD_LEVEL: i32 = 3;

#[derive(Debug)]
struct Args {
    output_dir: PathBuf,
    zstd_level: i32,
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

    let fixtures = fixture_inputs()?;
    let mut metadata = Vec::with_capacity(fixtures.len() + 1);
    for fixture in fixtures {
        metadata.push(write_fixture(&args.output_dir, args.zstd_level, fixture)?);
    }
    metadata.insert(3, huffman_blocker_metadata(args.zstd_level));

    let metadata_path = args.output_dir.join("fixtures.json");
    fs::write(&metadata_path, serde_json::to_vec_pretty(&metadata)?)
        .with_context(|| format!("write {}", metadata_path.display()))?;
    println!("fixtures={}", metadata.len());
    println!("metadata={}", metadata_path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut output_dir = None;
    let mut zstd_level = DEFAULT_ZSTD_LEVEL;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output-dir" => output_dir = Some(next_path(&mut args, "--output-dir")?),
            "--zstd-level" => zstd_level = next_parse(&mut args, "--zstd-level")?,
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
    eprintln!("usage: aura-fixture-gen --output-dir <dir> [--zstd-level N]");
}

fn fixture_inputs() -> Result<Vec<FixtureInput>> {
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
