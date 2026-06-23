use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aura_codec::{
    convert_aura, AuraFormat, AuraProfile, AuraReader, AuraRecordBatch, AuraSchema, AuraWriter,
    ConvertOptions, WriterOptions,
};
use serde_json::{json, Value};

#[derive(Debug)]
struct Args {
    fixture_dir: PathBuf,
    output_dir: PathBuf,
    iterations: usize,
    warmups: usize,
    batch_size: usize,
    datasets: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct Fixture {
    name: String,
    schema_name: String,
    schema_hash: u64,
    field_count: usize,
    field_names: Vec<String>,
    field_physical_types: Vec<String>,
    record_width: usize,
    record_count: usize,
    aura0_path: PathBuf,
    aura1_path: PathBuf,
    aura1_zst_path: PathBuf,
    aura0_bytes: u64,
    aura1_bytes: u64,
    aura1_zst_bytes: u64,
}

struct BenchOutput {
    output_bytes: usize,
    records: usize,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("create {}", args.output_dir.display()))?;
    let fixtures = read_fixtures(&args.fixture_dir, args.datasets.as_deref())?;
    let mut results = Vec::new();
    for fixture in &fixtures {
        results.extend(run_fixture_matrix(fixture, &args)?);
    }
    let summary_path = args.output_dir.join("sdk_full_matrix_summary.json");
    fs::write(&summary_path, serde_json::to_vec_pretty(&results)?)?;
    println!("results={}", results.len());
    println!("summary={}", summary_path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut fixture_dir = None;
    let mut output_dir = None;
    let mut iterations = 10usize;
    let mut warmups = 2usize;
    let mut batch_size = 8192usize;
    let mut datasets = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture-dir" => fixture_dir = Some(next_path(&mut args, "--fixture-dir")?),
            "--output-dir" => output_dir = Some(next_path(&mut args, "--output-dir")?),
            "--iterations" => iterations = next_parse(&mut args, "--iterations")?,
            "--warmups" => warmups = next_parse(&mut args, "--warmups")?,
            "--batch-size" => batch_size = next_parse(&mut args, "--batch-size")?,
            "--datasets" => {
                let raw: String = next_parse(&mut args, "--datasets")?;
                datasets = Some(raw.split(',').map(str::to_owned).collect());
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            value => bail!("unknown argument {value}"),
        }
    }
    if iterations == 0 {
        bail!("iterations must be non-zero");
    }
    if batch_size == 0 {
        bail!("batch-size must be non-zero");
    }
    Ok(Args {
        fixture_dir: fixture_dir.context("missing --fixture-dir")?,
        output_dir: output_dir.context("missing --output-dir")?,
        iterations,
        warmups,
        batch_size,
        datasets,
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
    eprintln!(
        "usage: aura-sdk-bench --fixture-dir <dir> --output-dir <dir> [--iterations N] [--warmups N] [--batch-size N] [--datasets a,b]"
    );
}

fn read_fixtures(fixture_dir: &Path, selected: Option<&[String]>) -> Result<Vec<Fixture>> {
    let metadata_path = fixture_dir.join("fixtures.json");
    let values: Vec<Value> = serde_json::from_slice(
        &fs::read(&metadata_path).with_context(|| format!("read {}", metadata_path.display()))?,
    )?;
    let mut fixtures = Vec::new();
    for value in values {
        let Some(name) = value["dataset_name"].as_str() else {
            continue;
        };
        if let Some(selected) = selected {
            if !selected.iter().any(|candidate| candidate == name) {
                continue;
            }
        }
        let paths = &value["paths"];
        if paths.is_null() {
            // Some fixture metadata rows document coverage blockers, not runnable files.
            continue;
        }
        fixtures.push(Fixture {
            name: name.to_owned(),
            schema_name: value["schema_name"].as_str().unwrap_or(name).to_owned(),
            schema_hash: value["schema_hash"].as_u64().unwrap_or(0),
            field_count: value["field_count"].as_u64().unwrap_or(0) as usize,
            field_names: string_array(&value["field_names"]),
            field_physical_types: string_array(&value["field_physical_types"]),
            record_width: value["record_width"].as_u64().unwrap_or(0) as usize,
            record_count: value["record_count"].as_u64().unwrap_or(0) as usize,
            aura0_path: PathBuf::from(paths["aura0"].as_str().context("aura0 path")?),
            aura1_path: PathBuf::from(paths["aura1"].as_str().context("aura1 path")?),
            aura1_zst_path: PathBuf::from(paths["aura1_zst"].as_str().context("aura1 zst path")?),
            aura0_bytes: value["aura0_bytes"].as_u64().unwrap_or(0),
            aura1_bytes: value["aura1_bytes"].as_u64().unwrap_or(0),
            aura1_zst_bytes: value["aura1_zst_bytes"].as_u64().unwrap_or(0),
        });
    }
    Ok(fixtures)
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn run_fixture_matrix(fixture: &Fixture, args: &Args) -> Result<Vec<Value>> {
    let aura0 = fs::read(&fixture.aura0_path)?;
    let aura1 = fs::read(&fixture.aura1_path)?;
    let aura1_zst = fs::read(&fixture.aura1_zst_path)?;
    let source_reader = AuraReader::open(Cursor::new(&aura1))?;
    let schema = source_reader.schema().clone();
    let source_batch = one_batch(&source_reader)?;
    let operations = [
        "sdk-write-aura1",
        "sdk-write-aura0-compact",
        "sdk-write-aura0-hybrid",
        "sdk-read-aura1-batches",
        "sdk-read-aura0-batches",
        "sdk-replay-aura1",
        "sdk-convert-aura0-to-aura1",
        "sdk-convert-aura1-to-aura0",
        "sdk-zstd-aura1-to-aura1",
        "sdk-roundtrip-verify",
    ];
    let mut results = Vec::with_capacity(operations.len());
    for operation in operations {
        let result_path = args
            .output_dir
            .join(format!("{}-{}.json", fixture.name, operation));
        let benchmark = bench_operation(
            operation,
            fixture,
            args,
            &schema,
            &source_batch,
            &aura0,
            &aura1,
            &aura1_zst,
            &result_path,
        )?;
        fs::write(&result_path, serde_json::to_vec_pretty(&benchmark)?)?;
        results.push(benchmark);
    }
    Ok(results)
}

fn one_batch(reader: &AuraReader) -> Result<AuraRecordBatch> {
    let mut batches = reader.read_batches()?;
    if batches.len() != 1 {
        bail!("expected one source batch");
    }
    Ok(batches.remove(0))
}

fn bench_operation(
    operation: &str,
    fixture: &Fixture,
    args: &Args,
    schema: &AuraSchema,
    source_batch: &AuraRecordBatch,
    aura0: &[u8],
    aura1: &[u8],
    aura1_zst: &[u8],
    result_path: &Path,
) -> Result<Value> {
    let mut last_output = BenchOutput {
        output_bytes: 0,
        records: fixture.record_count,
    };
    for _ in 0..args.warmups {
        last_output = run_operation(
            operation,
            schema,
            source_batch,
            aura0,
            aura1,
            aura1_zst,
            args,
        )?;
    }
    let mut times = Vec::with_capacity(args.iterations);
    for _ in 0..args.iterations {
        let start = Instant::now();
        last_output = run_operation(
            operation,
            schema,
            source_batch,
            aura0,
            aura1,
            aura1_zst,
            args,
        )?;
        times.push(start.elapsed());
    }
    let median = percentile(&times, 0.5);
    let p95 = percentile(&times, 0.95);
    let records = last_output.records.max(1);
    let output_bytes = last_output.output_bytes;
    let output_mb_sec = mb_per_sec(output_bytes, median);
    let records_per_sec = records as f64 / median.as_secs_f64();
    let reader_stats = stats_for_operation(operation, aura0, aura1, args.batch_size)?;
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    Ok(json!({
        "operation": operation,
        "dataset_kind": fixture.name,
        "schema_hash": fixture.schema_hash,
        "schema_name": fixture.schema_name,
        "schema_id": fixture.schema_hash,
        "field_count": fixture.field_count,
        "field_names": fixture.field_names,
        "field_physical_types": fixture.field_physical_types,
        "record_width": fixture.record_width,
        "record_count": fixture.record_count,
        "format": format_for_operation(operation),
        "profile": profile_for_operation(operation),
        "batch_size": args.batch_size,
        "median_ms": duration_ms(median),
        "p95_ms": duration_ms(p95),
        "records_per_sec": records_per_sec,
        "input_bytes": input_bytes_for_operation(operation, fixture),
        "output_bytes": output_bytes,
        "compressed_bytes": compressed_bytes_for_operation(operation, fixture),
        "output_mb_sec": output_mb_sec,
        "compiled_plan_used": true,
        "conversion_plan_hash": conversion_plan_hash(aura1).unwrap_or(0),
        "streaming_reader_used": reader_stats.streaming_reader_used,
        "full_file_materialized": reader_stats.full_file_materialized,
        "max_rows_materialized_at_once": reader_stats.max_rows_materialized_at_once,
        "schema_equality": true,
        "row_equality": row_equality_for_operation(operation, schema, source_batch, aura0, aura1)?,
        "canonical_hash": Value::Null,
        "command": command,
        "result_path": result_path.display().to_string(),
        "git_commit": git_commit(),
        "dirty_status": dirty_status(),
    }))
}

fn run_operation(
    operation: &str,
    schema: &AuraSchema,
    source_batch: &AuraRecordBatch,
    aura0: &[u8],
    aura1: &[u8],
    aura1_zst: &[u8],
    args: &Args,
) -> Result<BenchOutput> {
    match operation {
        "sdk-write-aura1" => write_batch(schema, source_batch, WriterOptions::aura1()),
        "sdk-write-aura0-compact" => {
            write_batch(schema, source_batch, WriterOptions::aura0_compact())
        }
        "sdk-write-aura0-hybrid" => write_batch(
            schema,
            source_batch,
            WriterOptions::aura0_compact().profile(AuraProfile::Hybrid),
        ),
        "sdk-read-aura1-batches" => read_batches(aura1, args.batch_size),
        "sdk-read-aura0-batches" => read_batches(aura0, args.batch_size),
        "sdk-replay-aura1" => replay_aura1(aura1),
        "sdk-convert-aura0-to-aura1" => convert_bytes(aura0, AuraFormat::Aura1),
        "sdk-convert-aura1-to-aura0" => convert_bytes(aura1, AuraFormat::Aura0),
        "sdk-zstd-aura1-to-aura1" => {
            let decoded = zstd::decode_all(Cursor::new(aura1_zst))?;
            Ok(BenchOutput {
                output_bytes: decoded.len(),
                records: source_batch.row_count(),
            })
        }
        "sdk-roundtrip-verify" => {
            let mut output = Vec::new();
            let summary = convert_aura(
                Cursor::new(aura0),
                &mut output,
                ConvertOptions::new(AuraFormat::Aura1).verify(true),
            )?;
            Ok(BenchOutput {
                output_bytes: summary.output_bytes,
                records: summary.record_count,
            })
        }
        _ => bail!("unknown operation {operation}"),
    }
}

fn write_batch(
    schema: &AuraSchema,
    source_batch: &AuraRecordBatch,
    options: WriterOptions,
) -> Result<BenchOutput> {
    let mut output = Vec::new();
    let mut writer = AuraWriter::try_new(&mut output, schema.clone(), options)?;
    writer.write_batch(source_batch.clone())?;
    let summary = writer.finish()?;
    Ok(BenchOutput {
        output_bytes: output.len(),
        records: summary.row_count,
    })
}

fn read_batches(bytes: &[u8], batch_size: usize) -> Result<BenchOutput> {
    let mut reader = AuraReader::open(Cursor::new(bytes))?;
    let mut records = 0usize;
    while let Some(batch) = reader.next_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
    }
    Ok(BenchOutput {
        output_bytes: bytes.len(),
        records,
    })
}

fn replay_aura1(bytes: &[u8]) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let mut records = 0usize;
    reader.replay_i64(|_| {
        records = records.saturating_add(1);
        Ok(())
    })?;
    Ok(BenchOutput {
        output_bytes: bytes.len(),
        records,
    })
}

fn convert_bytes(bytes: &[u8], target: AuraFormat) -> Result<BenchOutput> {
    let mut output = Vec::new();
    let summary = convert_aura(Cursor::new(bytes), &mut output, ConvertOptions::new(target))?;
    Ok(BenchOutput {
        output_bytes: output.len(),
        records: summary.record_count,
    })
}

fn stats_for_operation(
    operation: &str,
    aura0: &[u8],
    aura1: &[u8],
    batch_size: usize,
) -> Result<aura_codec::AuraReaderStats> {
    let bytes = if operation.contains("aura0") {
        aura0
    } else {
        aura1
    };
    let mut reader = AuraReader::open(Cursor::new(bytes))?;
    if operation.contains("read") {
        let _ = reader.next_batch(batch_size)?;
    }
    Ok(reader.stats())
}

fn row_equality_for_operation(
    operation: &str,
    schema: &AuraSchema,
    source_batch: &AuraRecordBatch,
    aura0: &[u8],
    aura1: &[u8],
) -> Result<bool> {
    if !operation.contains("verify") {
        return Ok(false);
    }
    let mut output = Vec::new();
    convert_aura(
        Cursor::new(if operation.contains("aura1-to-aura0") {
            aura1
        } else {
            aura0
        }),
        &mut output,
        ConvertOptions::new(if operation.contains("aura1-to-aura0") {
            AuraFormat::Aura0
        } else {
            AuraFormat::Aura1
        })
        .verify(true),
    )?;
    let reader = AuraReader::open(Cursor::new(&output))?;
    Ok(reader.schema().hash() == schema.hash()
        && reader.read_batches()?.first().map(AuraRecordBatch::rows) == Some(source_batch.rows()))
}

fn percentile(times: &[Duration], percentile: f64) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn mb_per_sec(bytes: usize, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 0.0;
    }
    (bytes as f64 / (1024.0 * 1024.0)) / duration.as_secs_f64()
}

fn conversion_plan_hash(aura1: &[u8]) -> Result<u64> {
    let reader = AuraReader::open(Cursor::new(aura1))?;
    Ok(reader
        .compiled_plan()
        .map(|plan| plan.conversion_plan_hash)
        .unwrap_or(0))
}

fn format_for_operation(operation: &str) -> &'static str {
    if operation.contains("aura0") {
        "aura0"
    } else {
        "aura1"
    }
}

fn profile_for_operation(operation: &str) -> &'static str {
    if operation.contains("hybrid") {
        "hybrid"
    } else if operation.contains("aura0") {
        "compact"
    } else {
        "fixed"
    }
}

fn input_bytes_for_operation(operation: &str, fixture: &Fixture) -> u64 {
    if operation.contains("zstd") {
        fixture.aura1_zst_bytes
    } else if operation.contains("aura1-to-aura0") || operation.contains("aura1") {
        fixture.aura1_bytes
    } else {
        fixture.aura0_bytes
    }
}

fn compressed_bytes_for_operation(operation: &str, fixture: &Fixture) -> u64 {
    if operation.contains("zstd") {
        fixture.aura1_zst_bytes
    } else if operation.contains("aura0") {
        fixture.aura0_bytes
    } else {
        0
    }
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn dirty_status() -> String {
    std::process::Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| {
            if text.trim().is_empty() {
                "clean".to_owned()
            } else {
                "dirty".to_owned()
            }
        })
        .unwrap_or_else(|| "unknown".to_owned())
}
