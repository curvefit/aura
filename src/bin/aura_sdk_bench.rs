#![recursion_limit = "256"]

use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aura_codec::{
    convert_aura, records, Aura1RowView, AuraColumn, AuraError, AuraFormat, AuraGroupStats,
    AuraProfile, AuraReader, AuraReaderStats, AuraRecordBatch, AuraSchema, AuraWriter,
    CompiledAuraField, ConvertOptions, GroupBy, WriterOptions,
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
    operations: Option<Vec<String>>,
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
    bytes_scanned: usize,
    bytes_touched: usize,
    fields_accessed: usize,
    values_decoded: usize,
    checksum: u64,
    rows_materialized: usize,
    values_materialized: usize,
    group_by_fields: Vec<String>,
    group_stats: Option<AuraGroupStats>,
    reader_stats: Option<AuraReaderStats>,
    dynamic_dispatch_count: usize,
    bounds_check_count: usize,
    kernel_group_count: usize,
    unsafe_loads_used: bool,
}

#[derive(Debug, Clone, Copy)]
enum GroupMode {
    Primary,
    Symbol,
    Pair,
}

#[derive(Debug, Clone, Copy)]
enum TouchMode {
    OneField,
    AllFields,
}

#[derive(Debug, Clone, Copy)]
enum AllFieldMode {
    FieldMajor,
    Unchecked,
    TypeKernel,
    InstructionTape,
}

#[derive(Debug, Clone, Copy)]
enum GroupTruthMode {
    ViewOnly,
    KeyOnly,
    OneFieldPerRow,
    AllFieldsPerRow,
    Aggregate,
}

impl BenchOutput {
    fn new(output_bytes: usize, records: usize) -> Self {
        Self {
            output_bytes,
            records,
            bytes_scanned: output_bytes,
            bytes_touched: 0,
            fields_accessed: 0,
            values_decoded: 0,
            checksum: 0,
            rows_materialized: 0,
            values_materialized: 0,
            group_by_fields: Vec::new(),
            group_stats: None,
            reader_stats: None,
            dynamic_dispatch_count: 0,
            bounds_check_count: 0,
            kernel_group_count: 0,
            unsafe_loads_used: false,
        }
    }

    fn with_row_materialization(mut self, field_count: usize) -> Self {
        self.rows_materialized = self.records;
        self.values_materialized = self.records.saturating_mul(field_count);
        self
    }

    fn with_column_materialization(mut self, field_count: usize) -> Self {
        self.values_materialized = self.records.saturating_mul(field_count);
        self
    }

    fn with_access(
        mut self,
        fields_accessed: usize,
        values_decoded: usize,
        bytes_touched: usize,
        checksum: u64,
    ) -> Self {
        self.fields_accessed = fields_accessed;
        self.values_decoded = values_decoded;
        self.bytes_touched = bytes_touched;
        self.checksum = checksum;
        self
    }

    fn with_group_stats(mut self, fields: Vec<String>, stats: AuraGroupStats) -> Self {
        self.group_by_fields = fields;
        self.group_stats = Some(stats);
        self
    }

    fn with_reader_stats(mut self, stats: AuraReaderStats) -> Self {
        self.reader_stats = Some(stats);
        self
    }

    fn with_parse_counters(
        mut self,
        dynamic_dispatch_count: usize,
        bounds_check_count: usize,
        kernel_group_count: usize,
        unsafe_loads_used: bool,
    ) -> Self {
        self.dynamic_dispatch_count = dynamic_dispatch_count;
        self.bounds_check_count = bounds_check_count;
        self.kernel_group_count = kernel_group_count;
        self.unsafe_loads_used = unsafe_loads_used;
        self
    }
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
    let mut operations = None;
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
            "--operations" => {
                let raw: String = next_parse(&mut args, "--operations")?;
                operations = Some(raw.split(',').map(str::to_owned).collect());
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
        operations,
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
        "usage: aura-sdk-bench --fixture-dir <dir> --output-dir <dir> [--iterations N] [--warmups N] [--batch-size N] [--datasets a,b] [--operations op1,op2]"
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
        "aura1-scan-raw",
        "aura1-scan-raw-file-range",
        "aura1-batch-view-only",
        "aura1-batch-view-only-file-range",
        "aura1-batch-touch-one-field",
        "aura1-batch-touch-one-field-file-range",
        "aura1-batch-touch-all-fields",
        "aura1-batch-touch-all-fields-file-range",
        "aura1-batch-touch-all-fields-field-major",
        "aura1-batch-touch-all-fields-field-major-file-range",
        "aura1-batch-touch-all-fields-unchecked",
        "aura1-batch-touch-all-fields-unchecked-file-range",
        "aura1-batch-touch-all-fields-type-kernel",
        "aura1-batch-touch-all-fields-type-kernel-file-range",
        "aura1-batch-touch-all-fields-instruction-tape",
        "aura1-batch-touch-all-fields-instruction-tape-file-range",
        "aura1-batch-selected-one-field",
        "aura1-batch-selected-one-field-file-range",
        "aura1-batch-selected-two-fields",
        "aura1-batch-selected-two-fields-file-range",
        "aura1-batch-selected-all-fields",
        "aura1-batch-selected-all-fields-file-range",
        "aura1-row-view-only",
        "aura1-row-view-only-file-range",
        "aura1-row-view-one-field",
        "aura1-row-view-one-field-file-range",
        "aura1-row-view-all-fields",
        "aura1-row-view-all-fields-file-range",
        "aura1-replay-i64",
        "aura1-replay-file-range",
        "aura1-replay-i64-current",
        "aura1-replay-i64-current-file-range",
        "aura1-replay-batch-callback",
        "aura1-replay-batch-callback-file-range",
        "aura1-read-batches-row",
        "aura1-read-batches-row-file-range",
        "aura1-read-batches-columnar",
        "aura1-read-batches-columnar-file-range",
        "aura1-grouped-replay-primary",
        "aura1-grouped-replay-primary-file-range",
        "aura1-grouped-replay-symbol",
        "aura1-grouped-replay-symbol-file-range",
        "aura1-grouped-replay-pair",
        "aura1-grouped-replay-pair-file-range",
        "aura1-grouped-view-only",
        "aura1-grouped-touch-key-only",
        "aura1-grouped-touch-one-field",
        "aura1-grouped-touch-all-fields",
        "aura1-grouped-aggregate",
    ];
    let mut results = Vec::with_capacity(operations.len());
    for operation in operations {
        if let Some(selected) = args.operations.as_deref() {
            if !selected.iter().any(|candidate| candidate == operation) {
                continue;
            }
        }
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
        bytes_scanned: 0,
        bytes_touched: 0,
        fields_accessed: 0,
        values_decoded: 0,
        checksum: 0,
        rows_materialized: 0,
        values_materialized: 0,
        group_by_fields: Vec::new(),
        group_stats: None,
        reader_stats: None,
        dynamic_dispatch_count: 0,
        bounds_check_count: 0,
        kernel_group_count: 0,
        unsafe_loads_used: false,
    };
    for _ in 0..args.warmups {
        last_output = run_operation(
            operation,
            schema,
            source_batch,
            aura0,
            aura1,
            &fixture.aura1_path,
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
            &fixture.aura1_path,
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
    let group_stats = last_output.group_stats;
    let groups_per_sec = group_stats
        .map(|stats| stats.group_count as f64 / median.as_secs_f64())
        .unwrap_or(0.0);
    let reader_stats = last_output.reader_stats.unwrap_or(stats_for_operation(
        operation,
        aura0,
        aura1,
        args.batch_size,
    )?);
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
        "bytes_scanned": last_output.bytes_scanned,
        "bytes_touched": last_output.bytes_touched,
        "fields_accessed": last_output.fields_accessed,
        "values_decoded": last_output.values_decoded,
        "checksum": last_output.checksum,
        "rows_materialized": last_output.rows_materialized,
        "values_materialized": last_output.values_materialized,
        "compiled_plan_used": true,
        "conversion_plan_hash": conversion_plan_hash(aura1).unwrap_or(0),
        "streaming_reader_used": reader_stats.streaming_reader_used,
        "full_file_materialized": reader_stats.full_file_materialized,
        "max_rows_materialized_at_once": reader_stats.max_rows_materialized_at_once,
        "source_kind": reader_stats.source_kind.as_str(),
        "replay_backend": reader_stats.replay_backend.as_str(),
        "file_len": reader_stats.file_len,
        "bytes_read_at_open": reader_stats.bytes_read_at_open,
        "body_bytes_read_at_open": reader_stats.body_bytes_read_at_open,
        "footer_bytes_read_at_open": reader_stats.footer_bytes_read_at_open,
        "bytes_read_during_replay": reader_stats.bytes_read_during_replay,
        "bytes_read_total": reader_stats.source_bytes_read_total,
        "bytes_read_in_last_batch": reader_stats.bytes_read_in_last_batch,
        "full_file_bytes_copied": reader_stats.full_file_bytes_copied,
        "row_width_from_plan": reader_stats.row_width_from_plan,
        "body_offset_from_header": reader_stats.body_offset_from_header,
        "footer_offset_from_trailer": reader_stats.footer_offset_from_trailer,
        "record_count_from_footer": reader_stats.record_count_from_footer,
        "rows_scanned": reader_stats.rows_scanned,
        "temp_row_buffers_allocated": reader_stats.temp_row_buffers_allocated,
        "visitor_calls": if group_stats.is_some() {
            group_stats.map(|stats| stats.callback_count).unwrap_or(0)
        } else {
            reader_stats.visitor_calls
        },
        "field_decode_count": reader_stats.field_decode_count,
        "endian_load_count": reader_stats.endian_load_count,
        "dynamic_dispatch_count": last_output.dynamic_dispatch_count,
        "bounds_check_count": last_output.bounds_check_count,
        "kernel_group_count": last_output.kernel_group_count,
        "unsafe_loads_used": last_output.unsafe_loads_used,
        "group_by_fields": last_output.group_by_fields,
        "group_count": group_stats.map(|stats| stats.group_count).unwrap_or(0),
        "callback_count": group_stats
            .map(|stats| stats.callback_count)
            .unwrap_or(reader_stats.visitor_calls),
        "group_row_count": group_stats.map(|stats| stats.row_count).unwrap_or(0),
        "groups_per_sec": groups_per_sec,
        "rows_per_group_avg": group_stats.map(|stats| stats.rows_per_group_avg).unwrap_or(0.0),
        "rows_per_group_p95": group_stats.map(|stats| stats.rows_per_group_p95).unwrap_or(0),
        "callback_count_reduction": group_stats
            .map(|stats| {
                if stats.callback_count == 0 {
                    0.0
                } else {
                    stats.row_count as f64 / stats.callback_count as f64
                }
            })
            .unwrap_or(0.0),
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
    aura1_path: &Path,
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
            let mut output = BenchOutput::new(decoded.len(), source_batch.row_count());
            output.bytes_scanned = aura1_zst.len();
            Ok(output)
        }
        "sdk-roundtrip-verify" => {
            let mut output = Vec::new();
            let summary = convert_aura(
                Cursor::new(aura0),
                &mut output,
                ConvertOptions::new(AuraFormat::Aura1).verify(true),
            )?;
            let mut output = BenchOutput::new(summary.output_bytes, summary.record_count);
            output.bytes_scanned = aura0.len();
            Ok(output)
        }
        "aura1-scan-raw" => scan_raw_aura1(aura1),
        "aura1-batch-view-only" => replay_aura1_batch_view_only(aura1, args.batch_size),
        "aura1-batch-view-only-file-range" => {
            replay_aura1_batch_view_only_path(aura1_path, args.batch_size)
        }
        "aura1-batch-touch-one-field" => {
            replay_aura1_batch_touch(aura1, args.batch_size, TouchMode::OneField)
        }
        "aura1-batch-touch-one-field-file-range" => {
            replay_aura1_batch_touch_path(aura1_path, args.batch_size, TouchMode::OneField)
        }
        "aura1-batch-touch-all-fields" => {
            replay_aura1_batch_touch(aura1, args.batch_size, TouchMode::AllFields)
        }
        "aura1-batch-touch-all-fields-file-range" => {
            replay_aura1_batch_touch_path(aura1_path, args.batch_size, TouchMode::AllFields)
        }
        "aura1-batch-touch-all-fields-field-major" => {
            replay_aura1_batch_all_fields_mode(aura1, args.batch_size, AllFieldMode::FieldMajor)
        }
        "aura1-batch-touch-all-fields-field-major-file-range" => {
            replay_aura1_batch_all_fields_mode_path(
                aura1_path,
                args.batch_size,
                AllFieldMode::FieldMajor,
            )
        }
        "aura1-batch-touch-all-fields-unchecked" => {
            replay_aura1_batch_all_fields_mode(aura1, args.batch_size, AllFieldMode::Unchecked)
        }
        "aura1-batch-touch-all-fields-unchecked-file-range" => {
            replay_aura1_batch_all_fields_mode_path(
                aura1_path,
                args.batch_size,
                AllFieldMode::Unchecked,
            )
        }
        "aura1-batch-touch-all-fields-type-kernel" => {
            replay_aura1_batch_all_fields_mode(aura1, args.batch_size, AllFieldMode::TypeKernel)
        }
        "aura1-batch-touch-all-fields-type-kernel-file-range" => {
            replay_aura1_batch_all_fields_mode_path(
                aura1_path,
                args.batch_size,
                AllFieldMode::TypeKernel,
            )
        }
        "aura1-batch-touch-all-fields-instruction-tape" => replay_aura1_batch_all_fields_mode(
            aura1,
            args.batch_size,
            AllFieldMode::InstructionTape,
        ),
        "aura1-batch-touch-all-fields-instruction-tape-file-range" => {
            replay_aura1_batch_all_fields_mode_path(
                aura1_path,
                args.batch_size,
                AllFieldMode::InstructionTape,
            )
        }
        "aura1-batch-selected-one-field" => replay_aura1_batch_selected(aura1, args.batch_size, 1),
        "aura1-batch-selected-one-field-file-range" => {
            replay_aura1_batch_selected_path(aura1_path, args.batch_size, 1)
        }
        "aura1-batch-selected-two-fields" => replay_aura1_batch_selected(aura1, args.batch_size, 2),
        "aura1-batch-selected-two-fields-file-range" => {
            replay_aura1_batch_selected_path(aura1_path, args.batch_size, 2)
        }
        "aura1-batch-selected-all-fields" => {
            replay_aura1_batch_selected(aura1, args.batch_size, usize::MAX)
        }
        "aura1-batch-selected-all-fields-file-range" => {
            replay_aura1_batch_selected_path(aura1_path, args.batch_size, usize::MAX)
        }
        "aura1-row-view-only" => replay_aura1_row_view_only(aura1),
        "aura1-row-view-only-file-range" => replay_aura1_row_view_only_path(aura1_path),
        "aura1-row-view-one-field" => replay_aura1_row_view(aura1, TouchMode::OneField),
        "aura1-row-view-one-field-file-range" => {
            replay_aura1_row_view_path(aura1_path, TouchMode::OneField)
        }
        "aura1-row-view-all-fields" => replay_aura1_row_view(aura1, TouchMode::AllFields),
        "aura1-row-view-all-fields-file-range" => {
            replay_aura1_row_view_path(aura1_path, TouchMode::AllFields)
        }
        "aura1-replay-i64" => replay_aura1(aura1),
        "aura1-replay-file-range" => replay_aura1_path(aura1_path),
        "aura1-replay-i64-current" => replay_aura1(aura1),
        "aura1-replay-i64-current-file-range" => replay_aura1_path(aura1_path),
        "aura1-replay-batch-callback" => replay_aura1_batches(aura1, args.batch_size),
        "aura1-replay-batch-callback-file-range" => {
            replay_aura1_batches_path(aura1_path, args.batch_size)
        }
        "aura1-read-batches-row" => read_batches(aura1, args.batch_size),
        "aura1-read-batches-row-file-range" => read_batches_path(aura1_path, args.batch_size),
        "aura1-read-batches-columnar" => read_column_batches(aura1, args.batch_size),
        "aura1-read-batches-columnar-file-range" => {
            read_column_batches_path(aura1_path, args.batch_size)
        }
        "aura1-scan-raw-file-range" => scan_raw_aura1_path(aura1_path),
        "aura1-grouped-replay-primary" => grouped_replay_aura1(aura1, schema, GroupMode::Primary),
        "aura1-grouped-replay-symbol" => grouped_replay_aura1(aura1, schema, GroupMode::Symbol),
        "aura1-grouped-replay-pair" => grouped_replay_aura1(aura1, schema, GroupMode::Pair),
        "aura1-grouped-replay-primary-file-range" => {
            grouped_replay_aura1_path(aura1_path, schema, GroupMode::Primary)
        }
        "aura1-grouped-replay-symbol-file-range" => {
            grouped_replay_aura1_path(aura1_path, schema, GroupMode::Symbol)
        }
        "aura1-grouped-replay-pair-file-range" => {
            grouped_replay_aura1_path(aura1_path, schema, GroupMode::Pair)
        }
        "aura1-grouped-view-only" => {
            grouped_replay_truth(aura1, schema, GroupMode::Primary, GroupTruthMode::ViewOnly)
        }
        "aura1-grouped-touch-key-only" => {
            grouped_replay_truth(aura1, schema, GroupMode::Primary, GroupTruthMode::KeyOnly)
        }
        "aura1-grouped-touch-one-field" => grouped_replay_truth(
            aura1,
            schema,
            GroupMode::Primary,
            GroupTruthMode::OneFieldPerRow,
        ),
        "aura1-grouped-touch-all-fields" => grouped_replay_truth(
            aura1,
            schema,
            GroupMode::Primary,
            GroupTruthMode::AllFieldsPerRow,
        ),
        "aura1-grouped-aggregate" => {
            grouped_replay_truth(aura1, schema, GroupMode::Primary, GroupTruthMode::Aggregate)
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
    Ok(BenchOutput::new(output.len(), summary.row_count)
        .with_row_materialization(schema.field_count()))
}

fn read_batches(bytes: &[u8], batch_size: usize) -> Result<BenchOutput> {
    let mut reader = AuraReader::open(Cursor::new(bytes))?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(bytes.len());
    let mut records = 0usize;
    let mut checksum = 0u64;
    while let Some(batch) = reader.next_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
        checksum = checksum_record_batch(checksum, &batch)?;
    }
    black_box(checksum);
    Ok(BenchOutput::new(bytes.len(), records)
        .with_row_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(reader.stats()))
}

fn read_batches_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(0);
    let mut records = 0usize;
    let mut checksum = 0u64;
    while let Some(batch) = reader.next_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
        checksum = checksum_record_batch(checksum, &batch)?;
    }
    black_box(checksum);
    let stats = reader.stats();
    Ok(BenchOutput::new(stats.file_len, records)
        .with_row_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(stats))
}

fn read_column_batches(bytes: &[u8], batch_size: usize) -> Result<BenchOutput> {
    let mut reader = AuraReader::open(Cursor::new(bytes))?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(bytes.len());
    let mut records = 0usize;
    let mut checksum = 0u64;
    while let Some(batch) = reader.next_column_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
        checksum = checksum_column_batch(checksum, &batch)?;
    }
    black_box(checksum);
    Ok(BenchOutput::new(bytes.len(), records)
        .with_column_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(reader.stats()))
}

fn read_column_batches_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(0);
    let mut records = 0usize;
    let mut checksum = 0u64;
    while let Some(batch) = reader.next_column_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
        checksum = checksum_column_batch(checksum, &batch)?;
    }
    black_box(checksum);
    let stats = reader.stats();
    Ok(BenchOutput::new(stats.file_len, records)
        .with_column_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(stats))
}

fn replay_aura1(bytes: &[u8]) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_i64(|row| {
        record_count = record_count.saturating_add(1);
        for value in row {
            checksum = mix_checksum(checksum, *value);
        }
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count);
    let field_count = reader.schema().field_count();
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(output.bytes_scanned);
    Ok(output
        .with_access(
            field_count,
            record_count.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(reader.stats()))
}

fn replay_aura1_path(path: &Path) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_i64(|row| {
        record_count = record_count.saturating_add(1);
        for value in row {
            checksum = mix_checksum(checksum, *value);
        }
        Ok(())
    })?;
    black_box(checksum);
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(0);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    let scanned = stats.bytes_read_during_replay;
    output.bytes_scanned = scanned;
    Ok(output.with_access(
        field_count,
        record_count.saturating_mul(field_count),
        bytes_touched,
        checksum,
    ))
}

fn replay_aura1_batches(bytes: &[u8], batch_size: usize) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.row_count() as u64);
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output.with_access(0, 0, 0, checksum))
}

fn replay_aura1_batches_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.row_count() as u64);
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(0, 0, 0, checksum))
}

fn replay_aura1_batch_view_only(bytes: &[u8], batch_size: usize) -> Result<BenchOutput> {
    replay_aura1_batches(bytes, batch_size)
}

fn replay_aura1_batch_view_only_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    replay_aura1_batches_path(path, batch_size)
}

fn replay_aura1_batch_touch(
    bytes: &[u8],
    batch_size: usize,
    mode: TouchMode,
) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = match mode {
            TouchMode::OneField => mix_checksum(checksum, batch.checksum_field(0)? as i64),
            TouchMode::AllFields => mix_checksum(checksum, batch.checksum_all_fields()? as i64),
        };
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output.with_access(
        fields_accessed,
        record_count.saturating_mul(fields_accessed),
        record_count.saturating_mul(bytes_per_row),
        checksum,
    ))
}

fn replay_aura1_batch_touch_path(
    path: &Path,
    batch_size: usize,
    mode: TouchMode,
) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = match mode {
            TouchMode::OneField => mix_checksum(checksum, batch.checksum_field(0)? as i64),
            TouchMode::AllFields => mix_checksum(checksum, batch.checksum_all_fields()? as i64),
        };
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(
        fields_accessed,
        record_count.saturating_mul(fields_accessed),
        record_count.saturating_mul(bytes_per_row),
        checksum,
    ))
}

fn replay_aura1_batch_all_fields_mode(
    bytes: &[u8],
    batch_size: usize,
    mode: AllFieldMode,
) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let field_count = reader.schema().field_count();
    let bytes_per_row = bytes_per_row_for_touch(&reader, TouchMode::AllFields)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(checksum_batch_all_fields(&batch, mode)?);
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output
        .with_access(
            field_count,
            record_count.saturating_mul(field_count),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(
            dynamic_dispatch_count_for_all_field_mode(mode, field_count),
            bounds_check_count_for_all_field_mode(mode, record_count, field_count),
            kernel_group_count_for_all_field_mode(mode, field_count),
            unsafe_loads_for_all_field_mode(mode),
        ))
}

fn replay_aura1_batch_all_fields_mode_path(
    path: &Path,
    batch_size: usize,
    mode: AllFieldMode,
) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_per_row = bytes_per_row_for_touch(&reader, TouchMode::AllFields)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(checksum_batch_all_fields(&batch, mode)?);
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = output
        .reader_stats
        .map(|stats| stats.bytes_read_during_replay)
        .unwrap_or_default();
    Ok(output
        .with_access(
            field_count,
            record_count.saturating_mul(field_count),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(
            dynamic_dispatch_count_for_all_field_mode(mode, field_count),
            bounds_check_count_for_all_field_mode(mode, record_count, field_count),
            kernel_group_count_for_all_field_mode(mode, field_count),
            unsafe_loads_for_all_field_mode(mode),
        ))
}

fn replay_aura1_batch_selected(
    bytes: &[u8],
    batch_size: usize,
    requested_fields: usize,
) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let selected = selected_field_indices(reader.schema().field_count(), requested_fields);
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.checksum_selected_fields(&selected)?);
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output
        .with_access(
            selected.len(),
            record_count.saturating_mul(selected.len()),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(selected.len(), selected.len(), selected.len(), false))
}

fn replay_aura1_batch_selected_path(
    path: &Path,
    batch_size: usize,
    requested_fields: usize,
) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let selected = selected_field_indices(reader.schema().field_count(), requested_fields);
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_fixed_batches(batch_size, |batch| {
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.checksum_selected_fields(&selected)?);
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = output
        .reader_stats
        .map(|stats| stats.bytes_read_during_replay)
        .unwrap_or_default();
    Ok(output
        .with_access(
            selected.len(),
            record_count.saturating_mul(selected.len()),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(selected.len(), selected.len(), selected.len(), false))
}

fn replay_aura1_row_view_only(bytes: &[u8]) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum.wrapping_add(row.field_count() as u64);
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output.with_access(0, 0, 0, checksum))
}

fn replay_aura1_row_view_only_path(path: &Path) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum.wrapping_add(row.field_count() as u64);
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(0, 0, 0, checksum))
}

fn replay_aura1_row_view(bytes: &[u8], mode: TouchMode) -> Result<BenchOutput> {
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum_row_view(checksum, row, mode)?;
        Ok(())
    })?;
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), record_count).with_reader_stats(reader.stats());
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output.with_access(
        fields_accessed,
        record_count.saturating_mul(fields_accessed),
        record_count.saturating_mul(bytes_per_row),
        checksum,
    ))
}

fn replay_aura1_row_view_path(path: &Path, mode: TouchMode) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum_row_view(checksum, row, mode)?;
        Ok(())
    })?;
    black_box(checksum);
    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(
        fields_accessed,
        record_count.saturating_mul(fields_accessed),
        record_count.saturating_mul(bytes_per_row),
        checksum,
    ))
}

fn scan_raw_aura1(bytes: &[u8]) -> Result<BenchOutput> {
    let info = records::aura1_fixed_layout_info(bytes)?;
    let body = bytes
        .get(info.body_offset..info.body_offset.saturating_add(info.body_bytes))
        .ok_or_else(|| anyhow::anyhow!("invalid Aura1 body range"))?;
    let mut checksum = 0u8;
    let stride = info.record_width.max(1);
    for row in body.chunks(stride) {
        checksum ^= row.first().copied().unwrap_or(0);
    }
    black_box(checksum);
    let mut output = BenchOutput::new(bytes.len(), info.record_count);
    output.bytes_scanned = info.body_bytes;
    Ok(output)
}

fn scan_raw_aura1_path(path: &Path) -> Result<BenchOutput> {
    let reader = AuraReader::open_path(path)?;
    let stats_at_open = reader.stats();
    let mut file = fs::File::open(path)?;
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(
        stats_at_open.body_offset_from_header as u64,
    ))?;
    let mut remaining = stats_at_open
        .footer_offset_from_trailer
        .saturating_sub(stats_at_open.body_offset_from_header);
    let mut buffer = vec![0u8; 256 * 1024];
    let mut checksum = 0u8;
    let mut bytes_read = 0usize;
    while remaining > 0 {
        let len = remaining.min(buffer.len());
        file.read_exact(&mut buffer[..len])?;
        for chunk in buffer[..len].chunks(stats_at_open.row_width_from_plan.max(1)) {
            checksum ^= chunk.first().copied().unwrap_or(0);
        }
        remaining -= len;
        bytes_read = bytes_read.saturating_add(len);
    }
    black_box(checksum);
    let mut stats = stats_at_open;
    stats.bytes_read_during_replay = bytes_read;
    stats.source_bytes_read_total = stats.source_bytes_read_total.saturating_add(bytes_read);
    let mut output =
        BenchOutput::new(stats.file_len, stats.record_count_from_footer).with_reader_stats(stats);
    output.bytes_scanned = bytes_read;
    Ok(output)
}

fn grouped_replay_aura1(bytes: &[u8], schema: &AuraSchema, mode: GroupMode) -> Result<BenchOutput> {
    let fields = group_fields(schema, mode)?;
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let mut callback_count = 0usize;
    let stats = reader.grouped_replay(&GroupBy::fields(fields.iter().cloned()), |_| {
        callback_count = callback_count.saturating_add(1);
        Ok(())
    })?;
    black_box(callback_count);
    let mut output = BenchOutput::new(bytes.len(), stats.row_count).with_group_stats(fields, stats);
    if let Ok(info) = records::aura1_fixed_layout_info(bytes) {
        output.bytes_scanned = info.body_bytes;
    }
    Ok(output.with_reader_stats(reader.stats()))
}

fn grouped_replay_aura1_path(
    path: &Path,
    schema: &AuraSchema,
    mode: GroupMode,
) -> Result<BenchOutput> {
    let fields = group_fields(schema, mode)?;
    let reader = AuraReader::open_path(path)?;
    let mut callback_count = 0usize;
    let stats = reader.grouped_replay(&GroupBy::fields(fields.iter().cloned()), |_| {
        callback_count = callback_count.saturating_add(1);
        Ok(())
    })?;
    black_box(callback_count);
    let reader_stats = reader.stats();
    let mut output =
        BenchOutput::new(reader_stats.file_len, stats.row_count).with_group_stats(fields, stats);
    output.bytes_scanned = reader_stats.bytes_read_during_replay;
    Ok(output.with_reader_stats(reader_stats))
}

fn grouped_replay_truth(
    bytes: &[u8],
    schema: &AuraSchema,
    group_mode: GroupMode,
    truth_mode: GroupTruthMode,
) -> Result<BenchOutput> {
    let fields = group_fields(schema, group_mode)?;
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let info = records::aura1_fixed_layout_info(bytes)?;
    let body = bytes
        .get(info.body_offset..info.body_offset.saturating_add(info.body_bytes))
        .ok_or_else(|| anyhow::anyhow!("invalid Aura1 body range"))?;
    let slots = bench_field_slots(&reader)?;
    let field_count = reader.schema().field_count();
    let bytes_per_row = match truth_mode {
        GroupTruthMode::ViewOnly | GroupTruthMode::KeyOnly => 0,
        GroupTruthMode::OneFieldPerRow => slots.first().map(|field| field.width).unwrap_or(0),
        GroupTruthMode::AllFieldsPerRow => info.record_width,
        GroupTruthMode::Aggregate => slots
            .iter()
            .take(field_count.min(2))
            .map(|field| field.width)
            .sum(),
    };
    let mut checksum = 0u64;
    let mut values_decoded = 0usize;
    let stats = reader.grouped_replay(&GroupBy::fields(fields.iter().cloned()), |group| {
        match truth_mode {
            GroupTruthMode::ViewOnly => {
                checksum = checksum.wrapping_add(group.row_count() as u64);
            }
            GroupTruthMode::KeyOnly => {
                for (name, value) in group.key().field_names().iter().zip(group.key().values()) {
                    let aura_type = schema
                        .fields()
                        .iter()
                        .find(|field| &field.name == name)
                        .map(|field| field.aura_type)
                        .ok_or(AuraError::InvalidValue("group field"))?;
                    checksum = mix_checksum(checksum, value.to_i64_for_type(aura_type)?);
                    values_decoded = values_decoded.saturating_add(1);
                }
            }
            GroupTruthMode::OneFieldPerRow => {
                checksum = checksum_group_rows(
                    checksum,
                    body,
                    info.record_width,
                    &slots,
                    group.row_start(),
                    group.row_count(),
                    TouchMode::OneField,
                )?;
                values_decoded = values_decoded.saturating_add(group.row_count());
            }
            GroupTruthMode::AllFieldsPerRow => {
                checksum = checksum_group_rows(
                    checksum,
                    body,
                    info.record_width,
                    &slots,
                    group.row_start(),
                    group.row_count(),
                    TouchMode::AllFields,
                )?;
                values_decoded =
                    values_decoded.saturating_add(group.row_count().saturating_mul(field_count));
            }
            GroupTruthMode::Aggregate => {
                let (next_checksum, decoded) = aggregate_group_rows(
                    checksum,
                    body,
                    info.record_width,
                    &slots,
                    group.row_start(),
                    group.row_count(),
                )?;
                checksum = next_checksum;
                values_decoded = values_decoded.saturating_add(decoded);
            }
        }
        Ok(())
    })?;
    black_box(checksum);
    let reader_stats = reader.stats();
    let fields_accessed = match truth_mode {
        GroupTruthMode::ViewOnly => 0,
        GroupTruthMode::KeyOnly => fields.len(),
        GroupTruthMode::OneFieldPerRow => 1.min(field_count),
        GroupTruthMode::AllFieldsPerRow => field_count,
        GroupTruthMode::Aggregate => field_count.min(2),
    };
    let mut output = BenchOutput::new(bytes.len(), stats.row_count).with_group_stats(fields, stats);
    output.bytes_scanned = info.body_bytes;
    Ok(output
        .with_access(
            fields_accessed,
            values_decoded,
            stats.row_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_reader_stats(reader_stats))
}

fn group_fields(schema: &AuraSchema, mode: GroupMode) -> Result<Vec<String>> {
    let primary = schema
        .fields()
        .iter()
        .find(|field| {
            matches!(
                field.aura_type,
                aura_codec::AuraType::TimestampNanos | aura_codec::AuraType::TimestampMicros
            )
        })
        .or_else(|| schema.fields().first())
        .ok_or_else(|| anyhow::anyhow!("schema has no fields"))?;
    let symbol = schema.fields().iter().find(|field| {
        field.name.contains("symbol")
            || field.name.contains("venue")
            || field.name.ends_with("_id")
            || field.name.ends_with("id")
    });
    match mode {
        GroupMode::Primary => Ok(vec![primary.name.clone()]),
        GroupMode::Symbol => Ok(vec![symbol.unwrap_or(primary).name.clone()]),
        GroupMode::Pair => {
            let mut fields = vec![primary.name.clone()];
            if let Some(second) = symbol {
                if second.name != primary.name {
                    fields.push(second.name.clone());
                }
            }
            if fields.len() == 1 {
                if let Some(second) = schema
                    .fields()
                    .iter()
                    .find(|field| field.name != primary.name)
                {
                    fields.push(second.name.clone());
                }
            }
            Ok(fields)
        }
    }
}

fn fields_accessed(field_count: usize, mode: TouchMode) -> usize {
    match mode {
        TouchMode::OneField => usize::from(field_count > 0),
        TouchMode::AllFields => field_count,
    }
}

fn bytes_per_row_for_touch(reader: &AuraReader, mode: TouchMode) -> Result<usize> {
    let plan = reader
        .compiled_plan()
        .ok_or_else(|| anyhow::anyhow!("compiled plan missing"))?;
    match mode {
        TouchMode::OneField => Ok(plan
            .aura1_field_offsets()
            .first()
            .map(|field| field.width)
            .unwrap_or(0)),
        TouchMode::AllFields => Ok(plan.aura1_record_width),
    }
}

fn selected_field_indices(field_count: usize, requested_fields: usize) -> Vec<usize> {
    let selected_count = requested_fields.min(field_count);
    (0..selected_count).collect()
}

fn selected_bytes_per_row(reader: &AuraReader, field_indices: &[usize]) -> Result<usize> {
    let slots = bench_field_slots(reader)?;
    field_indices.iter().try_fold(0usize, |acc, index| {
        let field = slots
            .get(*index)
            .ok_or_else(|| anyhow::anyhow!("field index out of bounds"))?;
        acc.checked_add(field.width)
            .ok_or_else(|| anyhow::anyhow!("field width overflow"))
    })
}

fn checksum_batch_all_fields(
    batch: &aura_codec::Aura1FixedBatchView<'_>,
    mode: AllFieldMode,
) -> std::result::Result<u64, AuraError> {
    match mode {
        AllFieldMode::FieldMajor => batch.checksum_all_fields_field_major(),
        AllFieldMode::Unchecked => batch.checksum_all_fields_checked_once(),
        AllFieldMode::TypeKernel => batch.checksum_all_fields_type_kernel(),
        AllFieldMode::InstructionTape => batch.checksum_all_fields_parse_program(),
    }
}

fn dynamic_dispatch_count_for_all_field_mode(mode: AllFieldMode, field_count: usize) -> usize {
    match mode {
        AllFieldMode::FieldMajor => field_count,
        AllFieldMode::Unchecked => field_count,
        AllFieldMode::TypeKernel => kernel_group_count_for_all_field_mode(mode, field_count),
        AllFieldMode::InstructionTape => field_count,
    }
}

fn bounds_check_count_for_all_field_mode(
    mode: AllFieldMode,
    record_count: usize,
    field_count: usize,
) -> usize {
    match mode {
        AllFieldMode::FieldMajor => record_count.saturating_mul(field_count),
        AllFieldMode::Unchecked | AllFieldMode::TypeKernel | AllFieldMode::InstructionTape => {
            field_count
        }
    }
}

fn kernel_group_count_for_all_field_mode(mode: AllFieldMode, field_count: usize) -> usize {
    match mode {
        AllFieldMode::TypeKernel => field_count.min(4),
        AllFieldMode::InstructionTape => field_count,
        _ => 0,
    }
}

fn unsafe_loads_for_all_field_mode(mode: AllFieldMode) -> bool {
    matches!(
        mode,
        AllFieldMode::Unchecked | AllFieldMode::TypeKernel | AllFieldMode::InstructionTape
    )
}

fn checksum_row_view(
    checksum: u64,
    row: Aura1RowView<'_>,
    mode: TouchMode,
) -> std::result::Result<u64, AuraError> {
    let mut checksum = checksum;
    match mode {
        TouchMode::OneField => {
            if row.field_count() > 0 {
                checksum = mix_checksum(checksum, row.get_i64(0)?);
            }
        }
        TouchMode::AllFields => {
            for field_index in 0..row.field_count() {
                checksum = mix_checksum(checksum, row.get_i64(field_index)?);
            }
        }
    }
    Ok(checksum)
}

fn bench_field_slots(reader: &AuraReader) -> Result<Vec<CompiledAuraField>> {
    let plan = reader
        .compiled_plan()
        .ok_or_else(|| anyhow::anyhow!("compiled plan missing"))?;
    let mut slots = vec![
        CompiledAuraField {
            field_index: 0,
            offset: 0,
            width: 0,
        };
        plan.field_count
    ];
    for field in plan.aura1_field_offsets() {
        let index = usize::from(field.field_index);
        let slot = slots
            .get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("field index out of bounds"))?;
        *slot = field;
    }
    Ok(slots)
}

fn checksum_record_batch(mut checksum: u64, batch: &AuraRecordBatch) -> Result<u64> {
    for row in batch.rows() {
        for (value, field) in row.iter().zip(batch.schema().fields()) {
            checksum = mix_checksum(checksum, value.to_i64_for_type(field.aura_type)?);
        }
    }
    Ok(checksum)
}

fn checksum_column_batch(mut checksum: u64, batch: &aura_codec::AuraColumnBatch) -> Result<u64> {
    for (column, field) in batch.columns().iter().zip(batch.schema().fields()) {
        match column {
            AuraColumn::Bool(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::U8(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::U16(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::U32(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::U64(values) => {
                for value in values {
                    let value = i64::try_from(*value)
                        .map_err(|_| anyhow::anyhow!("u64 value out of i64 range"))?;
                    checksum = mix_checksum(checksum, value);
                }
            }
            AuraColumn::I8(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::I16(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::I32(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, i64::from(*value));
                }
            }
            AuraColumn::I64(values) => {
                for value in values {
                    checksum = mix_checksum(checksum, *value);
                }
            }
        }
        black_box(field.aura_type);
    }
    Ok(checksum)
}

fn checksum_group_rows(
    checksum: u64,
    body: &[u8],
    record_width: usize,
    fields: &[CompiledAuraField],
    row_start: usize,
    row_count: usize,
    mode: TouchMode,
) -> std::result::Result<u64, AuraError> {
    let mut checksum = checksum;
    let row_end = row_start
        .checked_add(row_count)
        .ok_or(AuraError::InvalidValue("row range"))?;
    for row_index in row_start..row_end {
        let start = row_index
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("row offset"))?;
        let end = start
            .checked_add(record_width)
            .ok_or(AuraError::InvalidValue("row offset"))?;
        let row = body.get(start..end).ok_or(AuraError::UnexpectedEof)?;
        match mode {
            TouchMode::OneField => {
                if let Some(field) = fields.first() {
                    checksum = mix_checksum(checksum, read_field_i64(row, *field)?);
                }
            }
            TouchMode::AllFields => {
                for field in fields {
                    checksum = mix_checksum(checksum, read_field_i64(row, *field)?);
                }
            }
        }
    }
    Ok(checksum)
}

fn aggregate_group_rows(
    checksum: u64,
    body: &[u8],
    record_width: usize,
    fields: &[CompiledAuraField],
    row_start: usize,
    row_count: usize,
) -> std::result::Result<(u64, usize), AuraError> {
    let mut checksum = checksum;
    let mut decoded = 0usize;
    let row_end = row_start
        .checked_add(row_count)
        .ok_or(AuraError::InvalidValue("row range"))?;
    let first = fields.first().copied();
    let second = fields.get(1).copied();
    let mut sum = 0i64;
    let mut min_second = i64::MAX;
    let mut max_second = i64::MIN;
    for row_index in row_start..row_end {
        let start = row_index
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("row offset"))?;
        let end = start
            .checked_add(record_width)
            .ok_or(AuraError::InvalidValue("row offset"))?;
        let row = body.get(start..end).ok_or(AuraError::UnexpectedEof)?;
        if let Some(field) = first {
            sum = sum.wrapping_add(read_field_i64(row, field)?);
            decoded = decoded.saturating_add(1);
        }
        if let Some(field) = second {
            let value = read_field_i64(row, field)?;
            min_second = min_second.min(value);
            max_second = max_second.max(value);
            decoded = decoded.saturating_add(1);
        }
    }
    checksum = mix_checksum(checksum, row_count as i64);
    checksum = mix_checksum(checksum, sum);
    if second.is_some() {
        checksum = mix_checksum(checksum, min_second);
        checksum = mix_checksum(checksum, max_second);
    }
    Ok((checksum, decoded))
}

fn read_field_i64(row: &[u8], field: CompiledAuraField) -> std::result::Result<i64, AuraError> {
    let end = field
        .offset
        .checked_add(field.width)
        .ok_or(AuraError::InvalidValue("field offset"))?;
    let bytes = row.get(field.offset..end).ok_or(AuraError::UnexpectedEof)?;
    read_i64_fixed_width(bytes)
}

fn read_i64_fixed_width(bytes: &[u8]) -> std::result::Result<i64, AuraError> {
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(bytes[0] as i8 as i64),
        2 => Ok(i16::from_le_bytes([bytes[0], bytes[1]]) as i64),
        4 => Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64),
        8 => Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        16 => {
            let value = i128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 value"))
        }
        _ => Err(AuraError::InvalidValue("field width")),
    }
}

fn mix_checksum(checksum: u64, value: i64) -> u64 {
    checksum.wrapping_mul(0x9E37_79B1_85EB_CA87).rotate_left(7)
        ^ (value as u64).wrapping_add(0xC2B2_AE3D_27D4_EB4F)
}

fn convert_bytes(bytes: &[u8], target: AuraFormat) -> Result<BenchOutput> {
    let mut output = Vec::new();
    let summary = convert_aura(Cursor::new(bytes), &mut output, ConvertOptions::new(target))?;
    let mut bench = BenchOutput::new(output.len(), summary.record_count);
    bench.bytes_scanned = bytes.len();
    Ok(bench)
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
    if operation.contains("columnar") {
        let _ = reader.next_column_batch(batch_size)?;
    } else if operation.contains("read") {
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
    if operation.starts_with("aura1-") {
        "aura1"
    } else if operation.contains("aura0") {
        "aura0"
    } else {
        "aura1"
    }
}

fn profile_for_operation(operation: &str) -> &'static str {
    if operation.starts_with("aura1-") {
        "fixed"
    } else if operation.contains("hybrid") {
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
    } else if operation.starts_with("aura1-")
        || operation.contains("aura1-to-aura0")
        || operation.contains("aura1")
    {
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
