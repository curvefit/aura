use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aura_codec::{records, writer, Profile};
use records::{DirectAura1TranscodeStats, DirectAura1TranscodeTimings, OutputGuardMode};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    ParseAura1,
    DecodeAura0,
    TranscodeAura1ToAura0,
    TranscodeAura0ToAura1,
}

impl Operation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "parse-aura1" => Ok(Self::ParseAura1),
            "decode-aura0" => Ok(Self::DecodeAura0),
            "transcode-aura1-to-aura0" => Ok(Self::TranscodeAura1ToAura0),
            "transcode-aura0-to-aura1" => Ok(Self::TranscodeAura0ToAura1),
            other => bail!("unknown operation: {other}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ParseAura1 => "parse-aura1",
            Self::DecodeAura0 => "decode-aura0",
            Self::TranscodeAura1ToAura0 => "transcode-aura1-to-aura0",
            Self::TranscodeAura0ToAura1 => "transcode-aura0-to-aura1",
        }
    }

    const fn source_format(self) -> &'static str {
        match self {
            Self::ParseAura1 | Self::TranscodeAura1ToAura0 => "aura1",
            Self::DecodeAura0 | Self::TranscodeAura0ToAura1 => "aura0",
        }
    }

    const fn target_format(self) -> &'static str {
        match self {
            Self::ParseAura1 | Self::DecodeAura0 => "none",
            Self::TranscodeAura1ToAura0 => "aura0",
            Self::TranscodeAura0ToAura1 => "aura1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Json,
    Csv,
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            other => bail!("unknown output format: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheMode {
    Warm,
    Cold,
}

impl CacheMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "warm" => Ok(Self::Warm),
            "cold" => Ok(Self::Cold),
            other => bail!("unknown cache mode: {other}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }

    const fn note(self) -> &'static str {
        match self {
            Self::Warm => "input bytes are preloaded before timed iterations",
            Self::Cold => "input file is reread inside each timed iteration; OS page-cache dropping is not attempted",
        }
    }
}

fn parse_guard_mode(value: &str) -> Result<OutputGuardMode> {
    match value {
        "no_guard" => Ok(OutputGuardMode::NoGuard),
        "fused_output_guard" => Ok(OutputGuardMode::FusedOutputGuard),
        "old_post_output_guard" => Ok(OutputGuardMode::OldPostOutputGuard),
        "block_batched_output_guard" => Ok(OutputGuardMode::BlockBatchedOutputGuard),
        other => bail!("unknown guard mode: {other}"),
    }
}

#[derive(Debug)]
struct Config {
    operation: Operation,
    dataset_name: String,
    input: PathBuf,
    iterations: usize,
    warmups: usize,
    format: OutputFormat,
    output: Option<PathBuf>,
    cache_mode: CacheMode,
    guard_mode: OutputGuardMode,
}

impl Config {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut operation = None;
        let mut dataset_name = None;
        let mut input = None;
        let mut iterations = 7usize;
        let mut warmups = 2usize;
        let mut format = OutputFormat::Json;
        let mut output = None;
        let mut cache_mode = CacheMode::Warm;
        let mut guard_mode = OutputGuardMode::NoGuard;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--operation" => {
                    operation = Some(Operation::parse(
                        &args.next().context("missing --operation value")?,
                    )?);
                }
                "--dataset" => {
                    dataset_name = Some(args.next().context("missing --dataset value")?);
                }
                "--input" => {
                    input = Some(PathBuf::from(args.next().context("missing --input value")?));
                }
                "--iterations" => {
                    iterations = args
                        .next()
                        .context("missing --iterations value")?
                        .parse()
                        .context("invalid --iterations value")?;
                }
                "--warmups" => {
                    warmups = args
                        .next()
                        .context("missing --warmups value")?
                        .parse()
                        .context("invalid --warmups value")?;
                }
                "--format" => {
                    format = OutputFormat::parse(&args.next().context("missing --format value")?)?;
                }
                "--output" => {
                    output = Some(PathBuf::from(
                        args.next().context("missing --output value")?,
                    ));
                }
                "--cache-mode" => {
                    cache_mode =
                        CacheMode::parse(&args.next().context("missing --cache-mode value")?)?;
                }
                "--guard-mode" => {
                    guard_mode =
                        parse_guard_mode(&args.next().context("missing --guard-mode value")?)?;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        if iterations == 0 {
            bail!("--iterations must be greater than zero");
        }

        Ok(Self {
            operation: operation.context("missing --operation")?,
            dataset_name: dataset_name.context("missing --dataset")?,
            input: input.context("missing --input")?,
            iterations,
            warmups,
            format,
            output,
            cache_mode,
            guard_mode,
        })
    }
}

#[derive(Debug, Clone)]
struct RunOutcome {
    record_count: usize,
    output_bytes: usize,
    guard: u64,
    canonical_hash: Option<u64>,
    guard_mode: &'static str,
    output_byte_guard: Option<u64>,
    post_process_duration: Duration,
    timings: Option<DirectAura1TranscodeTimings>,
    stats: Option<DirectAura1TranscodeStats>,
}

#[derive(Debug, Clone)]
struct Measurement {
    operation_duration: Duration,
    total_duration: Duration,
    outcome: RunOutcome,
}

fn main() -> Result<()> {
    let config = Config::parse()?;
    let report = run_benchmark(&config)?;
    match config.output {
        Some(path) => fs::write(path, report)?,
        None => print!("{report}"),
    }
    Ok(())
}

fn run_benchmark(config: &Config) -> Result<String> {
    let input_bytes = fs::read(&config.input)
        .with_context(|| format!("read input {}", config.input.display()))?;
    let input_len = input_bytes.len();
    let dataset_sha256 = sha256_hex(&input_bytes);
    let command_used = std::env::args().collect::<Vec<_>>().join(" ");
    let record_count_hint = inspect_input(config.operation, &input_bytes)?;

    for _ in 0..config.warmups {
        match config.cache_mode {
            CacheMode::Warm => {
                let outcome = run_operation(
                    config.operation,
                    &input_bytes,
                    record_count_hint,
                    config.guard_mode,
                )?;
                black_box(outcome.guard);
            }
            CacheMode::Cold => {
                let bytes = fs::read(&config.input)?;
                let outcome =
                    run_operation(config.operation, &bytes, record_count_hint, config.guard_mode)?;
                black_box(outcome.guard);
            }
        }
    }

    let mut measurements = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let measurement = match config.cache_mode {
            CacheMode::Warm => {
                measure_operation(
                    config.operation,
                    &input_bytes,
                    record_count_hint,
                    config.guard_mode,
                )?
            }
            CacheMode::Cold => {
                let start = Instant::now();
                let bytes = fs::read(&config.input)?;
                let outcome =
                    run_operation(config.operation, &bytes, record_count_hint, config.guard_mode)?;
                let total_duration = start.elapsed();
                let operation_duration =
                    measured_operation_duration(total_duration, outcome.post_process_duration);
                Measurement {
                    operation_duration,
                    total_duration,
                    outcome,
                }
            }
        };
        black_box(measurement.outcome.guard);
        measurements.push(measurement);
    }

    let first = measurements
        .first()
        .context("missing benchmark measurement")?
        .outcome
        .clone();
    let median_ns = percentile_ns(&measurements, 0.50, |measurement| {
        measurement.operation_duration
    });
    let p95_ns = percentile_ns(&measurements, 0.95, |measurement| {
        measurement.operation_duration
    });
    let median_total_ns = percentile_ns(&measurements, 0.50, |measurement| {
        measurement.total_duration
    });
    let p95_total_ns = percentile_ns(&measurements, 0.95, |measurement| {
        measurement.total_duration
    });
    let median_post_process_ns = percentile_ns(&measurements, 0.50, |measurement| {
        measurement.outcome.post_process_duration
    });
    let p95_post_process_ns = percentile_ns(&measurements, 0.95, |measurement| {
        measurement.outcome.post_process_duration
    });
    let runtime_ns = median_ns;
    let representative = representative_measurement(&measurements, runtime_ns);
    let seconds = runtime_ns as f64 / 1_000_000_000.0;
    let records_per_sec = if seconds > 0.0 {
        first.record_count as f64 / seconds
    } else {
        0.0
    };
    let mb_per_sec = if seconds > 0.0 {
        input_len as f64 / (1024.0 * 1024.0) / seconds
    } else {
        0.0
    };
    let output_mb_per_sec = if seconds > 0.0 && first.output_bytes > 0 {
        first.output_bytes as f64 / (1024.0 * 1024.0) / seconds
    } else {
        0.0
    };
    let compression_ratio = if first.output_bytes > 0 && input_len > 0 {
        Some(first.output_bytes as f64 / input_len as f64)
    } else {
        None
    };

    let machine_info = machine_info();
    match config.format {
        OutputFormat::Json => {
            let value = json!({
                "dataset_name": config.dataset_name,
                "dataset_sha256": dataset_sha256,
                "operation": config.operation.as_str(),
                "record_count": first.record_count,
                "input_bytes": input_len,
                "output_bytes": first.output_bytes,
                "runtime_ns": runtime_ns,
                "median_runtime_ns": median_ns,
                "p95_runtime_ns": p95_ns,
                "total_runtime_ns": median_total_ns,
                "median_total_runtime_ns": median_total_ns,
                "p95_total_runtime_ns": p95_total_ns,
                "post_process_runtime_ns": median_post_process_ns,
                "median_post_process_runtime_ns": median_post_process_ns,
                "p95_post_process_runtime_ns": p95_post_process_ns,
                "post_process_note": "post-process time is output guard/checksum work after the measured operation; runtime_ns excludes it",
                "canonical_hash": first.canonical_hash,
                "guard_mode_requested": config.guard_mode.as_str(),
                "guard_mode": first.guard_mode,
                "output_byte_guard": first.output_byte_guard,
                "stage_timings_ns": stage_timings_json(representative.outcome.timings.as_ref()),
                "decode_stats": decode_stats_json(representative.outcome.stats.as_ref()),
                "writer_stats": writer_stats_json(representative.outcome.stats.as_ref()),
                "records_per_sec": records_per_sec,
                "mb_per_sec": mb_per_sec,
                "output_mb_per_sec": output_mb_per_sec,
                "compression_ratio": compression_ratio,
                "source_format": config.operation.source_format(),
                "target_format": config.operation.target_format(),
                "command_used": command_used,
                "git_commit": git_commit(),
                "machine_info": machine_info,
                "iterations": config.iterations,
                "warmups": config.warmups,
                "cache_mode": config.cache_mode.as_str(),
                "cache_note": config.cache_mode.note(),
            });
            Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
        }
        OutputFormat::Csv => Ok(csv_report(
            config,
            &dataset_sha256,
            input_len,
            &first,
            runtime_ns,
            median_ns,
            p95_ns,
            median_total_ns,
            p95_total_ns,
            median_post_process_ns,
            p95_post_process_ns,
            records_per_sec,
            mb_per_sec,
            output_mb_per_sec,
            compression_ratio,
            &command_used,
            &machine_info,
        )),
    }
}

fn measure_operation(
    operation: Operation,
    bytes: &[u8],
    record_count_hint: usize,
    guard_mode: OutputGuardMode,
) -> Result<Measurement> {
    let start = Instant::now();
    let outcome = run_operation(operation, bytes, record_count_hint, guard_mode)?;
    let total_duration = start.elapsed();
    let operation_duration =
        measured_operation_duration(total_duration, outcome.post_process_duration);
    Ok(Measurement {
        operation_duration,
        total_duration,
        outcome,
    })
}

fn measured_operation_duration(total: Duration, post_process: Duration) -> Duration {
    total.checked_sub(post_process).unwrap_or(total)
}

fn duration_from_ns(ns: u128) -> Duration {
    Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
}

fn representative_measurement(measurements: &[Measurement], median_ns: u128) -> &Measurement {
    measurements
        .iter()
        .min_by_key(|measurement| {
            let ns = measurement.operation_duration.as_nanos();
            ns.abs_diff(median_ns)
        })
        .expect("missing benchmark measurement")
}

fn ns_u64(ns: u128) -> u64 {
    u64::try_from(ns).unwrap_or(u64::MAX)
}

fn stage_timings_json(timings: Option<&DirectAura1TranscodeTimings>) -> serde_json::Value {
    let Some(timings) = timings else {
        return json!({
            "decode_input_streams": empty_decode_timing_json(),
            "partitioned_sparse_writer": empty_writer_timing_json(),
        });
    };
    json!({
        "metadata": ns_u64(timings.metadata_ns),
        "allocate_header": ns_u64(timings.allocate_header_ns),
        "trailer": ns_u64(timings.trailer_ns),
        "post_output_guard": ns_u64(timings.post_output_guard_ns),
        "total": ns_u64(timings.total_ns),
        "decode_input_streams": {
            "total": ns_u64(timings.decode_input_streams.total_ns),
            "compressed_input_read": ns_u64(timings.decode_input_streams.compressed_input_read_ns),
            "huffman_entropy_decode": ns_u64(timings.decode_input_streams.huffman_entropy_decode_ns),
            "delta_reconstruction": ns_u64(timings.decode_input_streams.delta_reconstruction_ns),
            "dictionary_symbol_reconstruction": ns_u64(timings.decode_input_streams.dictionary_symbol_reconstruction_ns),
            "validity_presence_bitmap_decode": ns_u64(timings.decode_input_streams.validity_presence_bitmap_decode_ns),
            "record_type_branching": ns_u64(timings.decode_input_streams.record_type_branching_ns),
            "integer_scaling_sign_extension": ns_u64(timings.decode_input_streams.integer_scaling_sign_extension_ns),
            "temporary_buffer_writes": ns_u64(timings.decode_input_streams.temporary_buffer_writes_ns),
            "checksum_hash_work": ns_u64(timings.decode_input_streams.checksum_hash_work_ns),
            "bounds_validation": ns_u64(timings.decode_input_streams.bounds_validation_ns),
            "allocation_reuse": ns_u64(timings.decode_input_streams.allocation_reuse_ns),
            "unclassified": ns_u64(timings.decode_input_streams.unclassified_ns),
        },
        "partitioned_sparse_writer": {
            "total": ns_u64(timings.partitioned_sparse_writer.total_ns),
            "sparse_partition_traversal": ns_u64(timings.partitioned_sparse_writer.sparse_partition_traversal_ns),
            "output_offset_calculation": ns_u64(timings.partitioned_sparse_writer.output_offset_calculation_ns),
            "field_reconstruction_packing": ns_u64(timings.partitioned_sparse_writer.field_reconstruction_packing_ns),
            "output_byte_stores": ns_u64(timings.partitioned_sparse_writer.output_byte_stores_ns),
            "guard_hash_update": ns_u64(timings.partitioned_sparse_writer.guard_hash_update_ns),
            "bounds_checks": ns_u64(timings.partitioned_sparse_writer.bounds_checks_ns),
            "branch_record_type_handling": ns_u64(timings.partitioned_sparse_writer.branch_record_type_handling_ns),
            "copy_cost": ns_u64(timings.partitioned_sparse_writer.copy_cost_ns),
            "buffer_slicing_view_creation": ns_u64(timings.partitioned_sparse_writer.buffer_slicing_view_creation_ns),
            "partition_finalization": ns_u64(timings.partitioned_sparse_writer.partition_finalization_ns),
            "allocation_reuse": ns_u64(timings.partitioned_sparse_writer.allocation_reuse_ns),
            "unclassified": ns_u64(timings.partitioned_sparse_writer.unclassified_ns),
        },
    })
}

fn empty_decode_timing_json() -> serde_json::Value {
    json!({
        "total": 0,
        "compressed_input_read": 0,
        "huffman_entropy_decode": 0,
        "delta_reconstruction": 0,
        "dictionary_symbol_reconstruction": 0,
        "validity_presence_bitmap_decode": 0,
        "record_type_branching": 0,
        "integer_scaling_sign_extension": 0,
        "temporary_buffer_writes": 0,
        "checksum_hash_work": 0,
        "bounds_validation": 0,
        "allocation_reuse": 0,
        "unclassified": 0,
    })
}

fn empty_writer_timing_json() -> serde_json::Value {
    json!({
        "total": 0,
        "sparse_partition_traversal": 0,
        "output_offset_calculation": 0,
        "field_reconstruction_packing": 0,
        "output_byte_stores": 0,
        "guard_hash_update": 0,
        "bounds_checks": 0,
        "branch_record_type_handling": 0,
        "copy_cost": 0,
        "buffer_slicing_view_creation": 0,
        "partition_finalization": 0,
        "allocation_reuse": 0,
        "unclassified": 0,
    })
}

fn writer_stats_json(stats: Option<&DirectAura1TranscodeStats>) -> serde_json::Value {
    let Some(stats) = stats else {
        return json!({});
    };
    json!({
        "partition_count": stats.writer.partition_count,
        "records_per_partition_min": stats.writer.records_per_partition_min,
        "records_per_partition_max": stats.writer.records_per_partition_max,
        "output_slices": stats.writer.output_slices,
        "non_contiguous_writes": stats.writer.non_contiguous_writes,
        "guard_update_calls": stats.writer.guard_update_calls,
        "guard_update_bytes": stats.writer.guard_update_bytes,
        "average_guard_update_size": if stats.writer.guard_update_calls > 0 {
            stats.writer.guard_update_bytes as f64 / stats.writer.guard_update_calls as f64
        } else {
            0.0
        },
        "output_offset_calculations": stats.writer.output_offset_calculations,
        "bounds_checks": stats.writer.bounds_checks,
        "temporary_buffer_bytes": stats.writer.temporary_buffer_bytes,
        "copied_bytes": stats.writer.copied_bytes,
        "allocation_count": stats.writer.allocation_count,
    })
}

fn decode_stats_json(stats: Option<&DirectAura1TranscodeStats>) -> serde_json::Value {
    let Some(stats) = stats else {
        return json!({
            "stream_count": 0,
            "stream_value_count": 0,
            "materialized_stream_count": 0,
            "materialized_value_count": 0,
            "direct_cursor_stream_count": 0,
            "direct_cursor_value_count": 0,
        });
    };
    json!({
        "stream_count": stats.decode.stream_count,
        "stream_value_count": stats.decode.stream_value_count,
        "materialized_stream_count": stats.decode.materialized_stream_count,
        "materialized_value_count": stats.decode.materialized_value_count,
        "direct_cursor_stream_count": stats.decode.direct_cursor_stream_count,
        "direct_cursor_value_count": stats.decode.direct_cursor_value_count,
    })
}

fn run_operation(
    operation: Operation,
    bytes: &[u8],
    record_count_hint: usize,
    guard_mode: OutputGuardMode,
) -> Result<RunOutcome> {
    match operation {
        Operation::ParseAura1 => parse_aura1(bytes),
        Operation::DecodeAura0 => decode_aura0(bytes),
        Operation::TranscodeAura1ToAura0 => {
            transcode(bytes, Profile::Aura0, record_count_hint, guard_mode)
        }
        Operation::TranscodeAura0ToAura1 => {
            transcode(bytes, Profile::Aura1, record_count_hint, guard_mode)
        }
    }
}

fn inspect_input(operation: Operation, bytes: &[u8]) -> Result<usize> {
    match operation {
        Operation::ParseAura1 | Operation::TranscodeAura1ToAura0 => {
            Ok(records::visit_i64_rows_file(bytes, |_| Ok(()))?)
        }
        Operation::DecodeAura0 | Operation::TranscodeAura0ToAura1 => {
            Ok(records::decode_i64_file(bytes)?.rows.len())
        }
    }
}

fn parse_aura1(bytes: &[u8]) -> Result<RunOutcome> {
    let mut guard = 0xcbf29ce484222325u64;
    let record_count = records::visit_i64_rows_file(bytes, |row| {
        for value in row {
            guard = guard
                .wrapping_mul(0x100000001b3)
                .wrapping_add(*value as u64);
        }
        Ok(())
    })?;
    Ok(RunOutcome {
        record_count,
        output_bytes: 0,
        guard,
        canonical_hash: Some(guard),
        guard_mode: "inline_parse",
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
    })
}

fn decode_aura0(bytes: &[u8]) -> Result<RunOutcome> {
    let decoded = records::decode_i64_file(bytes)?;
    let mut guard = 0xcbf29ce484222325u64;
    for row in &decoded.rows {
        for value in row {
            guard = guard
                .wrapping_mul(0x100000001b3)
                .wrapping_add(*value as u64);
        }
    }
    Ok(RunOutcome {
        record_count: decoded.rows.len(),
        output_bytes: 0,
        guard,
        canonical_hash: Some(guard),
        guard_mode: "inline_decode",
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
    })
}

fn transcode(
    bytes: &[u8],
    target: Profile,
    record_count: usize,
    guard_mode: OutputGuardMode,
) -> Result<RunOutcome> {
    if let Some(output) = records::try_compile_i64_file_profiled(bytes, target, guard_mode)? {
        let output_bytes = output.bytes.len();
        black_box(&output.bytes);
        let guard = output.output_byte_guard.unwrap_or(output_bytes as u64);
        let post_process_duration = duration_from_ns(output.timings.post_output_guard_ns);
        return Ok(RunOutcome {
            record_count,
            output_bytes,
            guard,
            canonical_hash: None,
            guard_mode: output.guard_mode.as_str(),
            output_byte_guard: output.output_byte_guard,
            post_process_duration,
            timings: Some(output.timings),
            stats: Some(output.stats),
        });
    }

    let output = writer::compile_i64(bytes, target)?;
    black_box(&output);
    if guard_mode == OutputGuardMode::NoGuard {
        return Ok(RunOutcome {
            record_count,
            output_bytes: output.len(),
            guard: output.len() as u64,
            canonical_hash: None,
            guard_mode: guard_mode.as_str(),
            output_byte_guard: None,
            post_process_duration: Duration::ZERO,
            timings: None,
            stats: None,
        });
    }
    let post_process_start = Instant::now();
    let guard = bytes_guard(&output);
    let post_process_duration = post_process_start.elapsed();
    Ok(RunOutcome {
        record_count,
        output_bytes: output.len(),
        guard,
        canonical_hash: None,
        guard_mode: "post_process_output",
        output_byte_guard: Some(guard),
        post_process_duration,
        timings: None,
        stats: None,
    })
}

fn percentile_ns<F>(measurements: &[Measurement], percentile: f64, mut duration: F) -> u128
where
    F: FnMut(&Measurement) -> Duration,
{
    let mut values = measurements
        .iter()
        .map(|measurement| duration(measurement).as_nanos())
        .collect::<Vec<_>>();
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * percentile).ceil() as usize;
    values[index]
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

fn bytes_guard(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |acc, byte| {
        acc.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(*byte))
    })
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--short=12")
        .arg("HEAD")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn machine_info() -> serde_json::Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": cpu_model().unwrap_or_else(|| "unknown".to_owned()),
        "logical_cpus": std::thread::available_parallelism().map(usize::from).unwrap_or(0),
        "mem_total_kb": mem_total_kb().unwrap_or(0),
        "rustc": command_output("rustc", &["-V"]).unwrap_or_else(|| "unknown".to_owned()),
    })
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("model name\t: "))
        .map(str::to_owned)
}

fn mem_total_kb() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(command)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[allow(clippy::too_many_arguments)]
fn csv_report(
    config: &Config,
    dataset_sha256: &str,
    input_bytes: usize,
    outcome: &RunOutcome,
    runtime_ns: u128,
    median_ns: u128,
    p95_ns: u128,
    median_total_ns: u128,
    p95_total_ns: u128,
    median_post_process_ns: u128,
    p95_post_process_ns: u128,
    records_per_sec: f64,
    mb_per_sec: f64,
    output_mb_per_sec: f64,
    compression_ratio: Option<f64>,
    command_used: &str,
    machine_info: &serde_json::Value,
) -> String {
    let headers = [
        "dataset_name",
        "dataset_sha256",
        "operation",
        "record_count",
        "input_bytes",
        "output_bytes",
        "runtime_ns",
        "median_runtime_ns",
        "p95_runtime_ns",
        "total_runtime_ns",
        "median_total_runtime_ns",
        "p95_total_runtime_ns",
        "post_process_runtime_ns",
        "median_post_process_runtime_ns",
        "p95_post_process_runtime_ns",
        "post_process_note",
        "guard_mode",
        "records_per_sec",
        "mb_per_sec",
        "output_mb_per_sec",
        "compression_ratio",
        "source_format",
        "target_format",
        "command_used",
        "git_commit",
        "machine_info",
        "iterations",
        "warmups",
        "cache_mode",
        "cache_note",
    ];
    let row = [
        config.dataset_name.clone(),
        dataset_sha256.to_owned(),
        config.operation.as_str().to_owned(),
        outcome.record_count.to_string(),
        input_bytes.to_string(),
        outcome.output_bytes.to_string(),
        runtime_ns.to_string(),
        median_ns.to_string(),
        p95_ns.to_string(),
        median_total_ns.to_string(),
        median_total_ns.to_string(),
        p95_total_ns.to_string(),
        median_post_process_ns.to_string(),
        median_post_process_ns.to_string(),
        p95_post_process_ns.to_string(),
        "post-process time is output guard/checksum work after the measured operation; runtime_ns excludes it".to_owned(),
        outcome.guard_mode.to_owned(),
        format!("{records_per_sec:.6}"),
        format!("{mb_per_sec:.6}"),
        format!("{output_mb_per_sec:.6}"),
        compression_ratio
            .map(|ratio| format!("{ratio:.6}"))
            .unwrap_or_default(),
        config.operation.source_format().to_owned(),
        config.operation.target_format().to_owned(),
        command_used.to_owned(),
        git_commit(),
        machine_info.to_string(),
        config.iterations.to_string(),
        config.warmups.to_string(),
        config.cache_mode.as_str().to_owned(),
        config.cache_mode.note().to_owned(),
    ];

    let mut out = String::new();
    out.push_str(&headers.join(","));
    out.push('\n');
    out.push_str(
        &row.iter()
            .map(|field| csv_escape(field))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    out
}

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

fn print_usage() {
    eprintln!(
        "usage: aura-bench --operation <parse-aura1|decode-aura0|transcode-aura1-to-aura0|transcode-aura0-to-aura1> --dataset <name> --input <path> [--iterations N] [--warmups N] [--format json|csv] [--output path] [--cache-mode warm|cold] [--guard-mode no_guard|fused_output_guard|old_post_output_guard|block_batched_output_guard]"
    );
}
