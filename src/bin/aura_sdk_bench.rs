#![recursion_limit = "512"]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aura_codec::{
    convert_aura, records, Aura1RowView, AuraColumn, AuraError, AuraEventBatch, AuraEventSource,
    AuraEventSourceStats, AuraFileSource, AuraFormat, AuraGroupStats, AuraLiveFrameSource,
    AuraLiveSource, AuraMemorySource, AuraProfile, AuraReader, AuraReaderSourceKind,
    AuraReaderStats, AuraRecordBatch, AuraReplayBackend, AuraSchema, AuraType, AuraWriter,
    BookApplyBreakdown, BookApplyStats, BookStateHash, CompiledAuraField, CompiledAuraPlan,
    ConvertOptions, GroupBy, OrderBookApplyMode, OrderBookApplyPlan, OrderBookDelta,
    OrderBookDeltaSpec, OrderBookEngine, OrderBookEngineKind, OrderBookLifecycleMode,
    OrderBookReplaySession, PreparedOrderBookApplyPlan, PreparedOrderBookEngine, WriterOptions,
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

#[derive(Debug, Clone)]
struct StageBreakdown {
    times_ms: BTreeMap<&'static str, f64>,
    counters: BTreeMap<&'static str, u64>,
    timer_tree_kind: &'static str,
}

#[derive(Clone)]
struct ExtractedOrderBookDeltas {
    deltas: Vec<OrderBookDelta>,
    payload_field_count: usize,
    bytes_per_row: usize,
    bytes_scanned: usize,
    reader_stats: AuraReaderStats,
}

struct PreparedOrderBookBenchmark {
    session: OrderBookReplaySession,
    extracted: ExtractedOrderBookDeltas,
    expected_hash: BookStateHash,
    setup_ns: u128,
    plan_build_ns: u128,
    allocation_ns: u128,
}

#[derive(Debug, Clone, Copy)]
struct OrderBookBenchmarkSpec {
    kind: OrderBookEngineKind,
    apply_mode: OrderBookApplyMode,
    lifecycle_mode: OrderBookLifecycleMode,
    include_extract: bool,
}

impl StageBreakdown {
    fn summed() -> Self {
        Self {
            times_ms: BTreeMap::new(),
            counters: BTreeMap::new(),
            timer_tree_kind: "summed",
        }
    }

    fn add_duration(&mut self, name: &'static str, duration: Duration) {
        self.add_ms(name, duration_ms(duration));
    }

    fn add_ms(&mut self, name: &'static str, ms: f64) {
        if ms <= 0.0 {
            self.times_ms.entry(name).or_insert(0.0);
            return;
        }
        *self.times_ms.entry(name).or_insert(0.0) += ms;
    }

    fn add_counter(&mut self, name: &'static str, value: usize) {
        self.counters.insert(name, value as u64);
    }

    fn stage_sum_ms(&self) -> f64 {
        self.times_ms.values().sum()
    }
}

#[derive(Clone)]
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
    replay_mode: Option<&'static str>,
    source_kind_override: Option<&'static str>,
    source_stats: Option<AuraEventSourceStats>,
    allocations_proxy_override: Option<usize>,
    book_levels: usize,
    instrument_count: usize,
    price_level_count: usize,
    book_adds: usize,
    book_modifies: usize,
    book_deletes: usize,
    zero_size_removes: usize,
    state_hash: u64,
    engine_kind: Option<&'static str>,
    allocation_count_proxy: usize,
    bytes_allocated_proxy: usize,
    cache_shape: Option<&'static str>,
    apply_breakdown: Option<BookApplyBreakdown>,
    apply_mode: Option<OrderBookApplyMode>,
    lifecycle_mode: Option<OrderBookLifecycleMode>,
    state_hash_checked: bool,
    engine_reused: bool,
    buffers_reused: bool,
    fused_enabled: bool,
    benchmark_class: &'static str,
    stages: StageBreakdown,
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
            replay_mode: None,
            source_kind_override: None,
            source_stats: None,
            allocations_proxy_override: None,
            book_levels: 0,
            instrument_count: 0,
            price_level_count: 0,
            book_adds: 0,
            book_modifies: 0,
            book_deletes: 0,
            zero_size_removes: 0,
            state_hash: 0,
            engine_kind: None,
            allocation_count_proxy: 0,
            bytes_allocated_proxy: 0,
            cache_shape: None,
            apply_breakdown: None,
            apply_mode: None,
            lifecycle_mode: None,
            state_hash_checked: false,
            engine_reused: false,
            buffers_reused: false,
            fused_enabled: false,
            benchmark_class: "other",
            stages: StageBreakdown::summed(),
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

    fn with_replay_mode(mut self, replay_mode: &'static str) -> Self {
        self.replay_mode = Some(replay_mode);
        self.benchmark_class = "replay";
        self
    }

    fn with_source_kind(mut self, source_kind: &'static str) -> Self {
        self.source_kind_override = Some(source_kind);
        self
    }

    fn with_source_stats(mut self, source_stats: AuraEventSourceStats) -> Self {
        self.source_stats = Some(source_stats);
        self
    }

    fn with_book_metrics(mut self, metrics: BookApplyMetrics) -> Self {
        self.book_levels = metrics.book_levels;
        self.instrument_count = metrics.instruments;
        self.price_level_count = metrics.price_levels;
        self.book_adds = metrics.adds;
        self.book_modifies = metrics.modifies;
        self.book_deletes = metrics.deletes;
        self.state_hash = metrics.state_hash;
        self.allocations_proxy_override = Some(metrics.allocations_proxy);
        self.benchmark_class = "orderbook apply";
        self
    }

    fn with_book_stats(mut self, stats: BookApplyStats) -> Self {
        self.book_levels = stats.book_levels;
        self.instrument_count = stats.instruments;
        self.price_level_count = stats.price_levels;
        self.book_adds = stats.adds;
        self.book_modifies = stats.modifies;
        self.book_deletes = stats.deletes;
        self.zero_size_removes = stats.zero_size_removes;
        self.state_hash = stats.state_hash.0;
        self.engine_kind = Some(stats.engine_kind);
        self.allocations_proxy_override = Some(stats.allocation_count_proxy);
        self.allocation_count_proxy = stats.allocation_count_proxy;
        self.bytes_allocated_proxy = stats.bytes_allocated_proxy;
        self.cache_shape = Some(stats.cache_shape);
        self.apply_breakdown = Some(stats.breakdown);
        self.apply_mode = Some(stats.apply_mode);
        self.lifecycle_mode = Some(stats.lifecycle_mode);
        self.state_hash_checked = stats.state_hash_checked;
        self.engine_reused = stats.engine_reused;
        self.buffers_reused = stats.buffers_reused;
        self.benchmark_class = "orderbook apply";
        self
    }

    fn with_fused_enabled(mut self) -> Self {
        self.fused_enabled = true;
        self
    }

    fn with_stages(mut self, stages: StageBreakdown) -> Self {
        self.stages = stages;
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
        "aura1-replay-per-row-noop",
        "aura1-replay-per-row-touch-selected",
        "aura1-replay-per-row-touch-all",
        "aura1-replay-batch-noop",
        "aura1-replay-batch-touch-selected",
        "aura1-replay-batch-touch-all",
        "aura1-replay-orderbook-deltas-batch",
        "aura1-replay-orderbook-deltas-apply-batch",
        "aura1-event-source-file-orderbook-apply",
        "aura1-event-source-memory-orderbook-apply",
        "aura1-event-source-live-orderbook-apply",
        "aura1-event-source-live-frame-orderbook-apply",
        "aura1-book-apply-current",
        "aura1-book-apply-packed-key",
        "aura1-book-apply-per-instrument",
        "aura1-book-apply-side-split",
        "aura1-book-apply-dense-ladder",
        "aura1-book-apply-btree",
        "orderbook-apply-only-current",
        "orderbook-apply-only-dense-ladder",
        "orderbook-apply-only-paged-ladder",
        "orderbook-apply-only-packed-key",
        "orderbook-apply-only-direct-index",
        "orderbook-apply-only-run-locality",
        "orderbook-apply-only-optimized",
        "orderbook-apply-only-btree",
        "orderbook-apply-only-optimized-production-cold",
        "orderbook-apply-only-optimized-verify-cold",
        "orderbook-apply-only-optimized-production-prepared",
        "orderbook-apply-only-optimized-verify-prepared",
        "aura1-orderbook-extract-only",
        "aura1-orderbook-extract-plus-apply-current",
        "aura1-orderbook-extract-plus-apply-dense-ladder",
        "aura1-orderbook-extract-plus-apply-paged-ladder",
        "aura1-orderbook-extract-plus-apply-direct-index",
        "aura1-orderbook-extract-plus-apply-run-locality",
        "aura1-orderbook-extract-plus-apply-optimized",
        "aura1-orderbook-extract-plus-apply-optimized-production-cold",
        "aura1-orderbook-extract-plus-apply-optimized-verify-cold",
        "aura1-orderbook-extract-plus-apply-optimized-production-prepared",
        "aura1-orderbook-extract-plus-apply-optimized-verify-prepared",
        "aura1-orderbook-fused-extract-plus-apply-optimized-production-prepared",
        "aura1-orderbook-fused-extract-plus-apply-optimized-verify-prepared",
        "aura1-replay-grouped-touch-selected",
        "aura1-replay-grouped-touch-all",
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
        } else if operation == "aura1-replay-orderbook-deltas-batch"
            || operation == "aura1-replay-orderbook-deltas-apply-batch"
        {
            continue;
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

#[allow(clippy::too_many_arguments)]
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
    let spec = orderbook_benchmark_spec(operation);
    let fused_spec = fused_orderbook_benchmark_spec(operation);
    let preextracted_orderbook = if orderbook_apply_only_kind(operation).is_some()
        || matches!(
            spec,
            Some(OrderBookBenchmarkSpec {
                include_extract: false,
                lifecycle_mode: OrderBookLifecycleMode::Cold,
                ..
            })
        ) {
        Some(extract_orderbook_deltas_for_engine(
            &fixture.aura1_path,
            args.batch_size,
        )?)
    } else {
        None
    };
    let mut prepared_orderbook = if matches!(
        spec.or(fused_spec),
        Some(OrderBookBenchmarkSpec {
            lifecycle_mode: OrderBookLifecycleMode::Prepared,
            ..
        }) | Some(OrderBookBenchmarkSpec {
            apply_mode: OrderBookApplyMode::Verify,
            ..
        })
    ) {
        Some(prepare_orderbook_benchmark(
            &fixture.aura1_path,
            args.batch_size,
            spec.or(fused_spec)
                .ok_or_else(|| anyhow::anyhow!("missing orderbook benchmark spec"))?
                .kind,
        )?)
    } else {
        None
    };
    let mut last_output = BenchOutput::new(0, fixture.record_count);
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
            preextracted_orderbook.as_ref(),
            prepared_orderbook.as_mut(),
        )?;
    }
    let mut samples = Vec::with_capacity(args.iterations);
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
            preextracted_orderbook.as_ref(),
            prepared_orderbook.as_mut(),
        )?;
        samples.push((start.elapsed(), last_output.clone()));
    }
    let times = samples
        .iter()
        .map(|(elapsed, _)| *elapsed)
        .collect::<Vec<_>>();
    let median = percentile(&times, 0.5);
    let p95 = percentile(&times, 0.95);
    if let Some((_elapsed, median_output)) = samples
        .iter()
        .min_by_key(|(elapsed, _)| elapsed.abs_diff(median))
    {
        last_output = median_output.clone();
    }
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
    let source_stats = last_output.source_stats.unwrap_or(AuraEventSourceStats {
        source_kind: last_output
            .source_kind_override
            .unwrap_or_else(|| reader_stats.source_kind.as_str()),
        buffer_reuse_enabled: false,
        bytes_copied: reader_stats.full_file_bytes_copied,
        bytes_borrowed_or_ranged: reader_stats.bytes_read_during_replay,
        allocations_proxy: reader_stats.temp_row_buffers_allocated,
        rows_materialized: reader_stats.max_rows_materialized_at_once,
        full_file_materialized: reader_stats.full_file_materialized,
        full_file_bytes_copied: reader_stats.full_file_bytes_copied,
    });
    let stage_sum_ms = last_output.stages.stage_sum_ms();
    let runtime_ms = duration_ms(median);
    let extract_ms = measured_ms(
        &last_output.stages,
        &[
            "event_source_extract_ms",
            "orderbook_extract_ms",
            "orderbook_delta_loop_ms",
        ],
    );
    let apply_ms = measured_ms(
        &last_output.stages,
        &[
            "event_source_apply_ms",
            "event_source_orderbook_apply_loop_ms",
            "orderbook_apply_ms",
            "orderbook_decode_apply_loop_ms",
        ],
    );
    let apply_recs_per_sec = if apply_ms > 0.0 {
        records as f64 / (apply_ms / 1000.0)
    } else {
        0.0
    };
    let unexplained_ms = runtime_ms - stage_sum_ms;
    let unexplained_pct = if runtime_ms > 0.0 {
        (unexplained_ms / runtime_ms) * 100.0
    } else {
        0.0
    };
    let mut counters = last_output.stages.counters.clone();
    counters.entry("record_count").or_insert(records as u64);
    counters
        .entry("record_width")
        .or_insert(reader_stats.row_width_from_plan as u64);
    counters
        .entry("field_count")
        .or_insert(fixture.field_count as u64);
    counters
        .entry("fields_accessed")
        .or_insert(last_output.fields_accessed as u64);
    counters
        .entry("values_decoded")
        .or_insert(last_output.values_decoded as u64);
    counters.entry("callback_count").or_insert(
        group_stats
            .map(|stats| stats.callback_count)
            .unwrap_or(reader_stats.visitor_calls) as u64,
    );
    counters
        .entry("batch_count")
        .or_insert(reader_stats.visitor_calls as u64);
    counters
        .entry("group_count")
        .or_insert(group_stats.map(|stats| stats.group_count).unwrap_or(0) as u64);
    counters
        .entry("rows_materialized")
        .or_insert(last_output.rows_materialized as u64);
    counters
        .entry("values_materialized")
        .or_insert(last_output.values_materialized as u64);
    counters
        .entry("aura_value_materialized_count")
        .or_insert(last_output.values_materialized as u64);
    counters
        .entry("row_vec_alloc_count")
        .or_insert(last_output.rows_materialized as u64);
    counters.entry("column_vec_alloc_count").or_insert(
        if last_output.values_materialized > 0 && last_output.rows_materialized == 0 {
            fixture.field_count as u64
        } else {
            0
        },
    );
    counters
        .entry("temp_row_buffer_count")
        .or_insert(reader_stats.temp_row_buffers_allocated as u64);
    counters
        .entry("field_load_count")
        .or_insert(last_output.values_decoded as u64);
    counters
        .entry("endian_decode_count")
        .or_insert(last_output.values_decoded as u64);
    counters
        .entry("checksum_ops")
        .or_insert(last_output.values_decoded as u64);
    counters
        .entry("bounds_check_count")
        .or_insert(last_output.bounds_check_count as u64);
    counters
        .entry("dynamic_dispatch_count")
        .or_insert(last_output.dynamic_dispatch_count as u64);
    counters
        .entry("type_kernel_group_count")
        .or_insert(last_output.kernel_group_count as u64);
    counters
        .entry("bytes_read_at_open")
        .or_insert(reader_stats.bytes_read_at_open as u64);
    counters
        .entry("body_bytes_read_at_open")
        .or_insert(reader_stats.body_bytes_read_at_open as u64);
    counters
        .entry("bytes_read_total")
        .or_insert(reader_stats.source_bytes_read_total as u64);
    counters
        .entry("full_file_bytes_copied")
        .or_insert(reader_stats.full_file_bytes_copied as u64);
    let allocations_proxy = last_output
        .allocations_proxy_override
        .unwrap_or(source_stats.allocations_proxy);
    counters
        .entry("bytes_copied")
        .or_insert(source_stats.bytes_copied as u64);
    counters
        .entry("allocations_proxy")
        .or_insert(allocations_proxy as u64);
    counters
        .entry("allocation_count_proxy")
        .or_insert(last_output.allocation_count_proxy as u64);
    counters
        .entry("bytes_allocated_proxy")
        .or_insert(last_output.bytes_allocated_proxy as u64);
    counters
        .entry("zero_size_removes")
        .or_insert(last_output.zero_size_removes as u64);
    let breakdown = last_output.apply_breakdown.unwrap_or_default();
    let setup_ms = nanos_to_ms(breakdown.setup_ns);
    let plan_build_ms = nanos_to_ms(breakdown.plan_build_ns);
    let allocation_ms = nanos_to_ms(breakdown.allocation_ns);
    let reset_ms = nanos_to_ms(breakdown.reset_ns);
    let apply_update_ms = nanos_to_ms(breakdown.update_remove_ns);
    let state_hash_ms = nanos_to_ms(breakdown.state_hash_ns);
    let production_total_ms = extract_ms + reset_ms + apply_update_ms;
    let verify_total_ms = production_total_ms + state_hash_ms;
    let production_records_per_sec = if production_total_ms > 0.0 {
        records as f64 / (production_total_ms / 1000.0)
    } else {
        records_per_sec
    };
    let apply_records_per_sec = if apply_update_ms > 0.0 {
        records as f64 / (apply_update_ms / 1000.0)
    } else {
        apply_recs_per_sec
    };
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    Ok(json!({
        "operation": operation,
        "dataset": fixture.name,
        "dataset_kind": fixture.name,
        "schema_hash": fixture.schema_hash,
        "schema_name": fixture.schema_name,
        "schema_id": fixture.schema_hash,
        "field_count": fixture.field_count,
        "field_names": fixture.field_names,
        "field_physical_types": fixture.field_physical_types,
        "record_width": fixture.record_width,
        "record_count": fixture.record_count,
        "records": records,
        "format": format_for_operation(operation),
        "profile": profile_for_operation(operation),
        "benchmark_class": benchmark_class_for_operation(operation, last_output.benchmark_class),
        "replay_mode": replay_mode_for_operation(operation, last_output.replay_mode),
        "batch_size": args.batch_size,
        "median_ms": duration_ms(median),
        "p95_ms": duration_ms(p95),
        "records_per_sec": records_per_sec,
        "recs_per_sec": records_per_sec,
        "apply_recs_per_sec": apply_recs_per_sec,
        "input_bytes": input_bytes_for_operation(operation, fixture),
        "output_bytes": output_bytes,
        "compressed_bytes": compressed_bytes_for_operation(operation, fixture),
        "output_mb_sec": output_mb_sec,
        "mb_per_sec": output_mb_sec,
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
        "source_kind": source_stats.source_kind,
        "buffer_reuse_enabled": source_stats.buffer_reuse_enabled,
        "bytes_copied": source_stats.bytes_copied,
        "bytes_borrowed_or_ranged": source_stats.bytes_borrowed_or_ranged,
        "allocations_proxy": allocations_proxy,
        "book_levels": last_output.book_levels,
        "instruments": last_output.instrument_count,
        "price_levels": last_output.price_level_count,
        "adds": last_output.book_adds,
        "modifies": last_output.book_modifies,
        "deletes": last_output.book_deletes,
        "zero_size_removes": last_output.zero_size_removes,
        "state_hash": last_output.state_hash,
        "state_hash_checked": last_output.state_hash_checked,
        "engine_kind": last_output.engine_kind.unwrap_or("none"),
        "fused_enabled": last_output.fused_enabled,
        "apply_mode": last_output
            .apply_mode
            .map(OrderBookApplyMode::as_str)
            .unwrap_or("none"),
        "lifecycle_mode": last_output
            .lifecycle_mode
            .map(OrderBookLifecycleMode::as_str)
            .unwrap_or("none"),
        "engine_reused": last_output.engine_reused,
        "buffers_reused": last_output.buffers_reused,
        "allocation_count_proxy": last_output.allocation_count_proxy,
        "bytes_allocated_proxy": last_output.bytes_allocated_proxy,
        "cache_shape": last_output.cache_shape.unwrap_or("none"),
        "setup_ms": setup_ms,
        "plan_build_ms": plan_build_ms,
        "allocation_ms": allocation_ms,
        "reset_ms": reset_ms,
        "apply_update_ms": apply_update_ms,
        "state_hash_ms": state_hash_ms,
        "production_total_ms": production_total_ms,
        "verify_total_ms": verify_total_ms,
        "production_records_per_sec": production_records_per_sec,
        "apply_records_per_sec": apply_records_per_sec,
        "instrument_lookup_ns": breakdown.instrument_lookup_ns,
        "side_lookup_ns": breakdown.side_lookup_ns,
        "price_lookup_ns": breakdown.price_lookup_ns,
        "update_remove_ns": breakdown.update_remove_ns,
        "allocation_ns": breakdown.allocation_ns,
        "state_hash_ns": breakdown.state_hash_ns,
        "loop_overhead_ns": breakdown.loop_overhead_ns,
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
        "stage_times_ms": last_output.stages.times_ms,
        "extract_ms": extract_ms,
        "apply_ms": apply_ms,
        "total_ms": runtime_ms,
        "counters": counters,
        "timer_tree_kind": last_output.stages.timer_tree_kind,
        "stage_sum_ms": stage_sum_ms,
        "runtime_ms": runtime_ms,
        "unexplained_ms": unexplained_ms,
        "unexplained_pct": unexplained_pct,
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

#[allow(clippy::too_many_arguments)]
fn run_operation(
    operation: &str,
    schema: &AuraSchema,
    source_batch: &AuraRecordBatch,
    aura0: &[u8],
    aura1: &[u8],
    aura1_path: &Path,
    aura1_zst: &[u8],
    args: &Args,
    preextracted_orderbook: Option<&ExtractedOrderBookDeltas>,
    prepared_orderbook: Option<&mut PreparedOrderBookBenchmark>,
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
        "aura1-replay-per-row-noop" => replay_aura1_per_row_noop_path(aura1_path),
        "aura1-replay-per-row-touch-selected" => replay_aura1_per_row_selected_path(aura1_path),
        "aura1-replay-per-row-touch-all" => {
            replay_aura1_row_view_path(aura1_path, TouchMode::AllFields)
                .map(|output| output.with_replay_mode("per_row"))
        }
        "aura1-replay-batch-noop" => replay_aura1_batches_path(aura1_path, args.batch_size)
            .map(|output| output.with_replay_mode("batch")),
        "aura1-replay-batch-touch-selected" => {
            replay_aura1_batch_default_selected_path(aura1_path, args.batch_size)
        }
        "aura1-replay-batch-touch-all" => replay_aura1_batch_all_fields_mode_path(
            aura1_path,
            args.batch_size,
            AllFieldMode::TypeKernel,
        )
        .map(|output| output.with_replay_mode("batch")),
        "aura1-replay-orderbook-deltas-batch" => {
            replay_orderbook_deltas_batch_path(aura1_path, args.batch_size)
        }
        "aura1-replay-orderbook-deltas-apply-batch" => {
            replay_orderbook_deltas_apply_batch_path(aura1_path, args.batch_size)
        }
        "aura1-event-source-file-orderbook-apply" => {
            replay_event_source_file_orderbook_apply(aura1_path, args.batch_size)
        }
        "aura1-event-source-memory-orderbook-apply" => {
            replay_event_source_memory_orderbook_apply(aura1, args.batch_size)
        }
        "aura1-event-source-live-orderbook-apply" => {
            replay_event_source_live_orderbook_apply(aura1, args.batch_size)
        }
        "aura1-event-source-live-frame-orderbook-apply" => {
            replay_event_source_live_frame_orderbook_apply(aura1, args.batch_size)
        }
        operation if fused_orderbook_benchmark_spec(operation).is_some() => {
            let spec = fused_orderbook_benchmark_spec(operation).unwrap();
            let prepared =
                prepared_orderbook.ok_or_else(|| anyhow::anyhow!("missing prepared orderbook"))?;
            replay_orderbook_fused_benchmark_spec(spec, aura1, args.batch_size, prepared)
        }
        operation if orderbook_benchmark_spec(operation).is_some() => {
            let spec = orderbook_benchmark_spec(operation).unwrap();
            replay_orderbook_benchmark_spec(
                spec,
                aura1_path,
                args.batch_size,
                preextracted_orderbook,
                prepared_orderbook,
            )
        }
        "aura1-orderbook-extract-only" => {
            replay_orderbook_extract_only_engine_path(aura1_path, args.batch_size)
        }
        "aura1-orderbook-extract-plus-apply-current" | "aura1-book-apply-current" => {
            replay_orderbook_apply_engine_path(
                aura1_path,
                args.batch_size,
                OrderBookEngineKind::Current,
            )
        }
        "aura1-orderbook-extract-plus-apply-dense-ladder" | "aura1-book-apply-dense-ladder" => {
            replay_orderbook_apply_engine_path(
                aura1_path,
                args.batch_size,
                OrderBookEngineKind::DenseLadder,
            )
        }
        "aura1-orderbook-extract-plus-apply-paged-ladder" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::PagedLadder { page_size: 64 },
        ),
        "aura1-orderbook-extract-plus-apply-direct-index" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::DirectIndex,
        ),
        "aura1-orderbook-extract-plus-apply-run-locality" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::RunLocality,
        ),
        "aura1-orderbook-extract-plus-apply-optimized" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::Optimized,
        ),
        "aura1-book-apply-packed-key" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::PackedKey,
        ),
        "aura1-book-apply-btree" => replay_orderbook_apply_engine_path(
            aura1_path,
            args.batch_size,
            OrderBookEngineKind::BTreeMap,
        ),
        operation if orderbook_apply_only_kind(operation).is_some() => {
            let extracted = preextracted_orderbook
                .ok_or_else(|| anyhow::anyhow!("missing pre-extracted orderbook deltas"))?;
            apply_orderbook_preextracted(extracted, orderbook_apply_only_kind(operation).unwrap())
        }
        "aura1-book-apply-per-instrument" => replay_orderbook_apply_variant_path(
            aura1_path,
            args.batch_size,
            BookApplyVariant::PerInstrument,
        ),
        "aura1-book-apply-side-split" => replay_orderbook_apply_variant_path(
            aura1_path,
            args.batch_size,
            BookApplyVariant::SideSplit,
        ),
        "aura1-replay-grouped-touch-selected" => grouped_replay_selected(aura1, schema),
        "aura1-replay-grouped-touch-all" => grouped_replay_truth(
            aura1,
            schema,
            GroupMode::Primary,
            GroupTruthMode::AllFieldsPerRow,
        )
        .map(|output| output.with_replay_mode("grouped")),
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let mut reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(0);
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut records = 0usize;
    let mut checksum = 0u64;
    let loop_start = Instant::now();
    while let Some(batch) = reader.next_batch(batch_size)? {
        let batch_rows = batch.row_count();
        records = records.saturating_add(batch.row_count());
        let checksum_start = Instant::now();
        checksum = checksum_record_batch(checksum, &batch)?;
        stages.add_duration("checksum_mix_ms", checksum_start.elapsed());
        stages.add_counter("last_batch_rows", batch_rows);
    }
    let loop_total = loop_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "aura_value_materialize_ms",
        loop_total,
        &["checksum_mix_ms"],
    );
    stages.add_ms("row_batch_alloc_ms", 0.0);
    stages.add_ms("row_vec_alloc_ms", 0.0);
    stages.add_ms("field_decode_ms", 0.0);
    stages.add_ms("row_push_ms", 0.0);
    Ok(BenchOutput::new(stats.file_len, records)
        .with_row_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(stats)
        .with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let mut reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_touched = reader
        .compiled_plan()
        .map(|plan| plan.aura1_body_size)
        .unwrap_or(0);
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut records = 0usize;
    let mut checksum = 0u64;
    let loop_start = Instant::now();
    while let Some(batch) = reader.next_column_batch(batch_size)? {
        records = records.saturating_add(batch.row_count());
        let checksum_start = Instant::now();
        checksum = checksum_column_batch(checksum, &batch)?;
        stages.add_duration("checksum_mix_ms", checksum_start.elapsed());
    }
    let loop_total = loop_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "field_major_decode_ms",
        loop_total,
        &["checksum_mix_ms"],
    );
    stages.add_ms("column_alloc_ms", 0.0);
    stages.add_ms("typed_vec_alloc_ms", 0.0);
    stages.add_ms("column_push_or_write_ms", 0.0);
    Ok(BenchOutput::new(stats.file_len, records)
        .with_column_materialization(field_count)
        .with_access(
            field_count,
            records.saturating_mul(field_count),
            bytes_touched,
            checksum,
        )
        .with_reader_stats(stats)
        .with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_fixed_batches(batch_size, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        checksum = checksum.wrapping_add(batch.row_count() as u64);
        stages.add_duration("batch_callback_ms", callback_start.elapsed());
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &["batch_callback_ms"],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("batch_advance_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(0, 0, 0, checksum).with_stages(stages))
}

fn replay_aura1_per_row_noop_path(path: &Path) -> Result<BenchOutput> {
    replay_aura1_row_view_only_path(path).map(|output| output.with_replay_mode("per_row"))
}

fn replay_aura1_per_row_selected_path(path: &Path) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let selected = default_selected_field_indices(&reader)?;
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_selected_row_views(&selected, |row| {
        record_count = record_count.saturating_add(1);
        for selected_index in 0..row.field_count() {
            checksum = mix_checksum(checksum, row.get_i64(selected_index)?);
        }
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    stages.add_duration("field_load_ms", replay_total);
    stages.add_ms("row_range_validation_ms", 0.0);
    stages.add_ms("row_slice_setup_ms", 0.0);
    stages.add_ms("row_view_construction_ms", 0.0);
    stages.add_ms("field_offset_calc_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_ms("user_callback_ms", 0.0);
    stages.add_ms("loop_overhead_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output
        .with_access(
            selected.len(),
            record_count.saturating_mul(selected.len()),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_replay_mode("per_row")
        .with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_fixed_batches(batch_size, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let field_loop_start = Instant::now();
        checksum = match mode {
            TouchMode::OneField => mix_checksum(checksum, batch.checksum_field(0)? as i64),
            TouchMode::AllFields => mix_checksum(checksum, batch.checksum_all_fields()? as i64),
        };
        let field_loop_elapsed = field_loop_start.elapsed();
        match mode {
            TouchMode::OneField => {
                stages.add_duration("selected_field_loop_ms", field_loop_elapsed)
            }
            TouchMode::AllFields => stages.add_duration("all_field_loop_ms", field_loop_elapsed),
        }
        let callback_residual =
            duration_ms(callback_start.elapsed()) - duration_ms(field_loop_elapsed);
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &[
            "selected_field_loop_ms",
            "all_field_loop_ms",
            "batch_callback_ms",
        ],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output
        .with_access(
            fields_accessed,
            record_count.saturating_mul(fields_accessed),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let bytes_per_row = bytes_per_row_for_touch(&reader, TouchMode::AllFields)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_fixed_batches(batch_size, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let dispatch_start = Instant::now();
        let dispatch_elapsed = dispatch_start.elapsed();
        stages.add_duration("type_kernel_dispatch_ms", dispatch_elapsed);
        let field_loop_start = Instant::now();
        checksum = checksum.wrapping_add(checksum_batch_all_fields(&batch, mode)?);
        let field_loop_elapsed = field_loop_start.elapsed();
        stages.add_duration("all_field_loop_ms", field_loop_elapsed);
        let measured = duration_ms(dispatch_elapsed) + duration_ms(field_loop_elapsed);
        let callback_residual = duration_ms(callback_start.elapsed()) - measured;
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &[
            "type_kernel_dispatch_ms",
            "all_field_loop_ms",
            "batch_callback_ms",
        ],
    );
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_ms("batch_view_construction_ms", 0.0);
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
        )
        .with_stages(stages))
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
        .with_parse_counters(
            selected_kernel_group_count(selected.len()),
            selected.len(),
            selected_kernel_group_count(selected.len()),
            true,
        ))
}

fn replay_aura1_batch_selected_path(
    path: &Path,
    batch_size: usize,
    requested_fields: usize,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let selected = selected_field_indices(reader.schema().field_count(), requested_fields);
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_fixed_batches(batch_size, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let field_loop_start = Instant::now();
        checksum = checksum.wrapping_add(batch.checksum_selected_fields(&selected)?);
        let field_loop_elapsed = field_loop_start.elapsed();
        stages.add_duration("selected_field_loop_ms", field_loop_elapsed);
        let callback_residual =
            duration_ms(callback_start.elapsed()) - duration_ms(field_loop_elapsed);
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &["selected_field_loop_ms", "batch_callback_ms"],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
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
        .with_parse_counters(
            selected_kernel_group_count(selected.len()),
            selected.len(),
            selected_kernel_group_count(selected.len()),
            true,
        )
        .with_stages(stages))
}

fn replay_aura1_batch_default_selected_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let selected = default_selected_field_indices(&reader)?;
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_fixed_batches(batch_size, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let field_loop_start = Instant::now();
        checksum = checksum.wrapping_add(batch.checksum_selected_fields(&selected)?);
        let field_loop_elapsed = field_loop_start.elapsed();
        stages.add_duration("selected_field_loop_ms", field_loop_elapsed);
        let callback_residual =
            duration_ms(callback_start.elapsed()) - duration_ms(field_loop_elapsed);
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &["selected_field_loop_ms", "batch_callback_ms"],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
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
        .with_parse_counters(
            selected_kernel_group_count(selected.len()),
            selected.len(),
            selected_kernel_group_count(selected.len()),
            true,
        )
        .with_replay_mode("batch")
        .with_stages(stages))
}

fn replay_orderbook_deltas_batch_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(reader.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row(&reader, &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_orderbook_deltas(batch_size, &spec, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let field_loop_start = Instant::now();
        checksum = checksum.wrapping_add(batch.checksum_payload()?);
        let field_loop_elapsed = field_loop_start.elapsed();
        stages.add_duration("orderbook_delta_loop_ms", field_loop_elapsed);
        let callback_residual =
            duration_ms(callback_start.elapsed()) - duration_ms(field_loop_elapsed);
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &["orderbook_delta_loop_ms", "batch_callback_ms"],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = output
        .reader_stats
        .map(|stats| stats.bytes_read_during_replay)
        .unwrap_or_default();
    Ok(output
        .with_access(
            payload_fields.len(),
            record_count.saturating_mul(payload_fields.len()),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(
            selected_kernel_group_count(payload_fields.len()),
            payload_fields.len(),
            selected_kernel_group_count(payload_fields.len()),
            true,
        )
        .with_replay_mode("batch")
        .with_stages(stages))
}

fn replay_orderbook_deltas_apply_batch_path(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(reader.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row(&reader, &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let mut record_count = 0usize;
    let record_capacity = reader
        .compiled_plan()
        .map(|plan| plan.record_count)
        .unwrap_or(0);
    let mut book = BenchOrderBook::with_capacity(record_capacity.saturating_mul(2));
    let replay_start = Instant::now();
    reader.replay_orderbook_deltas(batch_size, &spec, |batch| {
        let callback_start = Instant::now();
        record_count = record_count.saturating_add(batch.row_count());
        let decode_start = Instant::now();
        for row in 0..batch.row_count() {
            let timestamp = batch.timestamp(row)?;
            let instrument = batch.instrument(row)?;
            let side = batch.side(row)?;
            let price = batch.price(row)?;
            let size = batch.size(row)?;
            let flags = batch.flags(row)?.unwrap_or(0);
            let action = batch.action(row)?.unwrap_or(0);
            let sequence = batch.sequence(row)?.unwrap_or(0);
            let order_id = batch.order_id(row)?.unwrap_or(0);
            book.apply(BenchBookDelta {
                timestamp,
                instrument,
                side,
                price,
                size,
                flags,
                action,
                sequence,
                order_id,
            });
        }
        let decode_elapsed = decode_start.elapsed();
        stages.add_duration("orderbook_decode_apply_loop_ms", decode_elapsed);
        let callback_residual = duration_ms(callback_start.elapsed()) - duration_ms(decode_elapsed);
        stages.add_ms("batch_callback_ms", callback_residual.max(0.0));
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    let checksum = book.checksum;
    black_box(checksum);
    black_box(book.level_count);
    let stats = reader.stats();
    add_residual_ms(
        &mut stages,
        "batch_range_validation_ms",
        replay_total,
        &["orderbook_decode_apply_loop_ms", "batch_callback_ms"],
    );
    stages.add_ms("batch_view_construction_ms", 0.0);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_counter("book_update_count", book.updates);
    stages.add_counter("book_delete_count", book.deletes);
    stages.add_counter("book_level_count", book.level_count);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = output
        .reader_stats
        .map(|stats| stats.bytes_read_during_replay)
        .unwrap_or_default();
    Ok(output
        .with_access(
            payload_fields.len(),
            record_count.saturating_mul(payload_fields.len()),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_parse_counters(
            selected_kernel_group_count(payload_fields.len()),
            payload_fields.len(),
            selected_kernel_group_count(payload_fields.len()),
            true,
        )
        .with_replay_mode("batch")
        .with_stages(stages))
}

fn replay_orderbook_extract_only_engine_path(
    path: &Path,
    batch_size: usize,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let extract_start = Instant::now();
    let extracted = extract_orderbook_deltas_for_engine(path, batch_size)?;
    stages.add_duration("orderbook_extract_ms", extract_start.elapsed());
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    let mut output = BenchOutput::new(extracted.reader_stats.file_len, extracted.deltas.len())
        .with_reader_stats(extracted.reader_stats);
    output.bytes_scanned = extracted.bytes_scanned;
    Ok(output
        .with_access(
            extracted.payload_field_count,
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.payload_field_count),
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.bytes_per_row),
            0,
        )
        .with_parse_counters(
            selected_kernel_group_count(extracted.payload_field_count),
            extracted.payload_field_count,
            selected_kernel_group_count(extracted.payload_field_count),
            true,
        )
        .with_replay_mode("batch")
        .with_stages(stages))
}

fn replay_orderbook_apply_engine_path(
    path: &Path,
    batch_size: usize,
    kind: OrderBookEngineKind,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let extract_start = Instant::now();
    let extracted = extract_orderbook_deltas_for_engine(path, batch_size)?;
    stages.add_duration("orderbook_extract_ms", extract_start.elapsed());

    let apply_start = Instant::now();
    let stats = apply_orderbook_engine(&extracted.deltas, kind)?;
    stages.add_duration("orderbook_apply_ms", apply_start.elapsed());
    add_orderbook_stats_counters(&mut stages, &stats);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);

    let mut output = BenchOutput::new(extracted.reader_stats.file_len, extracted.deltas.len())
        .with_reader_stats(extracted.reader_stats);
    output.bytes_scanned = extracted.bytes_scanned;
    Ok(output
        .with_access(
            extracted.payload_field_count,
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.payload_field_count),
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.bytes_per_row),
            stats.state_hash.0,
        )
        .with_parse_counters(
            selected_kernel_group_count(extracted.payload_field_count),
            extracted.payload_field_count,
            selected_kernel_group_count(extracted.payload_field_count),
            true,
        )
        .with_replay_mode("batch")
        .with_book_stats(stats)
        .with_stages(stages))
}

fn apply_orderbook_preextracted(
    extracted: &ExtractedOrderBookDeltas,
    kind: OrderBookEngineKind,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    stages.add_ms("orderbook_extract_ms", 0.0);
    let apply_start = Instant::now();
    let stats = apply_orderbook_engine(&extracted.deltas, kind)?;
    stages.add_duration("orderbook_apply_ms", apply_start.elapsed());
    add_orderbook_stats_counters(&mut stages, &stats);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);

    let mut output = BenchOutput::new(extracted.reader_stats.file_len, extracted.deltas.len())
        .with_reader_stats(extracted.reader_stats);
    output.bytes_scanned = 0;
    Ok(output
        .with_access(0, 0, 0, stats.state_hash.0)
        .with_parse_counters(0, 0, 0, true)
        .with_replay_mode("apply_only")
        .with_book_stats(stats)
        .with_stages(stages))
}

fn replay_orderbook_benchmark_spec(
    spec: OrderBookBenchmarkSpec,
    path: &Path,
    batch_size: usize,
    preextracted: Option<&ExtractedOrderBookDeltas>,
    prepared: Option<&mut PreparedOrderBookBenchmark>,
) -> Result<BenchOutput> {
    match spec.lifecycle_mode {
        OrderBookLifecycleMode::Cold => {
            let mut stages = StageBreakdown::summed();
            let extracted;
            let extracted = if spec.include_extract {
                let extract_start = Instant::now();
                extracted = extract_orderbook_deltas_for_engine(path, batch_size)?;
                stages.add_duration("orderbook_extract_ms", extract_start.elapsed());
                &extracted
            } else {
                stages.add_ms("orderbook_extract_ms", 0.0);
                preextracted.ok_or_else(|| anyhow::anyhow!("missing pre-extracted deltas"))?
            };
            let expected_hash = prepared.as_ref().map(|prepared| prepared.expected_hash);
            let apply_start = Instant::now();
            let stats = apply_orderbook_engine_mode(
                &extracted.deltas,
                spec.kind,
                spec.apply_mode,
                OrderBookLifecycleMode::Cold,
                expected_hash,
            )?;
            stages.add_duration("orderbook_apply_ms", apply_start.elapsed());
            orderbook_bench_output(extracted, stats, stages)
        }
        OrderBookLifecycleMode::Prepared => {
            let prepared =
                prepared.ok_or_else(|| anyhow::anyhow!("missing prepared orderbook session"))?;
            let mut stages = StageBreakdown::summed();
            let extracted;
            let extracted = if spec.include_extract {
                let extract_start = Instant::now();
                extracted = extract_orderbook_deltas_for_engine(path, batch_size)?;
                stages.add_duration("orderbook_extract_ms", extract_start.elapsed());
                &extracted
            } else {
                stages.add_ms("orderbook_extract_ms", 0.0);
                &prepared.extracted
            };
            let expected_hash = matches!(spec.apply_mode, OrderBookApplyMode::Verify)
                .then_some(prepared.expected_hash);
            let apply_start = Instant::now();
            let mut stats =
                prepared
                    .session
                    .replay(&extracted.deltas, spec.apply_mode, expected_hash)?;
            stages.add_duration("orderbook_apply_ms", apply_start.elapsed());
            stats.breakdown.setup_ns = prepared.setup_ns;
            stats.breakdown.plan_build_ns = prepared.plan_build_ns;
            stats.breakdown.allocation_ns = prepared.allocation_ns;
            orderbook_bench_output(extracted, stats, stages)
        }
    }
}

fn replay_orderbook_fused_benchmark_spec(
    spec: OrderBookBenchmarkSpec,
    aura1: &[u8],
    batch_size: usize,
    prepared: &mut PreparedOrderBookBenchmark,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let reader_setup_start = Instant::now();
    let reader = AuraReader::open(Cursor::new(aura1))?;
    let delta_spec = default_orderbook_delta_spec(reader.schema())?;
    let payload_fields = delta_spec.field_indices();
    let bytes_per_row = selected_bytes_per_row(&reader, &payload_fields)?;
    let reader_setup_ns = reader_setup_start.elapsed().as_nanos();
    let reset_start = Instant::now();
    prepared.session.engine_mut().reset_for_replay();
    let reset_ns = reset_start.elapsed().as_nanos();
    let expected_hash =
        matches!(spec.apply_mode, OrderBookApplyMode::Verify).then_some(prepared.expected_hash);
    let replay_start = Instant::now();
    let fused_stats = reader.replay_orderbook_deltas_fused(
        batch_size,
        &delta_spec,
        prepared.session.engine_mut(),
    )?;
    let replay_ns = replay_start.elapsed().as_nanos();
    let extract_ns =
        reader_setup_ns.saturating_add(replay_ns.saturating_sub(fused_stats.apply_update_ns));
    stages.add_ms("orderbook_extract_ms", nanos_to_ms(extract_ns));
    stages.add_ms(
        "orderbook_apply_ms",
        nanos_to_ms(fused_stats.apply_update_ns),
    );
    stages.add_ms(
        "fused_decode_update_loop_ms",
        nanos_to_ms(fused_stats.apply_update_ns),
    );
    stages.add_ms(
        "fused_replay_residual_ms",
        nanos_to_ms(replay_ns.saturating_sub(fused_stats.apply_update_ns)),
    );
    stages.add_counter("selected_fields", fused_stats.selected_fields);
    stages.add_counter("values_decoded", fused_stats.values_decoded);
    stages.add_counter("batches", fused_stats.batches);
    stages.add_counter("callback_count", 0);
    stages.add_counter("batch_size", batch_size);
    stages.add_counter(
        "delta_batch_structs_created",
        fused_stats.delta_batch_structs_created,
    );
    stages.add_counter("temporary_buffer_bytes", fused_stats.temporary_buffer_bytes);
    stages.add_counter("field_load_count", fused_stats.field_load_count);
    stages.add_counter("timestamp_load_count", 0);
    stages.add_counter("instrument_load_count", fused_stats.records);
    stages.add_counter("side_load_count", fused_stats.records);
    stages.add_counter("price_load_count", fused_stats.records);
    stages.add_counter("size_load_count", fused_stats.records);
    stages.add_counter(
        "action_load_count",
        fused_stats
            .values_decoded
            .saturating_sub(fused_stats.records.saturating_mul(4)),
    );
    stages.add_counter("flags_load_count", 0);
    stages.add_counter("sequence_load_count", 0);
    stages.add_counter("selected_field_kernel_dispatch_count", 0);
    stages.add_counter("engine_apply_calls", fused_stats.engine_apply_calls);
    stages.add_counter("rows_materialized", fused_stats.rows_materialized);
    stages.add_counter("values_materialized", fused_stats.values_materialized);
    stages.add_ms("timestamp_load_ms", 0.0);
    stages.add_ms("instrument_side_price_size_action_load_ms", 0.0);
    stages.add_ms("selected_field_kernel_dispatch_ms", 0.0);
    stages.add_ms("orderbook_delta_batch_construction_ms", 0.0);
    stages.add_ms("batch_callback_handoff_ms", 0.0);
    stages.add_ms("checksum_state_update_ms", 0.0);

    let mut stats = prepared.session.engine_mut().finish_profiled(
        spec.apply_mode,
        expected_hash,
        fused_stats.apply_update_ns,
        reset_ns,
    )?;
    stats.breakdown.setup_ns = prepared.setup_ns;
    stats.breakdown.plan_build_ns = prepared.plan_build_ns;
    stats.breakdown.allocation_ns = prepared.allocation_ns;
    add_orderbook_stats_counters(&mut stages, &stats);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);

    let reader_stats = reader.stats();
    let mut output = BenchOutput::new(reader_stats.file_len, fused_stats.records)
        .with_reader_stats(reader_stats);
    output.bytes_scanned = reader_stats.bytes_read_during_replay;
    Ok(output
        .with_access(
            payload_fields.len(),
            fused_stats.values_decoded,
            fused_stats.records.saturating_mul(bytes_per_row),
            stats.state_hash.0,
        )
        .with_parse_counters(0, 0, 1, true)
        .with_replay_mode("fused")
        .with_book_stats(stats)
        .with_fused_enabled()
        .with_stages(stages))
}

fn orderbook_bench_output(
    extracted: &ExtractedOrderBookDeltas,
    stats: BookApplyStats,
    mut stages: StageBreakdown,
) -> Result<BenchOutput> {
    add_orderbook_stats_counters(&mut stages, &stats);
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    let mut output = BenchOutput::new(extracted.reader_stats.file_len, extracted.deltas.len())
        .with_reader_stats(extracted.reader_stats);
    output.bytes_scanned = extracted.bytes_scanned;
    Ok(output
        .with_access(
            extracted.payload_field_count,
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.payload_field_count),
            extracted
                .deltas
                .len()
                .saturating_mul(extracted.bytes_per_row),
            stats.state_hash.0,
        )
        .with_parse_counters(
            selected_kernel_group_count(extracted.payload_field_count),
            extracted.payload_field_count,
            selected_kernel_group_count(extracted.payload_field_count),
            true,
        )
        .with_replay_mode("batch")
        .with_book_stats(stats)
        .with_stages(stages))
}

fn prepare_orderbook_benchmark(
    path: &Path,
    batch_size: usize,
    kind: OrderBookEngineKind,
) -> Result<PreparedOrderBookBenchmark> {
    let extracted = extract_orderbook_deltas_for_engine(path, batch_size)?;
    let plan_start = Instant::now();
    let plan = PreparedOrderBookApplyPlan::build(&extracted.deltas, kind)?;
    let plan_build_ns = plan_start.elapsed().as_nanos();
    let allocation_start = Instant::now();
    let engine = PreparedOrderBookEngine::with_capacity(&plan)?;
    let allocation_ns = allocation_start.elapsed().as_nanos();
    let session = OrderBookReplaySession::from_prepared(plan, engine);
    let expected_hash = apply_orderbook_engine_mode(
        &extracted.deltas,
        kind,
        OrderBookApplyMode::Verify,
        OrderBookLifecycleMode::Cold,
        None,
    )?
    .state_hash;
    Ok(PreparedOrderBookBenchmark {
        session,
        extracted,
        expected_hash,
        setup_ns: plan_build_ns.saturating_add(allocation_ns),
        plan_build_ns,
        allocation_ns,
    })
}

fn extract_orderbook_deltas_for_engine(
    path: &Path,
    batch_size: usize,
) -> Result<ExtractedOrderBookDeltas> {
    let reader = AuraReader::open_path(path)?;
    let spec = default_orderbook_delta_spec(reader.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row(&reader, &payload_fields)?;
    let record_capacity = reader
        .compiled_plan()
        .map(|plan| plan.record_count)
        .unwrap_or(0);
    let mut deltas = Vec::with_capacity(record_capacity);
    reader.replay_orderbook_deltas(batch_size, &spec, |batch| {
        for row in 0..batch.row_count() {
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
    })?;
    let stats = reader.stats();
    Ok(ExtractedOrderBookDeltas {
        deltas,
        payload_field_count: payload_fields.len(),
        bytes_per_row,
        bytes_scanned: stats.bytes_read_during_replay,
        reader_stats: stats,
    })
}

fn apply_orderbook_engine(
    deltas: &[OrderBookDelta],
    kind: OrderBookEngineKind,
) -> Result<BookApplyStats> {
    apply_orderbook_engine_mode(
        deltas,
        kind,
        OrderBookApplyMode::Verify,
        OrderBookLifecycleMode::Cold,
        None,
    )
}

fn apply_orderbook_engine_mode(
    deltas: &[OrderBookDelta],
    kind: OrderBookEngineKind,
    mode: OrderBookApplyMode,
    lifecycle: OrderBookLifecycleMode,
    expected_hash: Option<BookStateHash>,
) -> Result<BookApplyStats> {
    let setup_start = Instant::now();
    let plan_start = Instant::now();
    let plan = OrderBookApplyPlan::compile(deltas, kind)?;
    let plan_build_ns = plan_start.elapsed().as_nanos();
    let allocation_start = Instant::now();
    let mut engine = OrderBookEngine::with_plan(plan)?;
    let allocation_ns = allocation_start.elapsed().as_nanos();
    let setup_ns = setup_start.elapsed().as_nanos();
    let mut stats = match mode {
        OrderBookApplyMode::Production => {
            engine.apply_all_production_profiled(deltas, lifecycle, false, false)?
        }
        OrderBookApplyMode::Verify => {
            engine.apply_all_verify_profiled(deltas, lifecycle, false, false, expected_hash)?
        }
    };
    stats.breakdown.setup_ns = setup_ns;
    stats.breakdown.plan_build_ns = plan_build_ns;
    stats.breakdown.allocation_ns = allocation_ns;
    Ok(stats)
}

fn add_orderbook_stats_counters(stages: &mut StageBreakdown, stats: &BookApplyStats) {
    stages.add_counter(
        "book_update_count",
        stats.adds.saturating_add(stats.modifies),
    );
    stages.add_counter("book_add_count", stats.adds);
    stages.add_counter("book_modify_count", stats.modifies);
    stages.add_counter("book_delete_count", stats.deletes);
    stages.add_counter("book_zero_size_remove_count", stats.zero_size_removes);
    stages.add_counter("book_level_count", stats.book_levels);
    stages.add_counter("instrument_count", stats.instruments);
    stages.add_counter("price_level_count", stats.price_levels);
    stages.add_counter("allocation_count_proxy", stats.allocation_count_proxy);
    stages.add_counter("bytes_allocated_proxy", stats.bytes_allocated_proxy);
    black_box(stats.state_hash.0);
}

fn orderbook_apply_only_kind(operation: &str) -> Option<OrderBookEngineKind> {
    match operation {
        "orderbook-apply-only-current" => Some(OrderBookEngineKind::Current),
        "orderbook-apply-only-dense-ladder" => Some(OrderBookEngineKind::DenseLadder),
        "orderbook-apply-only-paged-ladder" | "orderbook-apply-only-paged-ladder-64" => {
            Some(OrderBookEngineKind::PagedLadder { page_size: 64 })
        }
        "orderbook-apply-only-paged-ladder-32" => {
            Some(OrderBookEngineKind::PagedLadder { page_size: 32 })
        }
        "orderbook-apply-only-paged-ladder-128" => {
            Some(OrderBookEngineKind::PagedLadder { page_size: 128 })
        }
        "orderbook-apply-only-paged-ladder-256" => {
            Some(OrderBookEngineKind::PagedLadder { page_size: 256 })
        }
        "orderbook-apply-only-paged-ladder-512" => {
            Some(OrderBookEngineKind::PagedLadder { page_size: 512 })
        }
        "orderbook-apply-only-packed-key" => Some(OrderBookEngineKind::PackedKey),
        "orderbook-apply-only-direct-index" => Some(OrderBookEngineKind::DirectIndex),
        "orderbook-apply-only-run-locality" => Some(OrderBookEngineKind::RunLocality),
        "orderbook-apply-only-optimized" => Some(OrderBookEngineKind::Optimized),
        "orderbook-apply-only-btree" => Some(OrderBookEngineKind::BTreeMap),
        _ => None,
    }
}

fn orderbook_benchmark_spec(operation: &str) -> Option<OrderBookBenchmarkSpec> {
    let (include_extract, rest) =
        if let Some(rest) = operation.strip_prefix("aura1-orderbook-extract-plus-apply-") {
            (true, rest)
        } else {
            let rest = operation.strip_prefix("orderbook-apply-only-")?;
            (false, rest)
        };

    let (engine, mode, lifecycle) = rest
        .strip_suffix("-production-prepared")
        .map(|engine| {
            (
                engine,
                OrderBookApplyMode::Production,
                OrderBookLifecycleMode::Prepared,
            )
        })
        .or_else(|| {
            rest.strip_suffix("-verify-prepared").map(|engine| {
                (
                    engine,
                    OrderBookApplyMode::Verify,
                    OrderBookLifecycleMode::Prepared,
                )
            })
        })
        .or_else(|| {
            rest.strip_suffix("-production-cold").map(|engine| {
                (
                    engine,
                    OrderBookApplyMode::Production,
                    OrderBookLifecycleMode::Cold,
                )
            })
        })
        .or_else(|| {
            rest.strip_suffix("-verify-cold").map(|engine| {
                (
                    engine,
                    OrderBookApplyMode::Verify,
                    OrderBookLifecycleMode::Cold,
                )
            })
        })?;
    let kind = match engine {
        "optimized" => OrderBookEngineKind::Optimized,
        "direct-index" => OrderBookEngineKind::DirectIndex,
        "dense-ladder" => OrderBookEngineKind::DenseLadder,
        "run-locality" => OrderBookEngineKind::RunLocality,
        "paged-ladder" => OrderBookEngineKind::PagedLadder { page_size: 64 },
        "current" => OrderBookEngineKind::Current,
        "packed-key" => OrderBookEngineKind::PackedKey,
        "btree" => OrderBookEngineKind::BTreeMap,
        _ => return None,
    };
    Some(OrderBookBenchmarkSpec {
        kind,
        apply_mode: mode,
        lifecycle_mode: lifecycle,
        include_extract,
    })
}

fn fused_orderbook_benchmark_spec(operation: &str) -> Option<OrderBookBenchmarkSpec> {
    let rest = operation.strip_prefix("aura1-orderbook-fused-extract-plus-apply-")?;
    let (engine, mode, lifecycle) = rest
        .strip_suffix("-production-prepared")
        .map(|engine| {
            (
                engine,
                OrderBookApplyMode::Production,
                OrderBookLifecycleMode::Prepared,
            )
        })
        .or_else(|| {
            rest.strip_suffix("-verify-prepared").map(|engine| {
                (
                    engine,
                    OrderBookApplyMode::Verify,
                    OrderBookLifecycleMode::Prepared,
                )
            })
        })?;
    let kind = match engine {
        "optimized" => OrderBookEngineKind::Optimized,
        "direct-index" => OrderBookEngineKind::DirectIndex,
        "dense-ladder" => OrderBookEngineKind::DenseLadder,
        "run-locality" => OrderBookEngineKind::RunLocality,
        "paged-ladder" => OrderBookEngineKind::PagedLadder { page_size: 64 },
        "current" => OrderBookEngineKind::Current,
        "packed-key" => OrderBookEngineKind::PackedKey,
        "btree" => OrderBookEngineKind::BTreeMap,
        _ => return None,
    };
    Some(OrderBookBenchmarkSpec {
        kind,
        apply_mode: mode,
        lifecycle_mode: lifecycle,
        include_extract: true,
    })
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum BookApplyVariant {
    Current,
    PackedKey,
    PerInstrument,
    SideSplit,
    DenseLadder,
    BTree,
}

#[derive(Debug, Clone, Copy, Default)]
struct BookApplyMetrics {
    book_levels: usize,
    instruments: usize,
    price_levels: usize,
    adds: usize,
    modifies: usize,
    deletes: usize,
    state_hash: u64,
    allocations_proxy: usize,
}

#[derive(Debug, Default)]
struct BookApplyCounts {
    instruments: HashSet<i64>,
    prices: HashSet<i64>,
    adds: usize,
    modifies: usize,
    deletes: usize,
}

impl BookApplyCounts {
    fn observe(&mut self, delta: BenchBookDelta) {
        self.instruments.insert(delta.instrument);
        self.prices.insert(delta.price);
    }

    fn add(&mut self) {
        self.adds = self.adds.saturating_add(1);
    }

    fn modify(&mut self) {
        self.modifies = self.modifies.saturating_add(1);
    }

    fn delete(&mut self) {
        self.deletes = self.deletes.saturating_add(1);
    }

    fn finish(
        self,
        book_levels: usize,
        state_hash: u64,
        allocations_proxy: usize,
    ) -> BookApplyMetrics {
        BookApplyMetrics {
            book_levels,
            instruments: self.instruments.len(),
            price_levels: self.prices.len(),
            adds: self.adds,
            modifies: self.modifies,
            deletes: self.deletes,
            state_hash,
            allocations_proxy,
        }
    }
}

fn replay_orderbook_apply_variant_path(
    path: &Path,
    batch_size: usize,
    variant: BookApplyVariant,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(reader.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row(&reader, &payload_fields)?;
    let record_capacity = reader
        .compiled_plan()
        .map(|plan| plan.record_count)
        .unwrap_or(0);
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());

    let mut deltas = Vec::with_capacity(record_capacity);
    let extract_start = Instant::now();
    reader.replay_orderbook_deltas(batch_size, &spec, |batch| {
        for row in 0..batch.row_count() {
            deltas.push(BenchBookDelta {
                timestamp: batch.timestamp(row)?,
                instrument: batch.instrument(row)?,
                side: batch.side(row)?,
                price: batch.price(row)?,
                size: batch.size(row)?,
                flags: batch.flags(row)?.unwrap_or(0),
                action: batch.action(row)?.unwrap_or(0),
                sequence: batch.sequence(row)?.unwrap_or(0),
                order_id: batch.order_id(row)?.unwrap_or(0),
            });
        }
        Ok(())
    })?;
    stages.add_duration("orderbook_extract_ms", extract_start.elapsed());

    let apply_start = Instant::now();
    let metrics = apply_book_variant(&deltas, variant)?;
    stages.add_duration("orderbook_apply_ms", apply_start.elapsed());
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_counter(
        "book_update_count",
        metrics.adds.saturating_add(metrics.modifies),
    );
    stages.add_counter("book_add_count", metrics.adds);
    stages.add_counter("book_modify_count", metrics.modifies);
    stages.add_counter("book_delete_count", metrics.deletes);
    stages.add_counter("book_level_count", metrics.book_levels);
    stages.add_counter("instrument_count", metrics.instruments);
    stages.add_counter("price_level_count", metrics.price_levels);
    black_box(metrics.state_hash);

    let stats = reader.stats();
    let mut output = BenchOutput::new(stats.file_len, deltas.len()).with_reader_stats(stats);
    output.bytes_scanned = output
        .reader_stats
        .map(|stats| stats.bytes_read_during_replay)
        .unwrap_or_default();
    Ok(output
        .with_access(
            payload_fields.len(),
            deltas.len().saturating_mul(payload_fields.len()),
            deltas.len().saturating_mul(bytes_per_row),
            metrics.state_hash,
        )
        .with_parse_counters(
            selected_kernel_group_count(payload_fields.len()),
            payload_fields.len(),
            selected_kernel_group_count(payload_fields.len()),
            true,
        )
        .with_book_metrics(metrics)
        .with_stages(stages))
}

fn apply_book_variant(
    deltas: &[BenchBookDelta],
    variant: BookApplyVariant,
) -> Result<BookApplyMetrics> {
    match variant {
        BookApplyVariant::Current => Ok(apply_current_book(deltas)),
        BookApplyVariant::PackedKey => apply_packed_key_book(deltas),
        BookApplyVariant::PerInstrument => Ok(apply_per_instrument_book(deltas)),
        BookApplyVariant::SideSplit => Ok(apply_side_split_book(deltas)),
        BookApplyVariant::DenseLadder => Ok(apply_dense_ladder_book(deltas)),
        BookApplyVariant::BTree => Ok(apply_btree_book(deltas)),
    }
}

fn apply_current_book(deltas: &[BenchBookDelta]) -> BookApplyMetrics {
    let mut book = BenchOrderBook::with_capacity(deltas.len().saturating_mul(2));
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        book.apply(delta);
    }
    counts.adds = book.adds;
    counts.modifies = book.modifies;
    counts.deletes = book.deletes;
    let state_hash = hash_book_entries(book.active_entries());
    counts.finish(book.level_count, state_hash, book.slots.len())
}

fn apply_packed_key_book(deltas: &[BenchBookDelta]) -> Result<BookApplyMetrics> {
    let mut levels: HashMap<u128, i64> = HashMap::with_capacity(deltas.len().saturating_mul(2));
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        let key = BenchBookKey {
            instrument: delta.instrument,
            side: delta.side,
            price: delta.price,
        };
        let packed = pack_book_key(key)?;
        if is_delete_delta(delta) {
            counts.delete();
            levels.remove(&packed);
        } else if let std::collections::hash_map::Entry::Occupied(mut entry) = levels.entry(packed)
        {
            counts.modify();
            entry.insert(delta.size);
        } else {
            counts.add();
            levels.insert(packed, delta.size);
        }
    }
    let entries = levels
        .iter()
        .map(|(packed, size)| unpack_book_key(*packed).map(|key| (key, *size)))
        .collect::<Result<Vec<_>>>()?;
    let state_hash = hash_book_entries(entries);
    Ok(counts.finish(levels.len(), state_hash, levels.capacity()))
}

fn apply_per_instrument_book(deltas: &[BenchBookDelta]) -> BookApplyMetrics {
    let mut levels: HashMap<i64, HashMap<(i64, i64), i64>> =
        HashMap::with_capacity(deltas.len().min(1024));
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        let side_price = (delta.side, delta.price);
        if is_delete_delta(delta) {
            counts.delete();
            if let Some(book) = levels.get_mut(&delta.instrument) {
                book.remove(&side_price);
            }
        } else {
            let book = levels.entry(delta.instrument).or_default();
            if let std::collections::hash_map::Entry::Occupied(mut entry) = book.entry(side_price) {
                counts.modify();
                entry.insert(delta.size);
            } else {
                counts.add();
                book.insert(side_price, delta.size);
            }
        }
    }
    let entries = levels
        .iter()
        .flat_map(|(instrument, book)| {
            book.iter().map(move |((side, price), size)| {
                (
                    BenchBookKey {
                        instrument: *instrument,
                        side: *side,
                        price: *price,
                    },
                    *size,
                )
            })
        })
        .collect::<Vec<_>>();
    let book_levels = entries.len();
    let allocations_proxy = levels
        .values()
        .map(HashMap::capacity)
        .sum::<usize>()
        .saturating_add(levels.capacity());
    counts.finish(book_levels, hash_book_entries(entries), allocations_proxy)
}

fn apply_side_split_book(deltas: &[BenchBookDelta]) -> BookApplyMetrics {
    let mut levels: HashMap<(i64, i64), HashMap<i64, i64>> =
        HashMap::with_capacity(deltas.len().min(2048));
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        let book_key = (delta.instrument, delta.side);
        if is_delete_delta(delta) {
            counts.delete();
            if let Some(book) = levels.get_mut(&book_key) {
                book.remove(&delta.price);
            }
        } else {
            let book = levels.entry(book_key).or_default();
            if let std::collections::hash_map::Entry::Occupied(mut entry) = book.entry(delta.price)
            {
                counts.modify();
                entry.insert(delta.size);
            } else {
                counts.add();
                book.insert(delta.price, delta.size);
            }
        }
    }
    let entries = levels
        .iter()
        .flat_map(|((instrument, side), book)| {
            book.iter().map(move |(price, size)| {
                (
                    BenchBookKey {
                        instrument: *instrument,
                        side: *side,
                        price: *price,
                    },
                    *size,
                )
            })
        })
        .collect::<Vec<_>>();
    let book_levels = entries.len();
    let allocations_proxy = levels
        .values()
        .map(HashMap::capacity)
        .sum::<usize>()
        .saturating_add(levels.capacity());
    counts.finish(book_levels, hash_book_entries(entries), allocations_proxy)
}

#[derive(Debug)]
struct DenseBookLadder {
    min_price: i64,
    levels: Vec<i64>,
    active: usize,
}

fn apply_dense_ladder_book(deltas: &[BenchBookDelta]) -> BookApplyMetrics {
    let ranges = dense_ladder_ranges(deltas);
    let total_slots = ranges.values().fold(0usize, |acc, (min_price, max_price)| {
        let width = max_price
            .checked_sub(*min_price)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(usize::MAX);
        acc.saturating_add(width)
    });
    let max_slots = deltas.len().saturating_mul(64).max(1_000_000);
    if total_slots > max_slots {
        return apply_packed_key_book(deltas).unwrap_or_else(|_| apply_per_instrument_book(deltas));
    }

    let mut ladders = ranges
        .into_iter()
        .map(|(key, (min_price, max_price))| {
            let width = usize::try_from(max_price - min_price + 1).unwrap_or(0);
            (
                key,
                DenseBookLadder {
                    min_price,
                    levels: vec![0; width],
                    active: 0,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        let key = (delta.instrument, delta.side);
        if is_delete_delta(delta) {
            counts.delete();
            if let Some(ladder) = ladders.get_mut(&key) {
                if let Some(slot) = dense_ladder_slot_mut(ladder, delta.price) {
                    if *slot != 0 {
                        *slot = 0;
                        ladder.active = ladder.active.saturating_sub(1);
                    }
                }
            }
        } else if let Some(ladder) = ladders.get_mut(&key) {
            let mut inserted = false;
            let mut modified = false;
            if let Some(slot) = dense_ladder_slot_mut(ladder, delta.price) {
                inserted = *slot == 0;
                modified = !inserted;
                *slot = delta.size;
            }
            if inserted {
                counts.add();
                ladder.active = ladder.active.saturating_add(1);
            } else if modified {
                counts.modify();
            }
        }
    }
    let entries = ladders
        .iter()
        .flat_map(|((instrument, side), ladder)| {
            ladder
                .levels
                .iter()
                .enumerate()
                .filter(|(_, size)| **size != 0)
                .map(move |(offset, size)| {
                    (
                        BenchBookKey {
                            instrument: *instrument,
                            side: *side,
                            price: ladder.min_price + offset as i64,
                        },
                        *size,
                    )
                })
        })
        .collect::<Vec<_>>();
    let book_levels = entries.len();
    counts.finish(book_levels, hash_book_entries(entries), total_slots)
}

fn dense_ladder_ranges(deltas: &[BenchBookDelta]) -> HashMap<(i64, i64), (i64, i64)> {
    let mut ranges = HashMap::new();
    for delta in deltas
        .iter()
        .copied()
        .filter(|delta| !is_delete_delta(*delta))
    {
        ranges
            .entry((delta.instrument, delta.side))
            .and_modify(|range: &mut (i64, i64)| {
                range.0 = range.0.min(delta.price);
                range.1 = range.1.max(delta.price);
            })
            .or_insert((delta.price, delta.price));
    }
    ranges
}

fn dense_ladder_slot_mut(ladder: &mut DenseBookLadder, price: i64) -> Option<&mut i64> {
    let offset = price.checked_sub(ladder.min_price)?;
    ladder.levels.get_mut(usize::try_from(offset).ok()?)
}

fn apply_btree_book(deltas: &[BenchBookDelta]) -> BookApplyMetrics {
    let mut levels: BTreeMap<BenchBookKey, i64> = BTreeMap::new();
    let mut counts = BookApplyCounts::default();
    for &delta in deltas {
        counts.observe(delta);
        let key = BenchBookKey {
            instrument: delta.instrument,
            side: delta.side,
            price: delta.price,
        };
        if is_delete_delta(delta) {
            counts.delete();
            levels.remove(&key);
        } else if let std::collections::btree_map::Entry::Occupied(mut entry) = levels.entry(key) {
            counts.modify();
            entry.insert(delta.size);
        } else {
            counts.add();
            levels.insert(key, delta.size);
        }
    }
    let entries = levels
        .iter()
        .map(|(key, size)| (*key, *size))
        .collect::<Vec<_>>();
    counts.finish(levels.len(), hash_book_entries(entries), levels.len())
}

fn is_delete_delta(delta: BenchBookDelta) -> bool {
    delta.size <= 0 || delta.action == 2
}

fn pack_book_key(key: BenchBookKey) -> Result<u128> {
    if !fits_signed_bits(key.instrument, 48) || !fits_signed_bits(key.side, 16) {
        bail!("book key outside packed-key benchmark range");
    }
    let instrument = (i128::from(key.instrument) & ((1i128 << 48) - 1)) as u128;
    let side = (i128::from(key.side) & ((1i128 << 16) - 1)) as u128;
    let price = key.price as u64 as u128;
    Ok((instrument << 80) | (side << 64) | price)
}

fn unpack_book_key(packed: u128) -> Result<BenchBookKey> {
    let instrument = sign_extend_i64(packed >> 80, 48)?;
    let side = sign_extend_i64((packed >> 64) & 0xffff, 16)?;
    let price = packed as u64 as i64;
    Ok(BenchBookKey {
        instrument,
        side,
        price,
    })
}

fn fits_signed_bits(value: i64, bits: u32) -> bool {
    let min = -(1i128 << (bits - 1));
    let max = (1i128 << (bits - 1)) - 1;
    let value = i128::from(value);
    value >= min && value <= max
}

fn sign_extend_i64(value: u128, bits: u32) -> Result<i64> {
    let shift = 128 - bits;
    let signed = ((value << shift) as i128) >> shift;
    i64::try_from(signed).map_err(|_| anyhow::anyhow!("packed book key sign extension"))
}

fn hash_book_entries(entries: impl IntoIterator<Item = (BenchBookKey, i64)>) -> u64 {
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(key, _)| *key);
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (key, size) in entries {
        hash = mix_checksum(hash, key.instrument);
        hash = mix_checksum(hash, key.side);
        hash = mix_checksum(hash, key.price);
        hash = mix_checksum(hash, size);
    }
    hash = hash.wrapping_add(0x9e37_79b9_7f4a_7c15);
    hash
}

#[derive(Debug)]
struct EventSourceOrderBookStats {
    record_count: usize,
    batch_count: usize,
    last_batch_rows: usize,
    checksum: u64,
}

fn replay_event_source_file_orderbook_apply(path: &Path, batch_size: usize) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let mut source = AuraFileSource::open_path(path, batch_size)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(source.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row_from_plan(source.compiled_plan(), &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let replay = replay_event_source_orderbook_apply(&mut source, &spec, &mut stages)?;
    let source_stats = source.source_stats();
    let reader = source.into_reader();
    let stats = reader.stats();
    event_source_orderbook_output(
        stats,
        replay,
        payload_fields.len(),
        bytes_per_row,
        None,
        source_stats,
        stages,
    )
}

fn replay_event_source_memory_orderbook_apply(
    aura1: &[u8],
    batch_size: usize,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let mut source = AuraMemorySource::try_new(aura1.to_vec(), batch_size)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(source.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row_from_plan(source.compiled_plan(), &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let replay = replay_event_source_orderbook_apply(&mut source, &spec, &mut stages)?;
    let source_stats = source.source_stats();
    let reader = source.into_reader();
    let stats = reader.stats();
    event_source_orderbook_output(
        stats,
        replay,
        payload_fields.len(),
        bytes_per_row,
        None,
        source_stats,
        stages,
    )
}

fn replay_event_source_live_orderbook_apply(
    aura1: &[u8],
    batch_size: usize,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open(Cursor::new(aura1))?;
    let schema = reader.schema().clone();
    let plan = reader
        .compiled_plan()
        .ok_or_else(|| anyhow::anyhow!("compiled plan"))?
        .clone();
    let body = aura1_body_slice(aura1)?;
    let body_len = body.len();
    let mut source =
        AuraLiveSource::with_plan(Cursor::new(body.to_vec()), schema, plan.clone(), batch_size)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(source.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row_from_plan(source.compiled_plan(), &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let replay = replay_event_source_orderbook_apply(&mut source, &spec, &mut stages)?;
    let source_stats = source.source_stats();
    let stats = live_event_source_stats(
        reader.stats(),
        &plan,
        body_len,
        replay.batch_count,
        replay.last_batch_rows,
        replay.record_count.saturating_mul(payload_fields.len()),
    );
    event_source_orderbook_output(
        stats,
        replay,
        payload_fields.len(),
        bytes_per_row,
        Some("live_stream"),
        source_stats,
        stages,
    )
}

fn replay_event_source_live_frame_orderbook_apply(
    aura1: &[u8],
    batch_size: usize,
) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open(Cursor::new(aura1))?;
    let schema = reader.schema().clone();
    let plan = reader
        .compiled_plan()
        .ok_or_else(|| anyhow::anyhow!("compiled plan"))?
        .clone();
    let body = aura1_body_slice(aura1)?;
    let body_len = body.len();
    let mut source =
        AuraLiveFrameSource::from_body_chunks(body.to_vec(), schema, plan.clone(), batch_size)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let spec = default_orderbook_delta_spec(source.schema())?;
    let payload_fields = spec.field_indices();
    let bytes_per_row = selected_bytes_per_row_from_plan(source.compiled_plan(), &payload_fields)?;
    stages.add_duration("selected_field_recipe_lookup_ms", recipe_start.elapsed());
    let replay = replay_event_source_orderbook_apply(&mut source, &spec, &mut stages)?;
    let source_stats = source.source_stats();
    let stats = live_event_source_stats(
        reader.stats(),
        &plan,
        body_len,
        replay.batch_count,
        replay.last_batch_rows,
        replay.record_count.saturating_mul(payload_fields.len()),
    );
    event_source_orderbook_output(
        stats,
        replay,
        payload_fields.len(),
        bytes_per_row,
        Some("live_frame"),
        source_stats,
        stages,
    )
}

fn replay_event_source_orderbook_apply<S>(
    source: &mut S,
    spec: &OrderBookDeltaSpec,
    stages: &mut StageBreakdown,
) -> Result<EventSourceOrderBookStats>
where
    S: AuraEventSource,
    for<'a> S::Batch<'a>: AuraEventBatch,
{
    let payload_fields = spec.field_indices();
    let flags_index = payload_fields.get(5).copied();
    let record_capacity = source.compiled_plan().record_count;
    let mut book = BenchOrderBook::with_capacity(record_capacity.saturating_mul(2));
    let mut record_count = 0usize;
    let mut batch_count = 0usize;
    let mut last_batch_rows = 0usize;
    let replay_start = Instant::now();
    loop {
        let next_start = Instant::now();
        let Some(batch) = source.next_batch()? else {
            stages.add_duration("event_source_next_batch_ms", next_start.elapsed());
            break;
        };
        stages.add_duration("event_source_next_batch_ms", next_start.elapsed());
        batch_count = batch_count.saturating_add(1);
        last_batch_rows = batch.row_count();
        record_count = record_count.saturating_add(batch.row_count());
        let extract_start = Instant::now();
        let mut deltas = Vec::with_capacity(batch.row_count());
        for row in 0..batch.row_count() {
            let timestamp = batch.value_i64(row, spec.timestamp_index())?;
            let instrument = batch.value_i64(row, spec.instrument_index())?;
            let side = batch.value_i64(row, spec.side_index())?;
            let price = batch.value_i64(row, spec.price_index())?;
            let size = batch.value_i64(row, spec.size_index())?;
            let flags = flags_index
                .map(|index| batch.value_i64(row, index))
                .transpose()?
                .unwrap_or(0);
            deltas.push(BenchBookDelta {
                timestamp,
                instrument,
                side,
                price,
                size,
                flags,
                action: 0,
                sequence: 0,
                order_id: 0,
            });
        }
        stages.add_duration("event_source_extract_ms", extract_start.elapsed());
        let apply_start = Instant::now();
        for delta in deltas {
            book.apply(delta);
        }
        let apply_elapsed = apply_start.elapsed();
        stages.add_duration("event_source_apply_ms", apply_elapsed);
        stages.add_duration("event_source_orderbook_apply_loop_ms", apply_elapsed);
    }
    let replay_elapsed = replay_start.elapsed();
    add_residual_ms(
        stages,
        "event_source_batch_overhead_ms",
        replay_elapsed,
        &[
            "event_source_next_batch_ms",
            "event_source_extract_ms",
            "event_source_apply_ms",
        ],
    );
    stages.add_ms("field_load_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_counter("book_update_count", book.updates);
    stages.add_counter("book_delete_count", book.deletes);
    stages.add_counter("book_level_count", book.level_count);
    let checksum = book.checksum;
    black_box(checksum);
    black_box(book.level_count);
    Ok(EventSourceOrderBookStats {
        record_count,
        batch_count,
        last_batch_rows,
        checksum,
    })
}

fn event_source_orderbook_output(
    stats: AuraReaderStats,
    replay: EventSourceOrderBookStats,
    payload_field_count: usize,
    bytes_per_row: usize,
    source_kind_override: Option<&'static str>,
    source_stats: AuraEventSourceStats,
    stages: StageBreakdown,
) -> Result<BenchOutput> {
    let mut output = BenchOutput::new(stats.file_len, replay.record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    let output = output
        .with_access(
            payload_field_count,
            replay.record_count.saturating_mul(payload_field_count),
            replay.record_count.saturating_mul(bytes_per_row),
            replay.checksum,
        )
        .with_parse_counters(
            selected_kernel_group_count(payload_field_count),
            payload_field_count,
            selected_kernel_group_count(payload_field_count),
            true,
        )
        .with_replay_mode("event_source")
        .with_source_stats(source_stats)
        .with_stages(stages);
    let output = if let Some(source_kind) = source_kind_override {
        output.with_source_kind(source_kind)
    } else {
        output
    };
    Ok(output)
}

fn live_event_source_stats(
    mut stats: AuraReaderStats,
    plan: &CompiledAuraPlan,
    body_len: usize,
    batch_count: usize,
    last_batch_rows: usize,
    values_decoded: usize,
) -> AuraReaderStats {
    stats.source_kind = AuraReaderSourceKind::Memory;
    stats.replay_backend = AuraReplayBackend::Memory;
    stats.file_len = body_len;
    stats.open_decoded_row_count = 0;
    stats.full_file_materialized = false;
    stats.batches_read = batch_count;
    stats.rows_decoded_in_last_batch = last_batch_rows;
    stats.max_rows_materialized_at_once = 0;
    stats.bytes_read_at_open = 0;
    stats.body_bytes_read_at_open = 0;
    stats.footer_bytes_read_at_open = 0;
    stats.bytes_read_during_replay = body_len;
    stats.bytes_read_in_last_batch = last_batch_rows.saturating_mul(plan.aura1_record_width);
    stats.full_file_bytes_copied = 0;
    stats.row_width_from_plan = plan.aura1_record_width;
    stats.body_offset_from_header = 0;
    stats.footer_offset_from_trailer = body_len;
    stats.record_count_from_footer = plan.record_count;
    stats.rows_scanned = plan.record_count;
    stats.temp_row_buffers_allocated = 0;
    stats.visitor_calls = batch_count;
    stats.field_decode_count = values_decoded;
    stats.endian_load_count = values_decoded;
    stats.compiled_plan_used = true;
    stats.source_bytes_read_at_open = 0;
    stats.source_bytes_read_total = body_len;
    stats.streaming_reader_used = true;
    stats
}

fn selected_bytes_per_row_from_plan(
    plan: &CompiledAuraPlan,
    field_indices: &[usize],
) -> Result<usize> {
    let slots = plan.aura1_field_offsets();
    field_indices.iter().try_fold(0usize, |acc, index| {
        let field_index =
            u16::try_from(*index).map_err(|_| anyhow::anyhow!("field index out of bounds"))?;
        let field = slots
            .iter()
            .find(|field| field.field_index == field_index)
            .ok_or_else(|| anyhow::anyhow!("field index out of bounds"))?;
        acc.checked_add(field.width)
            .ok_or_else(|| anyhow::anyhow!("field width overflow"))
    })
}

fn aura1_body_slice(aura1: &[u8]) -> Result<&[u8]> {
    let info = records::aura1_fixed_layout_info(aura1)?;
    aura1
        .get(info.body_offset..info.body_offset.saturating_add(info.body_bytes))
        .ok_or_else(|| anyhow::anyhow!("invalid Aura1 body range"))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BenchBookKey {
    instrument: i64,
    side: i64,
    price: i64,
}

#[derive(Debug, Clone, Copy)]
struct BenchBookDelta {
    timestamp: i64,
    instrument: i64,
    side: i64,
    price: i64,
    size: i64,
    flags: i64,
    action: i64,
    sequence: i64,
    order_id: i64,
}

#[derive(Debug, Clone, Copy, Default)]
struct BenchBookSlot {
    key: BenchBookKey,
    size: i64,
    state: u8,
}

#[derive(Debug)]
struct BenchOrderBook {
    slots: Vec<BenchBookSlot>,
    mask: usize,
    level_count: usize,
    updates: usize,
    adds: usize,
    modifies: usize,
    deletes: usize,
    checksum: u64,
}

impl BenchOrderBook {
    fn with_capacity(capacity: usize) -> Self {
        let table_len = capacity.min(16 * 1024).next_power_of_two().max(1024);
        Self {
            slots: vec![BenchBookSlot::default(); table_len],
            mask: table_len - 1,
            level_count: 0,
            updates: 0,
            adds: 0,
            modifies: 0,
            deletes: 0,
            checksum: 0,
        }
    }

    fn apply(&mut self, delta: BenchBookDelta) {
        let key = BenchBookKey {
            instrument: delta.instrument,
            side: delta.side,
            price: delta.price,
        };
        let previous = if delta.size <= 0 || delta.action == 2 {
            self.deletes = self.deletes.saturating_add(1);
            self.delete(key)
        } else {
            self.updates = self.updates.saturating_add(1);
            let (previous, inserted) = self.upsert_with_status(key, delta.size);
            if inserted {
                self.adds = self.adds.saturating_add(1);
            } else {
                self.modifies = self.modifies.saturating_add(1);
            }
            previous
        };
        self.checksum = mix_checksum(self.checksum, delta.timestamp);
        self.checksum = mix_checksum(self.checksum, delta.instrument);
        self.checksum = mix_checksum(self.checksum, delta.side);
        self.checksum = mix_checksum(self.checksum, delta.price);
        self.checksum = mix_checksum(self.checksum, delta.size);
        self.checksum = mix_checksum(self.checksum, delta.flags);
        self.checksum = mix_checksum(self.checksum, delta.sequence);
        self.checksum = mix_checksum(self.checksum, delta.order_id);
        self.checksum = mix_checksum(self.checksum, previous);
        self.checksum = self.checksum.wrapping_add(self.level_count as u64);
    }

    fn upsert(&mut self, key: BenchBookKey, size: i64) -> i64 {
        self.upsert_with_status(key, size).0
    }

    fn upsert_with_status(&mut self, key: BenchBookKey, size: i64) -> (i64, bool) {
        if self.level_count.saturating_mul(10) >= self.slots.len().saturating_mul(7) {
            self.grow();
        }
        let (index, found) = self.find_slot(key);
        let slot = &mut self.slots[index];
        if found {
            let previous = slot.size;
            slot.size = size;
            (previous, false)
        } else {
            slot.key = key;
            slot.size = size;
            slot.state = 1;
            self.level_count = self.level_count.saturating_add(1);
            (0, true)
        }
    }

    fn grow(&mut self) {
        let old_slots = std::mem::take(&mut self.slots);
        let table_len = (old_slots.len().saturating_mul(2)).max(1024);
        self.slots = vec![BenchBookSlot::default(); table_len];
        self.mask = table_len - 1;
        self.level_count = 0;
        for slot in old_slots {
            if slot.state == 1 {
                self.upsert(slot.key, slot.size);
            }
        }
    }

    fn delete(&mut self, key: BenchBookKey) -> i64 {
        let (index, found) = self.find_slot(key);
        if found {
            let slot = &mut self.slots[index];
            let previous = slot.size;
            slot.size = 0;
            slot.state = 2;
            self.level_count = self.level_count.saturating_sub(1);
            previous
        } else {
            0
        }
    }

    fn find_slot(&self, key: BenchBookKey) -> (usize, bool) {
        let mut index = hash_book_key(key) as usize & self.mask;
        let mut first_tombstone = None;
        loop {
            let slot = self.slots[index];
            match slot.state {
                0 => return (first_tombstone.unwrap_or(index), false),
                1 if slot.key == key => return (index, true),
                2 if first_tombstone.is_none() => first_tombstone = Some(index),
                _ => {}
            }
            index = (index + 1) & self.mask;
        }
    }

    fn active_entries(&self) -> Vec<(BenchBookKey, i64)> {
        self.slots
            .iter()
            .filter(|slot| slot.state == 1)
            .map(|slot| (slot.key, slot.size))
            .collect()
    }
}

fn hash_book_key(key: BenchBookKey) -> u64 {
    let mut hash = (key.instrument as u64).wrapping_mul(0x9E37_79B1_85EB_CA87);
    hash ^= (key.side as u64)
        .rotate_left(17)
        .wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    hash ^= (key.price as u64)
        .rotate_left(31)
        .wrapping_mul(0x1656_67B1_9E37_79F9);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    hash ^ (hash >> 29)
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum.wrapping_add(row.field_count() as u64);
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    stages.add_duration("loop_overhead_ms", replay_total);
    stages.add_ms("row_range_validation_ms", 0.0);
    stages.add_ms("row_slice_setup_ms", 0.0);
    stages.add_ms("row_view_construction_ms", 0.0);
    stages.add_ms("user_callback_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output.with_access(0, 0, 0, checksum).with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    let field_count = reader.schema().field_count();
    let fields_accessed = fields_accessed(field_count, mode);
    let bytes_per_row = bytes_per_row_for_touch(&reader, mode)?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let mut record_count = 0usize;
    let mut checksum = 0u64;
    let replay_start = Instant::now();
    reader.replay_row_views(|row| {
        record_count = record_count.saturating_add(1);
        checksum = checksum_row_view(checksum, row, mode)?;
        Ok(())
    })?;
    let replay_total = replay_start.elapsed();
    black_box(checksum);
    let stats = reader.stats();
    stages.add_duration("field_load_ms", replay_total);
    stages.add_ms("row_range_validation_ms", 0.0);
    stages.add_ms("row_slice_setup_ms", 0.0);
    stages.add_ms("row_view_construction_ms", 0.0);
    stages.add_ms("field_offset_calc_ms", 0.0);
    stages.add_ms("endian_decode_ms", 0.0);
    stages.add_ms("checksum_mix_ms", 0.0);
    stages.add_ms("user_callback_ms", 0.0);
    stages.add_ms("loop_overhead_ms", 0.0);
    let mut output = BenchOutput::new(stats.file_len, record_count).with_reader_stats(stats);
    output.bytes_scanned = stats.bytes_read_during_replay;
    Ok(output
        .with_access(
            fields_accessed,
            record_count.saturating_mul(fields_accessed),
            record_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let reader = AuraReader::open_path(path)?;
    let stats_at_open = reader.stats();
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let range_start = Instant::now();
    let mut file = fs::File::open(path)?;
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(
        stats_at_open.body_offset_from_header as u64,
    ))?;
    let mut remaining = stats_at_open
        .footer_offset_from_trailer
        .saturating_sub(stats_at_open.body_offset_from_header);
    stages.add_duration("raw_range_validation_ms", range_start.elapsed());
    let mut buffer = vec![0u8; 256 * 1024];
    let mut checksum = 0u8;
    let mut bytes_read = 0usize;
    while remaining > 0 {
        let len = remaining.min(buffer.len());
        let read_start = Instant::now();
        file.read_exact(&mut buffer[..len])?;
        stages.add_duration("raw_body_read_or_map_ms", read_start.elapsed());
        let scan_start = Instant::now();
        for chunk in buffer[..len].chunks(stats_at_open.row_width_from_plan.max(1)) {
            checksum ^= chunk.first().copied().unwrap_or(0);
        }
        stages.add_duration("raw_scan_loop_ms", scan_start.elapsed());
        remaining -= len;
        bytes_read = bytes_read.saturating_add(len);
    }
    black_box(checksum);
    stages.add_ms("raw_checksum_ms", 0.0);
    let mut stats = stats_at_open;
    stats.bytes_read_during_replay = bytes_read;
    stats.source_bytes_read_total = stats.source_bytes_read_total.saturating_add(bytes_read);
    let mut output =
        BenchOutput::new(stats.file_len, stats.record_count_from_footer).with_reader_stats(stats);
    output.bytes_scanned = bytes_read;
    Ok(output.with_stages(stages))
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
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
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
    stages.add_duration("open_total_ms", setup_start.elapsed());
    let recipe_start = Instant::now();
    let mut checksum = 0u64;
    let mut values_decoded = 0usize;
    stages.add_duration("group_key_recipe_setup_ms", recipe_start.elapsed());
    let grouped_start = Instant::now();
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
    let grouped_total = grouped_start.elapsed();
    black_box(checksum);
    let reader_stats = reader.stats();
    match truth_mode {
        GroupTruthMode::ViewOnly => stages.add_duration("group_callback_ms", grouped_total),
        GroupTruthMode::KeyOnly => {
            stages.add_duration("group_key_materialization_ms", grouped_total)
        }
        GroupTruthMode::OneFieldPerRow | GroupTruthMode::AllFieldsPerRow => {
            stages.add_duration("grouped_field_touch_ms", grouped_total)
        }
        GroupTruthMode::Aggregate => stages.add_duration("grouped_aggregate_ms", grouped_total),
    }
    stages.add_ms("group_boundary_detection_ms", 0.0);
    stages.add_ms("key_byte_compare_ms", 0.0);
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
        .with_reader_stats(reader_stats)
        .with_stages(stages))
}

fn grouped_replay_selected(bytes: &[u8], schema: &AuraSchema) -> Result<BenchOutput> {
    let mut stages = StageBreakdown::summed();
    let setup_start = Instant::now();
    let fields = group_fields(schema, GroupMode::Primary)?;
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let selected = default_selected_field_indices(&reader)?;
    let bytes_per_row = selected_bytes_per_row(&reader, &selected)?;
    let info = records::aura1_fixed_layout_info(bytes)?;
    let body = bytes
        .get(info.body_offset..info.body_offset.saturating_add(info.body_bytes))
        .ok_or_else(|| anyhow::anyhow!("invalid Aura1 body range"))?;
    let slots = bench_field_slots(&reader)?;
    let selected_slots = selected
        .iter()
        .map(|index| {
            slots
                .get(*index)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("field index out of bounds"))
        })
        .collect::<Result<Vec<_>>>()?;
    stages.add_duration("open_total_ms", setup_start.elapsed());
    stages.add_ms("group_key_recipe_setup_ms", 0.0);
    let mut checksum = 0u64;
    let mut values_decoded = 0usize;
    let grouped_start = Instant::now();
    let stats = reader.grouped_replay(&GroupBy::fields(fields.iter().cloned()), |group| {
        checksum = checksum_group_selected_rows(
            checksum,
            body,
            info.record_width,
            &selected_slots,
            group.row_start(),
            group.row_count(),
        )?;
        values_decoded =
            values_decoded.saturating_add(group.row_count().saturating_mul(selected_slots.len()));
        Ok(())
    })?;
    let grouped_total = grouped_start.elapsed();
    black_box(checksum);
    let reader_stats = reader.stats();
    stages.add_duration("grouped_field_touch_ms", grouped_total);
    stages.add_ms("group_boundary_detection_ms", 0.0);
    stages.add_ms("key_byte_compare_ms", 0.0);
    stages.add_ms("group_key_materialization_ms", 0.0);
    stages.add_ms("group_callback_ms", 0.0);
    let mut output = BenchOutput::new(bytes.len(), stats.row_count).with_group_stats(fields, stats);
    output.bytes_scanned = info.body_bytes;
    Ok(output
        .with_access(
            selected.len(),
            values_decoded,
            stats.row_count.saturating_mul(bytes_per_row),
            checksum,
        )
        .with_reader_stats(reader_stats)
        .with_replay_mode("grouped")
        .with_stages(stages))
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

fn default_selected_field_indices(reader: &AuraReader) -> Result<Vec<usize>> {
    let slots = bench_field_slots(reader)?;
    let mut selected = Vec::new();
    let schema = reader.schema();
    push_first_matching_field(&mut selected, schema, &slots, |aura_type| {
        matches!(
            aura_type,
            aura_codec::AuraType::TimestampNanos | aura_codec::AuraType::TimestampMicros
        )
    });
    push_first_matching_field(&mut selected, schema, &slots, |aura_type| {
        matches!(
            aura_type,
            aura_codec::AuraType::PriceI64Scaled { .. } | aura_codec::AuraType::I64Scaled { .. }
        )
    });
    push_first_matching_field(&mut selected, schema, &slots, |aura_type| {
        matches!(
            aura_type,
            aura_codec::AuraType::U64
                | aura_codec::AuraType::U32
                | aura_codec::AuraType::I64
                | aura_codec::AuraType::I32
                | aura_codec::AuraType::U16
                | aura_codec::AuraType::I16
                | aura_codec::AuraType::U8
                | aura_codec::AuraType::I8
                | aura_codec::AuraType::FlagsU32
                | aura_codec::AuraType::EnumU8
        )
    });
    for field in &slots {
        let index = usize::from(field.field_index);
        if selected.len() >= 3 {
            break;
        }
        if field.width > 0 && !selected.contains(&index) {
            selected.push(index);
        }
    }
    if selected.is_empty() && !slots.is_empty() {
        selected.push(usize::from(slots[0].field_index));
    }
    Ok(selected)
}

fn default_orderbook_delta_spec(schema: &AuraSchema) -> Result<OrderBookDeltaSpec> {
    let mut used = Vec::new();
    let timestamp = find_delta_field(schema, &used, is_timestamp_type, &["ts", "time", "event"])?;
    used.push(timestamp);
    let price = find_delta_field(schema, &used, is_price_type, &["price", "px", "open"])?;
    used.push(price);
    let size = find_delta_field(
        schema,
        &used,
        is_integer_type,
        &["size", "qty", "quantity", "volume"],
    )?;
    used.push(size);
    let side = find_delta_field(
        schema,
        &used,
        is_integer_type,
        &["side", "event_type", "condition", "type"],
    )?;
    used.push(side);
    let instrument = find_delta_field(
        schema,
        &used,
        is_integer_type,
        &["symbol", "instrument", "book", "venue", "id"],
    )?;
    used.push(instrument);
    let flags = find_optional_delta_field(schema, &used, is_integer_type, &["flags"]);

    let field_name = |index: usize| -> Result<String> {
        schema
            .fields()
            .get(index)
            .map(|field| field.name.clone())
            .ok_or_else(|| anyhow::anyhow!("orderbook delta field index out of bounds"))
    };

    let mut builder = OrderBookDeltaSpec::builder()
        .timestamp(field_name(timestamp)?)
        .instrument(field_name(instrument)?)
        .side(field_name(side)?)
        .price(field_name(price)?)
        .size(field_name(size)?);
    if let Some(flags) = flags {
        builder = builder.flags_optional(field_name(flags)?);
    }
    builder
        .build(schema)
        .map_err(|error| anyhow::anyhow!("orderbook delta spec: {error}"))
}

fn find_delta_field(
    schema: &AuraSchema,
    used: &[usize],
    predicate: fn(AuraType) -> bool,
    name_hints: &[&str],
) -> Result<usize> {
    find_named_delta_field(schema, used, predicate, name_hints)
        .or_else(|| find_typed_delta_field(schema, used, predicate))
        .ok_or_else(|| anyhow::anyhow!("orderbook delta schema needs more fixed-width fields"))
}

fn find_optional_delta_field(
    schema: &AuraSchema,
    used: &[usize],
    predicate: fn(AuraType) -> bool,
    name_hints: &[&str],
) -> Option<usize> {
    find_named_delta_field(schema, used, predicate, name_hints)
}

fn find_named_delta_field(
    schema: &AuraSchema,
    used: &[usize],
    predicate: fn(AuraType) -> bool,
    name_hints: &[&str],
) -> Option<usize> {
    schema
        .fields()
        .iter()
        .enumerate()
        .find_map(|(index, field)| {
            let lower = field.name.to_ascii_lowercase();
            if !used.contains(&index)
                && predicate(field.aura_type)
                && name_hints.iter().any(|hint| lower.contains(hint))
            {
                Some(index)
            } else {
                None
            }
        })
}

fn find_typed_delta_field(
    schema: &AuraSchema,
    used: &[usize],
    predicate: fn(AuraType) -> bool,
) -> Option<usize> {
    schema
        .fields()
        .iter()
        .enumerate()
        .find_map(|(index, field)| {
            if !used.contains(&index) && predicate(field.aura_type) {
                Some(index)
            } else {
                None
            }
        })
}

fn is_timestamp_type(aura_type: AuraType) -> bool {
    matches!(
        aura_type,
        AuraType::TimestampNanos | AuraType::TimestampMicros | AuraType::I64
    )
}

fn is_price_type(aura_type: AuraType) -> bool {
    matches!(
        aura_type,
        AuraType::PriceI64Scaled { .. }
            | AuraType::I64Scaled { .. }
            | AuraType::I64
            | AuraType::I32
    )
}

fn is_integer_type(aura_type: AuraType) -> bool {
    matches!(
        aura_type,
        AuraType::Bool
            | AuraType::U8
            | AuraType::U16
            | AuraType::U32
            | AuraType::U64
            | AuraType::I8
            | AuraType::I16
            | AuraType::I32
            | AuraType::I64
            | AuraType::EnumU8
            | AuraType::FlagsU32
    )
}

fn push_first_matching_field(
    selected: &mut Vec<usize>,
    schema: &AuraSchema,
    slots: &[CompiledAuraField],
    predicate: impl Fn(aura_codec::AuraType) -> bool,
) {
    if let Some((index, _field)) = schema.fields().iter().enumerate().find(|(index, field)| {
        predicate(field.aura_type)
            && slots
                .iter()
                .any(|slot| usize::from(slot.field_index) == *index && slot.width > 0)
            && !selected.contains(index)
    }) {
        selected.push(index);
    }
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

fn selected_kernel_group_count(field_count: usize) -> usize {
    field_count.min(4)
}

fn unsafe_loads_for_all_field_mode(mode: AllFieldMode) -> bool {
    matches!(
        mode,
        AllFieldMode::Unchecked | AllFieldMode::TypeKernel | AllFieldMode::InstructionTape
    )
}

fn measured_ms(stages: &StageBreakdown, names: &[&'static str]) -> f64 {
    names
        .iter()
        .filter_map(|name| stages.times_ms.get(name))
        .copied()
        .sum()
}

fn add_residual_ms(
    stages: &mut StageBreakdown,
    name: &'static str,
    total: Duration,
    measured_names: &[&'static str],
) {
    let residual = duration_ms(total) - measured_ms(stages, measured_names);
    stages.add_ms(name, residual.max(0.0));
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

fn checksum_group_selected_rows(
    checksum: u64,
    body: &[u8],
    record_width: usize,
    fields: &[CompiledAuraField],
    row_start: usize,
    row_count: usize,
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
        for field in fields {
            checksum = mix_checksum(checksum, read_field_i64(row, *field)?);
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

fn nanos_to_ms(ns: u128) -> f64 {
    ns as f64 / 1_000_000.0
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

fn replay_mode_for_operation(operation: &str, replay_mode: Option<&'static str>) -> &'static str {
    replay_mode.unwrap_or_else(|| {
        if operation.contains("per-row") {
            "per_row"
        } else if operation.contains("grouped") {
            "grouped"
        } else if operation.contains("batch") && operation.starts_with("aura1-replay-") {
            "batch"
        } else {
            "none"
        }
    })
}

fn benchmark_class_for_operation(operation: &str, benchmark_class: &'static str) -> &'static str {
    if benchmark_class != "other" {
        benchmark_class
    } else if operation.contains("scan-raw") {
        "raw scan"
    } else if operation.contains("view-only") || operation.contains("batch-callback") {
        "view construction"
    } else if operation.contains("read-batches-row") || operation.contains("read-batches-columnar")
    {
        "materialized read"
    } else if operation.contains("batch-touch")
        || operation.contains("batch-selected")
        || operation.contains("row-view")
        || operation.contains("grouped-touch")
        || operation.contains("grouped-aggregate")
    {
        "parse kernel"
    } else {
        benchmark_class
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
        || operation.starts_with("orderbook-")
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
