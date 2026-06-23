#![recursion_limit = "512"]

use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aura_codec::{records, writer, Profile};
use records::{
    Aura0DecodePath, Aura0EncoderPath, OutputGuardMode, ProfiledCompileStats,
    ProfiledCompileTimings, TranscodePath,
};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    ParseAura1,
    DecodeAura0,
    TranscodeAura1ToAura0,
    TranscodeAura0ToAura1,
    Aura1ScanFixed,
    Aura1ReplayCallback,
    Aura1ParseToRows,
    ZstdDecompressOnly,
    ZstdDecompressPlusParse,
    ZstdDecompressPlusReplay,
    ZstdDecompressPlusAura1Output,
    Aura0ToAura1Bytes,
    Aura0ToAura1BytesVerify,
    ZstdAura1ToAura1Bytes,
    ZstdAura1ToAura1BytesVerify,
}

impl Operation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "parse-aura1" => Ok(Self::ParseAura1),
            "decode-aura0" => Ok(Self::DecodeAura0),
            "transcode-aura1-to-aura0" => Ok(Self::TranscodeAura1ToAura0),
            "transcode-aura0-to-aura1" => Ok(Self::TranscodeAura0ToAura1),
            "aura1-scan-fixed" => Ok(Self::Aura1ScanFixed),
            "aura1-replay-callback" => Ok(Self::Aura1ReplayCallback),
            "aura1-parse-to-rows" => Ok(Self::Aura1ParseToRows),
            "zstd-decompress-only" => Ok(Self::ZstdDecompressOnly),
            "zstd-decompress-plus-parse" => Ok(Self::ZstdDecompressPlusParse),
            "zstd-decompress-plus-replay" => Ok(Self::ZstdDecompressPlusReplay),
            "zstd-decompress-plus-aura1-output" => Ok(Self::ZstdDecompressPlusAura1Output),
            "aura0-to-aura1-bytes" => Ok(Self::Aura0ToAura1Bytes),
            "aura0-to-aura1-bytes-verify" => Ok(Self::Aura0ToAura1BytesVerify),
            "zstd-aura1-to-aura1-bytes" => Ok(Self::ZstdAura1ToAura1Bytes),
            "zstd-aura1-to-aura1-bytes-verify" => Ok(Self::ZstdAura1ToAura1BytesVerify),
            other => bail!("unknown operation: {other}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ParseAura1 => "parse-aura1",
            Self::DecodeAura0 => "decode-aura0",
            Self::TranscodeAura1ToAura0 => "transcode-aura1-to-aura0",
            Self::TranscodeAura0ToAura1 => "transcode-aura0-to-aura1",
            Self::Aura1ScanFixed => "aura1-scan-fixed",
            Self::Aura1ReplayCallback => "aura1-replay-callback",
            Self::Aura1ParseToRows => "aura1-parse-to-rows",
            Self::ZstdDecompressOnly => "zstd-decompress-only",
            Self::ZstdDecompressPlusParse => "zstd-decompress-plus-parse",
            Self::ZstdDecompressPlusReplay => "zstd-decompress-plus-replay",
            Self::ZstdDecompressPlusAura1Output => "zstd-decompress-plus-aura1-output",
            Self::Aura0ToAura1Bytes => "aura0-to-aura1-bytes",
            Self::Aura0ToAura1BytesVerify => "aura0-to-aura1-bytes-verify",
            Self::ZstdAura1ToAura1Bytes => "zstd-aura1-to-aura1-bytes",
            Self::ZstdAura1ToAura1BytesVerify => "zstd-aura1-to-aura1-bytes-verify",
        }
    }

    const fn source_format(self) -> &'static str {
        match self {
            Self::ParseAura1
            | Self::TranscodeAura1ToAura0
            | Self::Aura1ScanFixed
            | Self::Aura1ReplayCallback
            | Self::Aura1ParseToRows => "aura1",
            Self::DecodeAura0
            | Self::TranscodeAura0ToAura1
            | Self::Aura0ToAura1Bytes
            | Self::Aura0ToAura1BytesVerify => "aura0",
            Self::ZstdDecompressOnly
            | Self::ZstdDecompressPlusParse
            | Self::ZstdDecompressPlusReplay
            | Self::ZstdDecompressPlusAura1Output
            | Self::ZstdAura1ToAura1Bytes
            | Self::ZstdAura1ToAura1BytesVerify => "zstd",
        }
    }

    const fn target_format(self) -> &'static str {
        match self {
            Self::ParseAura1
            | Self::DecodeAura0
            | Self::Aura1ScanFixed
            | Self::Aura1ReplayCallback
            | Self::Aura1ParseToRows
            | Self::ZstdDecompressOnly
            | Self::ZstdDecompressPlusParse
            | Self::ZstdDecompressPlusReplay => "none",
            Self::TranscodeAura1ToAura0 => "aura0",
            Self::TranscodeAura0ToAura1
            | Self::Aura0ToAura1Bytes
            | Self::Aura0ToAura1BytesVerify
            | Self::ZstdDecompressPlusAura1Output
            | Self::ZstdAura1ToAura1Bytes
            | Self::ZstdAura1ToAura1BytesVerify => "aura1",
        }
    }

    const fn is_transcode(self) -> bool {
        matches!(
            self,
            Self::TranscodeAura1ToAura0 | Self::TranscodeAura0ToAura1
        )
    }

    const fn is_zstd_baseline(self) -> bool {
        matches!(
            self,
            Self::ZstdDecompressOnly
                | Self::ZstdDecompressPlusParse
                | Self::ZstdDecompressPlusReplay
                | Self::ZstdDecompressPlusAura1Output
                | Self::ZstdAura1ToAura1Bytes
                | Self::ZstdAura1ToAura1BytesVerify
        )
    }

    const fn uses_zstd_compressed_input(self) -> bool {
        self.is_zstd_baseline()
    }

    const fn is_fair_bytes_benchmark(self) -> bool {
        matches!(
            self,
            Self::Aura0ToAura1Bytes
                | Self::Aura0ToAura1BytesVerify
                | Self::ZstdAura1ToAura1Bytes
                | Self::ZstdAura1ToAura1BytesVerify
        )
    }

    const fn is_fair_verify(self) -> bool {
        matches!(
            self,
            Self::Aura0ToAura1BytesVerify | Self::ZstdAura1ToAura1BytesVerify
        )
    }

    const fn is_fair_zstd(self) -> bool {
        matches!(
            self,
            Self::ZstdAura1ToAura1Bytes | Self::ZstdAura1ToAura1BytesVerify
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanonicalHashMode {
    None,
    Verify,
}

impl CanonicalHashMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "verify" => Ok(Self::Verify),
            other => bail!("unknown canonical hash mode: {other}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Verify => "verify",
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

fn parse_transcode_path(value: &str) -> Result<TranscodePath> {
    match value {
        "auto" => Ok(TranscodePath::Auto),
        "materialized" => Ok(TranscodePath::Materialized),
        "direct" => Ok(TranscodePath::Direct),
        other => bail!("unknown transcode path: {other}"),
    }
}

fn parse_encoder_path(value: &str) -> Result<Aura0EncoderPath> {
    match value {
        "materialized" => Ok(Aura0EncoderPath::Materialized),
        "direct-streams" => Ok(Aura0EncoderPath::DirectStreams),
        "column-free" => Ok(Aura0EncoderPath::ColumnFree),
        other => bail!("unknown encoder path: {other}"),
    }
}

fn parse_decode_path(value: &str) -> Result<Aura0DecodePath> {
    match value {
        "materialized" => Ok(Aura0DecodePath::Materialized),
        "cursor" => Ok(Aura0DecodePath::Cursor),
        other => bail!("unknown decode path: {other}"),
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
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
    preserve_output: Option<PathBuf>,
    verify_output_decodes: bool,
    canonical_hash_mode: Option<CanonicalHashMode>,
    zstd_level: i32,
    reference_aura0: Option<PathBuf>,
    reference_aura1: Option<PathBuf>,
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
        let mut transcode_path = TranscodePath::Auto;
        let mut encoder_path = Aura0EncoderPath::Materialized;
        let mut decode_path = Aura0DecodePath::Materialized;
        let mut preserve_output = None;
        let mut verify_output_decodes = false;
        let mut canonical_hash_mode = None;
        let mut zstd_level = 3;
        let mut reference_aura0 = None;
        let mut reference_aura1 = None;

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
                "--transcode-path" => {
                    transcode_path = parse_transcode_path(
                        &args.next().context("missing --transcode-path value")?,
                    )?;
                }
                "--encoder-path" => {
                    encoder_path =
                        parse_encoder_path(&args.next().context("missing --encoder-path value")?)?;
                }
                "--decode-path" => {
                    decode_path =
                        parse_decode_path(&args.next().context("missing --decode-path value")?)?;
                }
                "--preserve-output" => {
                    preserve_output = Some(PathBuf::from(
                        args.next().context("missing --preserve-output value")?,
                    ));
                }
                "--verify-output-decodes" => {
                    verify_output_decodes = true;
                }
                "--canonical-hash-mode" => {
                    canonical_hash_mode = Some(CanonicalHashMode::parse(
                        &args.next().context("missing --canonical-hash-mode value")?,
                    )?);
                }
                "--zstd-level" => {
                    zstd_level = args
                        .next()
                        .context("missing --zstd-level value")?
                        .parse()
                        .context("invalid --zstd-level value")?;
                }
                "--reference-aura0" => {
                    reference_aura0 = Some(PathBuf::from(
                        args.next().context("missing --reference-aura0 value")?,
                    ));
                }
                "--reference-aura1" => {
                    reference_aura1 = Some(PathBuf::from(
                        args.next().context("missing --reference-aura1 value")?,
                    ));
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
        let operation = operation.context("missing --operation")?;
        if (preserve_output.is_some() || verify_output_decodes) && !operation.is_transcode() {
            bail!("--preserve-output and --verify-output-decodes require a transcode operation");
        }

        Ok(Self {
            operation,
            dataset_name: dataset_name.context("missing --dataset")?,
            input: input.context("missing --input")?,
            iterations,
            warmups,
            format,
            output,
            cache_mode,
            guard_mode,
            transcode_path,
            encoder_path,
            decode_path,
            preserve_output,
            verify_output_decodes,
            canonical_hash_mode,
            zstd_level,
            reference_aura0,
            reference_aura1,
        })
    }
}

#[derive(Debug, Clone)]
struct RunOutcome {
    record_count: usize,
    output_bytes: usize,
    guard: u64,
    canonical_hash: Option<u64>,
    canonical_hash_time: Duration,
    canonical_hash_equality: Option<bool>,
    guard_mode: &'static str,
    transcode_path: &'static str,
    encoder_path: &'static str,
    conversion_plan_hash: Option<u64>,
    compiled_plan_used: bool,
    plan_setup_time: Duration,
    output_byte_guard: Option<u64>,
    post_process_duration: Duration,
    timings: Option<ProfiledCompileTimings>,
    stats: Option<ProfiledCompileStats>,
    replay_stats: Option<ReplayStats>,
    zstd_stats: Option<ZstdStats>,
    fair_bytes_stats: Option<FairBytesStats>,
    preserved_output: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct Measurement {
    operation_duration: Duration,
    total_duration: Duration,
    outcome: RunOutcome,
}

#[derive(Debug, Clone, Default)]
struct OutputVerification {
    output_preserved: bool,
    output_path: Option<String>,
    output_sha256: Option<String>,
    preserve_output_runtime: Duration,
    decoded_row_equality: Option<bool>,
    record_count_equality: Option<bool>,
    schema_footer_validation: Option<bool>,
    byte_equality: Option<bool>,
    canonical_hash_equality: Option<bool>,
    output_byte_guard_equality: Option<bool>,
    output_verification_runtime: Duration,
}

#[derive(Debug, Clone, Default)]
struct ReplayStats {
    record_width: usize,
    bytes_scanned: usize,
    callback_count: usize,
    callback_time: Duration,
    materialized_row_count: usize,
    materialized_column_count: usize,
    allocations: Option<usize>,
}

#[derive(Debug, Clone)]
struct ZstdStats {
    compressed_input_bytes: usize,
    decompressed_output_bytes: usize,
    logical_record_count: usize,
    work_included: &'static str,
    parse_time: Duration,
    replay_time: Duration,
}

#[derive(Debug, Clone)]
struct FairBytesStats {
    dataset_sha256_aura0: String,
    dataset_sha256_aura1: String,
    dataset_sha256_aura1_zst: String,
    aura0_compressed_bytes: usize,
    aura1_zstd_compressed_bytes: usize,
    aura1_uncompressed_bytes: usize,
    zstd_level: i32,
    output_sink: &'static str,
    compressed_input_bytes: usize,
    uncompressed_output_bytes: usize,
    output_bytes_equal: Option<bool>,
    output_byte_hash: Option<u64>,
}

#[derive(Debug, Clone)]
struct FairBytesContext {
    aura0_bytes: Vec<u8>,
    aura1_bytes: Vec<u8>,
    aura1_zst_bytes: Vec<u8>,
    aura0_sha256: String,
    aura1_sha256: String,
    aura1_zst_sha256: String,
    record_count: usize,
    zstd_level: i32,
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
    let fair_context = load_fair_bytes_context(config, &input_bytes)?;
    let record_count_hint = if let Some(context) = fair_context.as_ref() {
        context.record_count
    } else {
        inspect_input(config.operation, &input_bytes)?
    };
    let effective_canonical_hash_mode = effective_canonical_hash_mode(config);
    let benchmark_input = if config.operation.is_fair_zstd() {
        fair_context
            .as_ref()
            .context("missing fair bytes context")?
            .aura1_zst_bytes
            .clone()
    } else if config.operation.uses_zstd_compressed_input() {
        zstd::stream::encode_all(Cursor::new(input_bytes.as_slice()), config.zstd_level)
            .context("zstd-compress benchmark input")?
    } else {
        input_bytes.clone()
    };
    let benchmark_input_len = benchmark_input.len();

    let should_capture_output = config.preserve_output.is_some() || config.verify_output_decodes;
    let mut captured_output = None;

    for _ in 0..config.warmups {
        match config.cache_mode {
            CacheMode::Warm => {
                let outcome = run_operation(
                    config.operation,
                    &benchmark_input,
                    record_count_hint,
                    config.guard_mode,
                    config.transcode_path,
                    config.encoder_path,
                    config.decode_path,
                    effective_canonical_hash_mode,
                    fair_context.as_ref(),
                    false,
                )?;
                black_box(outcome.guard);
            }
            CacheMode::Cold => {
                let bytes = fs::read(&config.input)?;
                let bytes = if config.operation.is_fair_zstd() {
                    fair_context
                        .as_ref()
                        .context("missing fair bytes context")?
                        .aura1_zst_bytes
                        .clone()
                } else if config.operation.uses_zstd_compressed_input() {
                    zstd::stream::encode_all(Cursor::new(bytes.as_slice()), config.zstd_level)
                        .context("zstd-compress cold benchmark input")?
                } else {
                    bytes
                };
                let outcome = run_operation(
                    config.operation,
                    &bytes,
                    record_count_hint,
                    config.guard_mode,
                    config.transcode_path,
                    config.encoder_path,
                    config.decode_path,
                    effective_canonical_hash_mode,
                    fair_context.as_ref(),
                    false,
                )?;
                black_box(outcome.guard);
            }
        }
    }

    let mut measurements = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let measurement = match config.cache_mode {
            CacheMode::Warm => measure_operation(
                config.operation,
                &benchmark_input,
                record_count_hint,
                config.guard_mode,
                config.transcode_path,
                config.encoder_path,
                config.decode_path,
                effective_canonical_hash_mode,
                fair_context.as_ref(),
                should_capture_output,
            )?,
            CacheMode::Cold => {
                let start = Instant::now();
                let bytes = fs::read(&config.input)?;
                let bytes = if config.operation.is_fair_zstd() {
                    fair_context
                        .as_ref()
                        .context("missing fair bytes context")?
                        .aura1_zst_bytes
                        .clone()
                } else if config.operation.uses_zstd_compressed_input() {
                    zstd::stream::encode_all(Cursor::new(bytes.as_slice()), config.zstd_level)
                        .context("zstd-compress cold benchmark input")?
                } else {
                    bytes
                };
                let outcome = run_operation(
                    config.operation,
                    &bytes,
                    record_count_hint,
                    config.guard_mode,
                    config.transcode_path,
                    config.encoder_path,
                    config.decode_path,
                    effective_canonical_hash_mode,
                    fair_context.as_ref(),
                    should_capture_output,
                )?;
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
        let mut measurement = measurement;
        if should_capture_output {
            if let Some(output) = measurement.outcome.preserved_output.take() {
                captured_output = Some(output);
            }
        }
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
        benchmark_input_len as f64 / (1024.0 * 1024.0) / seconds
    } else {
        0.0
    };
    let output_mb_per_sec = if seconds > 0.0 && first.output_bytes > 0 {
        first.output_bytes as f64 / (1024.0 * 1024.0) / seconds
    } else {
        0.0
    };
    let fair_stats = representative.outcome.fair_bytes_stats.as_ref();
    let compressed_input_mb_sec = if seconds > 0.0 {
        fair_stats
            .map(|stats| stats.compressed_input_bytes as f64 / (1024.0 * 1024.0) / seconds)
            .unwrap_or(mb_per_sec)
    } else {
        0.0
    };
    let uncompressed_output_mb_sec = if seconds > 0.0 {
        fair_stats
            .map(|stats| stats.uncompressed_output_bytes as f64 / (1024.0 * 1024.0) / seconds)
            .unwrap_or(output_mb_per_sec)
    } else {
        0.0
    };
    let result_path = config
        .output
        .as_ref()
        .map(|path| path.display().to_string());
    let output_verification = preserve_and_verify_output(
        config,
        &input_bytes,
        captured_output.as_deref(),
        first.record_count,
        first.output_byte_guard,
        first.canonical_hash,
    )?;
    let working_tree_dirty = git_dirty();
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
                "canonical_hash_mode": effective_canonical_hash_mode.as_str(),
                "canonical_hash_time_ms": first.canonical_hash_time.as_secs_f64() * 1_000.0,
                "canonical_hash_equality": first.canonical_hash_equality.or(output_verification.canonical_hash_equality),
                "conversion_plan_hash": first.conversion_plan_hash,
                "compiled_plan_used": first.compiled_plan_used,
                "plan_mode": if first.compiled_plan_used { "compiled" } else { "none" },
                "plan_setup_time_ms": first.plan_setup_time.as_secs_f64() * 1_000.0,
                "verify_output_decodes_requested": config.verify_output_decodes,
                "output_preserved": output_verification.output_preserved,
                "output_path": output_verification.output_path,
                "output_sha256": output_verification.output_sha256,
                "preserve_output_runtime_ns": ns_u64(output_verification.preserve_output_runtime.as_nanos()),
                "decoded_row_equality": output_verification.decoded_row_equality,
                "record_count_equality": output_verification.record_count_equality,
                "schema_footer_validation": output_verification.schema_footer_validation,
                "byte_equality": output_verification.byte_equality,
                "output_byte_guard_equality": output_verification.output_byte_guard_equality,
                "output_verification_runtime_ns": ns_u64(output_verification.output_verification_runtime.as_nanos()),
                "guard_mode_requested": config.guard_mode.as_str(),
            "guard_mode": first.guard_mode,
            "transcode_path_requested": config.transcode_path.as_str(),
            "transcode_path": first.transcode_path,
            "decode_path_requested": config.decode_path.as_str(),
            "decode_path": config.decode_path.as_str(),
            "encoder_path_requested": config.encoder_path.as_str(),
            "encoder_path": first.encoder_path,
            "direct_streams_enabled": first.encoder_path == Aura0EncoderPath::DirectStreams.as_str(),
            "output_byte_guard": first.output_byte_guard,
                "stage_timings_ns": stage_timings_json(representative.outcome.timings.as_ref()),
                "decode_stats": decode_stats_json(representative.outcome.stats.as_ref()),
                "writer_stats": writer_stats_json(representative.outcome.stats.as_ref()),
                "aura1_to_aura0_stats": aura1_to_aura0_stats_json(representative.outcome.stats.as_ref()),
                "replay_stats": replay_stats_json(representative.outcome.replay_stats.as_ref()),
                "zstd_stats": zstd_stats_json(representative.outcome.zstd_stats.as_ref()),
                "baseline_kind": representative.outcome.zstd_stats.as_ref().map(|_| "zstd"),
                "compressed_input_bytes": representative.outcome.zstd_stats.as_ref().map(|stats| stats.compressed_input_bytes),
                "decompressed_output_bytes": representative.outcome.zstd_stats.as_ref().map(|stats| stats.decompressed_output_bytes),
                "logical_record_count": representative.outcome.zstd_stats.as_ref().map(|stats| stats.logical_record_count),
                "work_included": representative.outcome.zstd_stats.as_ref().map(|stats| stats.work_included),
                "parse_time_ms": representative.outcome.zstd_stats.as_ref().map(|stats| stats.parse_time.as_secs_f64() * 1_000.0),
                "replay_time_ms": representative.outcome.zstd_stats.as_ref().map(|stats| stats.replay_time.as_secs_f64() * 1_000.0),
                "dataset_sha256_aura0": fair_stats.map(|stats| stats.dataset_sha256_aura0.as_str()),
                "dataset_sha256_aura1": fair_stats.map(|stats| stats.dataset_sha256_aura1.as_str()),
                "dataset_sha256_aura1_zst": fair_stats.map(|stats| stats.dataset_sha256_aura1_zst.as_str()),
                "aura0_compressed_bytes": fair_stats.map(|stats| stats.aura0_compressed_bytes),
                "aura1_zstd_compressed_bytes": fair_stats.map(|stats| stats.aura1_zstd_compressed_bytes),
                "aura1_uncompressed_bytes": fair_stats.map(|stats| stats.aura1_uncompressed_bytes),
                "zstd_level": fair_stats.map(|stats| stats.zstd_level).unwrap_or(config.zstd_level),
                "output_sink": fair_stats.map(|stats| stats.output_sink),
                "compressed_input_mb_sec": compressed_input_mb_sec,
                "uncompressed_output_mb_sec": uncompressed_output_mb_sec,
                "output_bytes_equal": fair_stats.and_then(|stats| stats.output_bytes_equal),
                "output_byte_hash": fair_stats.and_then(|stats| stats.output_byte_hash),
                "records_per_sec": records_per_sec,
                "input_mb_per_sec": mb_per_sec,
                "mb_per_sec": mb_per_sec,
                "output_mb_per_sec": output_mb_per_sec,
                "compression_ratio": compression_ratio,
                "source_format": config.operation.source_format(),
                "target_format": config.operation.target_format(),
                "command_used": command_used,
                "git_commit": git_commit(),
                "working_tree_dirty": working_tree_dirty,
                "working_tree_status": if working_tree_dirty { "dirty" } else { "clean" },
                "machine_info": machine_info,
                "iterations": config.iterations,
                "warmups": config.warmups,
                "cache_mode": config.cache_mode.as_str(),
                "cache_note": config.cache_mode.note(),
                "result_path": result_path,
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

fn load_fair_bytes_context(
    config: &Config,
    input_bytes: &[u8],
) -> Result<Option<FairBytesContext>> {
    if !config.operation.is_fair_bytes_benchmark() {
        return Ok(None);
    }

    let aura0_path = config
        .reference_aura0
        .as_ref()
        .context("fair bytes benchmark requires --reference-aura0")?;
    let aura1_path = config
        .reference_aura1
        .as_ref()
        .context("fair bytes benchmark requires --reference-aura1")?;
    let aura0_bytes = fs::read(aura0_path)
        .with_context(|| format!("read reference Aura0 {}", aura0_path.display()))?;
    let aura1_bytes = fs::read(aura1_path)
        .with_context(|| format!("read reference Aura1 {}", aura1_path.display()))?;
    let input_sha = sha256_hex(input_bytes);
    let aura0_sha256 = sha256_hex(&aura0_bytes);
    let aura1_sha256 = sha256_hex(&aura1_bytes);

    if matches!(
        config.operation,
        Operation::Aura0ToAura1Bytes | Operation::Aura0ToAura1BytesVerify
    ) && input_sha != aura0_sha256
    {
        bail!("--input must match --reference-aura0 for Aura0 fair bytes benchmark");
    }
    if matches!(
        config.operation,
        Operation::ZstdAura1ToAura1Bytes | Operation::ZstdAura1ToAura1BytesVerify
    ) && input_sha != aura1_sha256
    {
        bail!("--input must match --reference-aura1 for zstd fair bytes benchmark");
    }

    let aura1_zst_bytes =
        zstd::stream::encode_all(Cursor::new(aura1_bytes.as_slice()), config.zstd_level)
            .context("zstd-compress reference Aura1")?;
    let aura1_zst_sha256 = sha256_hex(&aura1_zst_bytes);
    let record_count = records::visit_i64_rows_file(&aura1_bytes, |_| Ok(()))?;

    Ok(Some(FairBytesContext {
        aura0_bytes,
        aura1_bytes,
        aura1_zst_bytes,
        aura0_sha256,
        aura1_sha256,
        aura1_zst_sha256,
        record_count,
        zstd_level: config.zstd_level,
    }))
}

fn measure_operation(
    operation: Operation,
    bytes: &[u8],
    record_count_hint: usize,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
    canonical_hash_mode: CanonicalHashMode,
    fair_context: Option<&FairBytesContext>,
    capture_output: bool,
) -> Result<Measurement> {
    let start = Instant::now();
    let outcome = run_operation(
        operation,
        bytes,
        record_count_hint,
        guard_mode,
        transcode_path,
        encoder_path,
        decode_path,
        canonical_hash_mode,
        fair_context,
        capture_output,
    )?;
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

fn preserve_and_verify_output(
    config: &Config,
    input_bytes: &[u8],
    output_bytes: Option<&[u8]>,
    expected_record_count: usize,
    expected_output_guard: Option<u64>,
    expected_canonical_hash: Option<u64>,
) -> Result<OutputVerification> {
    let Some(output_bytes) = output_bytes else {
        if config.preserve_output.is_some() || config.verify_output_decodes {
            bail!("transcode output bytes were not captured");
        }
        return Ok(OutputVerification::default());
    };

    let mut result = OutputVerification::default();

    if let Some(path) = &config.preserve_output {
        let start = Instant::now();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create output directory {}", parent.display()))?;
        }
        fs::write(path, output_bytes)
            .with_context(|| format!("write preserved output {}", path.display()))?;
        result.preserve_output_runtime = start.elapsed();
        result.output_preserved = true;
        result.output_path = Some(path.display().to_string());
        result.output_sha256 = Some(sha256_hex(output_bytes));
    }

    if config.verify_output_decodes {
        let start = Instant::now();
        let source = records::decode_i64_file(input_bytes)
            .context("decode transcode source for output verification")?;
        let output = records::decode_i64_file(output_bytes)
            .context("decode transcode output for output verification")?;
        let output_guard = bytes_guard(output_bytes);
        result.decoded_row_equality = Some(source.rows == output.rows);
        result.record_count_equality = Some(
            output.rows.len() == source.rows.len() && output.rows.len() == expected_record_count,
        );
        result.schema_footer_validation = Some(true);
        result.output_byte_guard_equality =
            expected_output_guard.map(|guard| guard == output_guard);
        let source_hash = canonical_hash_i64_file(input_bytes)?.0;
        let output_hash = canonical_hash_i64_file(output_bytes)?.0;
        result.canonical_hash_equality = Some(
            source_hash == output_hash
                && expected_canonical_hash.is_none_or(|hash| hash == output_hash),
        );
        result.output_verification_runtime = start.elapsed();
    }

    Ok(result)
}

fn effective_canonical_hash_mode(config: &Config) -> CanonicalHashMode {
    if let Some(mode) = config.canonical_hash_mode {
        return mode;
    }
    match config.operation {
        Operation::ParseAura1 | Operation::DecodeAura0 => CanonicalHashMode::Verify,
        _ => CanonicalHashMode::None,
    }
}

fn duration_from_ns(ns: u128) -> Duration {
    Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
}

fn profiled_post_process_duration(timings: &ProfiledCompileTimings) -> Duration {
    let ns = timings
        .aura0_to_aura1
        .as_ref()
        .map(|timings| timings.post_output_guard_ns)
        .or_else(|| {
            timings
                .aura1_to_aura0
                .as_ref()
                .map(|timings| timings.post_output_guard_ns)
        })
        .unwrap_or(0);
    duration_from_ns(ns)
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

fn stage_timings_json(timings: Option<&ProfiledCompileTimings>) -> serde_json::Value {
    let aura0_to_aura1 = timings.and_then(|timings| timings.aura0_to_aura1.as_ref());
    let aura1_to_aura0 = timings.and_then(|timings| timings.aura1_to_aura0.as_ref());
    let Some(timings) = aura0_to_aura1 else {
        return json!({
            "decode_input_streams": empty_decode_timing_json(),
            "partitioned_sparse_writer": empty_writer_timing_json(),
            "aura1_to_aura0": aura1_to_aura0_timing_json(aura1_to_aura0),
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
        "aura1_to_aura0": aura1_to_aura0_timing_json(aura1_to_aura0),
    })
}

fn aura1_to_aura0_timing_json(
    timings: Option<&records::DirectAura0TranscodeTimings>,
) -> serde_json::Value {
    let Some(timings) = timings else {
        return json!({
            "total": 0,
            "metadata": 0,
            "fixed_row_scan": 0,
            "stats_frequency_collection": 0,
            "direct_stream_construction": 0,
            "frequency_pass_time_ms": 0.0,
            "direct_stream_emit_time_ms": 0.0,
            "compression_time_ms": 0.0,
            "encode_total_time_ms": 0.0,
            "field_extraction": 0,
            "dictionary_state_update": 0,
            "delta_stream_construction": 0,
            "compression_encoding": 0,
            "writer_finalization": 0,
            "canonical_hash": 0,
            "post_output_guard": 0,
        });
    };
    json!({
        "total": ns_u64(timings.total_ns),
        "metadata": ns_u64(timings.metadata_ns),
        "fixed_row_scan": ns_u64(timings.fixed_row_scan_ns),
        "stats_frequency_collection": ns_u64(timings.stats_frequency_collection_ns),
        "direct_stream_construction": ns_u64(timings.direct_stream_construction_ns),
        "frequency_pass_time_ms": timings.stats_frequency_collection_ns as f64 / 1_000_000.0,
        "direct_stream_emit_time_ms": timings.direct_stream_construction_ns as f64 / 1_000_000.0,
        "compression_time_ms": timings.compression_encoding_ns as f64 / 1_000_000.0,
        "encode_total_time_ms": (timings.stats_frequency_collection_ns
            + timings.direct_stream_construction_ns
            + timings.compression_encoding_ns) as f64 / 1_000_000.0,
        "field_extraction": ns_u64(timings.field_extraction_ns),
        "dictionary_state_update": ns_u64(timings.dictionary_state_update_ns),
        "delta_stream_construction": ns_u64(timings.delta_stream_construction_ns),
        "compression_encoding": ns_u64(timings.compression_encoding_ns),
        "writer_finalization": ns_u64(timings.writer_finalization_ns),
        "canonical_hash": ns_u64(timings.canonical_hash_ns),
        "post_output_guard": ns_u64(timings.post_output_guard_ns),
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

fn writer_stats_json(stats: Option<&ProfiledCompileStats>) -> serde_json::Value {
    let Some(stats) = stats.and_then(|stats| stats.aura0_to_aura1.as_ref()) else {
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

fn decode_stats_json(stats: Option<&ProfiledCompileStats>) -> serde_json::Value {
    let Some(stats) = stats.and_then(|stats| stats.aura0_to_aura1.as_ref()) else {
        return json!({
            "cursor_enabled": false,
            "stream_count": 0,
            "stream_value_count": 0,
            "materialized_stream_count": 0,
            "materialized_value_count": 0,
            "direct_cursor_stream_count": 0,
            "direct_cursor_value_count": 0,
        });
    };
    json!({
        "cursor_enabled": stats.decode.direct_cursor_stream_count > 0,
        "stream_count": stats.decode.stream_count,
        "stream_value_count": stats.decode.stream_value_count,
        "materialized_stream_count": stats.decode.materialized_stream_count,
        "materialized_value_count": stats.decode.materialized_value_count,
        "direct_cursor_stream_count": stats.decode.direct_cursor_stream_count,
        "direct_cursor_value_count": stats.decode.direct_cursor_value_count,
    })
}

fn aura1_to_aura0_stats_json(stats: Option<&ProfiledCompileStats>) -> serde_json::Value {
    let Some(stats) = stats.and_then(|stats| stats.aura1_to_aura0.as_ref()) else {
        return json!({
            "encoder_path": "none",
            "direct_streams_enabled": false,
            "column_vector_count": 0,
            "column_value_count": 0,
            "rows_scanned": 0,
            "aura1_rows_scanned": 0,
            "aura1_scan_passes": 0,
            "row_allocations": 0,
            "stream_vector_count": 0,
            "stream_vector_allocations": 0,
            "stream_count": 0,
            "stream_value_count": 0,
            "direct_stream_count": 0,
            "direct_stream_value_count": 0,
            "bytes_read": 0,
            "bytes_written": 0,
            "copied_bytes": 0,
            "temporary_buffer_bytes": 0,
            "encoder_allocations": 0,
            "output_blocks": 0,
            "compression_blocks": 0,
        });
    };
    json!({
        "encoder_path": stats.encoder_path.as_str(),
        "direct_streams_enabled": stats.direct_streams_enabled,
        "column_vector_count": stats.column_vector_count,
        "column_value_count": stats.column_value_count,
        "rows_scanned": stats.rows_scanned,
        "aura1_rows_scanned": stats.rows_scanned,
        "aura1_scan_passes": stats.aura1_scan_passes,
        "row_allocations": stats.row_allocations,
        "stream_vector_count": stats.stream_vector_count,
        "stream_vector_allocations": stats.stream_vector_allocations,
        "stream_count": stats.stream_count,
        "stream_value_count": stats.stream_value_count,
        "direct_stream_count": stats.direct_stream_count,
        "direct_stream_value_count": stats.direct_stream_value_count,
        "bytes_read": stats.bytes_read,
        "bytes_written": stats.bytes_written,
        "copied_bytes": stats.copied_bytes,
        "temporary_buffer_bytes": stats.temporary_buffer_bytes,
        "encoder_allocations": stats.encoder_allocations,
        "output_blocks": stats.output_blocks,
        "compression_blocks": stats.compression_blocks,
    })
}

fn replay_stats_json(stats: Option<&ReplayStats>) -> serde_json::Value {
    let Some(stats) = stats else {
        return json!({
            "record_width": 0,
            "bytes_scanned": 0,
            "callback_count": 0,
            "callback_time_ms": 0.0,
            "materialized_row_count": 0,
            "materialized_column_count": 0,
            "allocations": serde_json::Value::Null,
        });
    };
    json!({
        "record_width": stats.record_width,
        "bytes_scanned": stats.bytes_scanned,
        "callback_count": stats.callback_count,
        "callback_time_ms": stats.callback_time.as_secs_f64() * 1_000.0,
        "materialized_row_count": stats.materialized_row_count,
        "materialized_column_count": stats.materialized_column_count,
        "allocations": stats.allocations,
    })
}

fn zstd_stats_json(stats: Option<&ZstdStats>) -> serde_json::Value {
    let Some(stats) = stats else {
        return json!({});
    };
    json!({
        "baseline_kind": "zstd",
        "compressed_input_bytes": stats.compressed_input_bytes,
        "decompressed_output_bytes": stats.decompressed_output_bytes,
        "logical_record_count": stats.logical_record_count,
        "work_included": stats.work_included,
        "parse_time_ms": stats.parse_time.as_secs_f64() * 1_000.0,
        "replay_time_ms": stats.replay_time.as_secs_f64() * 1_000.0,
    })
}

fn run_operation(
    operation: Operation,
    bytes: &[u8],
    record_count_hint: usize,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
    canonical_hash_mode: CanonicalHashMode,
    fair_context: Option<&FairBytesContext>,
    capture_output: bool,
) -> Result<RunOutcome> {
    match operation {
        Operation::ParseAura1 => parse_aura1(bytes, canonical_hash_mode),
        Operation::DecodeAura0 => decode_aura0(bytes, canonical_hash_mode),
        Operation::Aura1ScanFixed => aura1_scan_fixed(bytes, canonical_hash_mode),
        Operation::Aura1ReplayCallback => aura1_replay_callback(bytes, canonical_hash_mode),
        Operation::Aura1ParseToRows => aura1_parse_to_rows(bytes, canonical_hash_mode),
        Operation::ZstdDecompressOnly
        | Operation::ZstdDecompressPlusParse
        | Operation::ZstdDecompressPlusReplay
        | Operation::ZstdDecompressPlusAura1Output => {
            zstd_baseline(operation, bytes, record_count_hint, canonical_hash_mode)
        }
        Operation::Aura0ToAura1Bytes
        | Operation::Aura0ToAura1BytesVerify
        | Operation::ZstdAura1ToAura1Bytes
        | Operation::ZstdAura1ToAura1BytesVerify => fair_aura1_bytes_operation(
            operation,
            bytes,
            fair_context.context("missing fair bytes context")?,
        ),
        Operation::TranscodeAura1ToAura0 => transcode(
            bytes,
            Profile::Aura0,
            record_count_hint,
            guard_mode,
            transcode_path,
            encoder_path,
            decode_path,
            canonical_hash_mode,
            capture_output,
        ),
        Operation::TranscodeAura0ToAura1 => transcode(
            bytes,
            Profile::Aura1,
            record_count_hint,
            guard_mode,
            transcode_path,
            encoder_path,
            decode_path,
            canonical_hash_mode,
            capture_output,
        ),
    }
}

fn inspect_input(operation: Operation, bytes: &[u8]) -> Result<usize> {
    match operation {
        Operation::ParseAura1
        | Operation::TranscodeAura1ToAura0
        | Operation::Aura1ScanFixed
        | Operation::Aura1ReplayCallback
        | Operation::Aura1ParseToRows
        | Operation::ZstdDecompressOnly
        | Operation::ZstdDecompressPlusParse
        | Operation::ZstdDecompressPlusReplay
        | Operation::ZstdDecompressPlusAura1Output
        | Operation::ZstdAura1ToAura1Bytes
        | Operation::ZstdAura1ToAura1BytesVerify => {
            Ok(records::visit_i64_rows_file(bytes, |_| Ok(()))?)
        }
        Operation::DecodeAura0
        | Operation::TranscodeAura0ToAura1
        | Operation::Aura0ToAura1Bytes
        | Operation::Aura0ToAura1BytesVerify => Ok(records::decode_i64_file(bytes)?.rows.len()),
    }
}

fn parse_aura1(bytes: &[u8], canonical_hash_mode: CanonicalHashMode) -> Result<RunOutcome> {
    let plan_start = Instant::now();
    let layout = records::aura1_fixed_layout_info(bytes)?;
    let plan_setup_time = plan_start.elapsed();
    let mut guard = canonical_hash_init();
    let record_count = records::visit_i64_rows_file(bytes, |row| {
        update_canonical_hash_row(&mut guard, row);
        Ok(())
    })?;
    Ok(RunOutcome {
        record_count,
        output_bytes: 0,
        guard,
        canonical_hash: (canonical_hash_mode == CanonicalHashMode::Verify).then_some(guard),
        canonical_hash_time: Duration::ZERO,
        canonical_hash_equality: None,
        guard_mode: "inline_parse",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: Some(layout.conversion_plan_hash),
        compiled_plan_used: true,
        plan_setup_time,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats: None,
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output: None,
    })
}

fn decode_aura0(bytes: &[u8], canonical_hash_mode: CanonicalHashMode) -> Result<RunOutcome> {
    let hash_start = Instant::now();
    let (guard, record_count) = canonical_hash_i64_file(bytes)?;
    let canonical_hash_time = hash_start.elapsed();
    Ok(RunOutcome {
        record_count,
        output_bytes: 0,
        guard,
        canonical_hash: (canonical_hash_mode == CanonicalHashMode::Verify).then_some(guard),
        canonical_hash_time,
        canonical_hash_equality: None,
        guard_mode: "inline_decode",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: None,
        compiled_plan_used: false,
        plan_setup_time: Duration::ZERO,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats: None,
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output: None,
    })
}

fn canonical_hash_init() -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in b"AURA_CANON_I64_V1" {
        hash = hash
            .wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(*byte));
    }
    hash
}

fn update_canonical_hash_value(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash = hash
            .wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(byte));
    }
}

fn update_canonical_hash_row(hash: &mut u64, row: &[i64]) {
    update_canonical_hash_value(hash, row.len() as u64);
    for value in row {
        update_canonical_hash_value(hash, *value as u64);
    }
}

fn canonical_hash_i64_file(bytes: &[u8]) -> Result<(u64, usize)> {
    let mut hash = canonical_hash_init();
    if let Ok(record_count) = records::visit_i64_rows_file(bytes, |row| {
        update_canonical_hash_row(&mut hash, row);
        Ok(())
    }) {
        return Ok((hash, record_count));
    }

    if let Some(columns) = records::decode_i64_columns_file(bytes)? {
        for row_index in 0..columns.record_count {
            update_canonical_hash_value(&mut hash, columns.columns.len() as u64);
            for column in &columns.columns {
                update_canonical_hash_value(&mut hash, column[row_index] as u64);
            }
        }
        return Ok((hash, columns.record_count));
    }

    let decoded = records::decode_i64_file(bytes)?;
    for row in &decoded.rows {
        update_canonical_hash_row(&mut hash, row);
    }
    Ok((hash, decoded.rows.len()))
}

fn transcode(
    bytes: &[u8],
    target: Profile,
    record_count: usize,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
    canonical_hash_mode: CanonicalHashMode,
    capture_output: bool,
) -> Result<RunOutcome> {
    if let Some(output) = records::try_compile_i64_file_profiled(
        bytes,
        target,
        guard_mode,
        transcode_path,
        encoder_path,
        decode_path,
    )? {
        let records::ProfiledCompileOutput {
            bytes: output_bytes_vec,
            output_byte_guard,
            guard_mode,
            transcode_path,
            encoder_path,
            conversion_plan_hash,
            timings,
            stats,
        } = output;
        let output_bytes = output_bytes_vec.len();
        black_box(&output_bytes_vec);
        let guard = output_byte_guard.unwrap_or(output_bytes as u64);
        let post_process_duration = profiled_post_process_duration(&timings);
        let (canonical_hash, canonical_hash_time, canonical_hash_equality) =
            if canonical_hash_mode == CanonicalHashMode::Verify {
                let hash_start = Instant::now();
                let (source_hash, _) = canonical_hash_i64_file(bytes)?;
                let (output_hash, _) = canonical_hash_i64_file(&output_bytes_vec)?;
                (
                    Some(source_hash),
                    hash_start.elapsed(),
                    Some(source_hash == output_hash),
                )
            } else {
                (None, Duration::ZERO, None)
            };
        let preserved_output = capture_output.then_some(output_bytes_vec);
        return Ok(RunOutcome {
            record_count,
            output_bytes,
            guard,
            canonical_hash,
            canonical_hash_time,
            canonical_hash_equality,
            guard_mode: guard_mode.as_str(),
            transcode_path: transcode_path.as_str(),
            encoder_path: encoder_path.as_str(),
            conversion_plan_hash,
            compiled_plan_used: conversion_plan_hash.is_some(),
            plan_setup_time: Duration::ZERO,
            output_byte_guard,
            post_process_duration,
            timings: Some(timings),
            stats: Some(stats),
            replay_stats: None,
            zstd_stats: None,
            fair_bytes_stats: None,
            preserved_output,
        });
    }
    if transcode_path == TranscodePath::Direct {
        bail!("direct transcode path unsupported for this input/target pair");
    }

    let output = writer::compile_i64(bytes, target)?;
    black_box(&output);
    if guard_mode == OutputGuardMode::NoGuard {
        let output_bytes = output.len();
        let (canonical_hash, canonical_hash_time, canonical_hash_equality) =
            if canonical_hash_mode == CanonicalHashMode::Verify {
                let hash_start = Instant::now();
                let (source_hash, _) = canonical_hash_i64_file(bytes)?;
                let (output_hash, _) = canonical_hash_i64_file(&output)?;
                (
                    Some(source_hash),
                    hash_start.elapsed(),
                    Some(source_hash == output_hash),
                )
            } else {
                (None, Duration::ZERO, None)
            };
        let preserved_output = capture_output.then_some(output);
        return Ok(RunOutcome {
            record_count,
            output_bytes,
            guard: output_bytes as u64,
            canonical_hash,
            canonical_hash_time,
            canonical_hash_equality,
            guard_mode: guard_mode.as_str(),
            transcode_path: "materialized",
            encoder_path: Aura0EncoderPath::Materialized.as_str(),
            conversion_plan_hash: None,
            compiled_plan_used: false,
            plan_setup_time: Duration::ZERO,
            output_byte_guard: None,
            post_process_duration: Duration::ZERO,
            timings: None,
            stats: None,
            replay_stats: None,
            zstd_stats: None,
            fair_bytes_stats: None,
            preserved_output,
        });
    }
    let post_process_start = Instant::now();
    let guard = bytes_guard(&output);
    let post_process_duration = post_process_start.elapsed();
    let output_bytes = output.len();
    let (canonical_hash, canonical_hash_time, canonical_hash_equality) =
        if canonical_hash_mode == CanonicalHashMode::Verify {
            let hash_start = Instant::now();
            let (source_hash, _) = canonical_hash_i64_file(bytes)?;
            let (output_hash, _) = canonical_hash_i64_file(&output)?;
            (
                Some(source_hash),
                hash_start.elapsed(),
                Some(source_hash == output_hash),
            )
        } else {
            (None, Duration::ZERO, None)
        };
    let preserved_output = capture_output.then_some(output);
    Ok(RunOutcome {
        record_count,
        output_bytes,
        guard,
        canonical_hash,
        canonical_hash_time,
        canonical_hash_equality,
        guard_mode: "post_process_output",
        transcode_path: "materialized",
        encoder_path: Aura0EncoderPath::Materialized.as_str(),
        conversion_plan_hash: None,
        compiled_plan_used: false,
        plan_setup_time: Duration::ZERO,
        output_byte_guard: Some(guard),
        post_process_duration,
        timings: None,
        stats: None,
        replay_stats: None,
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output,
    })
}

fn aura1_scan_fixed(bytes: &[u8], canonical_hash_mode: CanonicalHashMode) -> Result<RunOutcome> {
    let plan_start = Instant::now();
    let layout = records::aura1_fixed_layout_info(bytes)?;
    let plan_setup_time = plan_start.elapsed();
    let hash_start = Instant::now();
    let mut hash = canonical_hash_init();
    let mut record_count = 0usize;
    records::visit_i64_rows_file(bytes, |row| {
        if canonical_hash_mode == CanonicalHashMode::Verify {
            update_canonical_hash_row(&mut hash, row);
        }
        record_count += 1;
        Ok(())
    })?;
    let canonical_hash_time = if canonical_hash_mode == CanonicalHashMode::Verify {
        hash_start.elapsed()
    } else {
        Duration::ZERO
    };
    Ok(RunOutcome {
        record_count,
        output_bytes: 0,
        guard: record_count as u64,
        canonical_hash: (canonical_hash_mode == CanonicalHashMode::Verify).then_some(hash),
        canonical_hash_time,
        canonical_hash_equality: None,
        guard_mode: "no_guard",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: Some(layout.conversion_plan_hash),
        compiled_plan_used: true,
        plan_setup_time,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats: Some(ReplayStats {
            record_width: layout.record_width,
            bytes_scanned: layout.body_bytes,
            callback_count: 0,
            callback_time: Duration::ZERO,
            materialized_row_count: 0,
            materialized_column_count: 0,
            allocations: Some(0),
        }),
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output: None,
    })
}

fn aura1_replay_callback(
    bytes: &[u8],
    canonical_hash_mode: CanonicalHashMode,
) -> Result<RunOutcome> {
    let plan_start = Instant::now();
    let layout = records::aura1_fixed_layout_info(bytes)?;
    let plan_setup_time = plan_start.elapsed();
    let mut hash = canonical_hash_init();
    let mut record_count = 0usize;
    let mut callback_count = 0usize;
    let mut callback_time = Duration::ZERO;
    let hash_start = Instant::now();
    records::visit_i64_rows_file(bytes, |row| {
        if canonical_hash_mode == CanonicalHashMode::Verify {
            update_canonical_hash_row(&mut hash, row);
        }
        let callback_start = Instant::now();
        black_box(row);
        callback_time += callback_start.elapsed();
        record_count += 1;
        callback_count += 1;
        Ok(())
    })?;
    let canonical_hash_time = if canonical_hash_mode == CanonicalHashMode::Verify {
        hash_start.elapsed().saturating_sub(callback_time)
    } else {
        Duration::ZERO
    };
    Ok(RunOutcome {
        record_count,
        output_bytes: 0,
        guard: callback_count as u64,
        canonical_hash: (canonical_hash_mode == CanonicalHashMode::Verify).then_some(hash),
        canonical_hash_time,
        canonical_hash_equality: None,
        guard_mode: "no_guard",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: Some(layout.conversion_plan_hash),
        compiled_plan_used: true,
        plan_setup_time,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats: Some(ReplayStats {
            record_width: layout.record_width,
            bytes_scanned: layout.body_bytes,
            callback_count,
            callback_time,
            materialized_row_count: 0,
            materialized_column_count: 0,
            allocations: Some(0),
        }),
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output: None,
    })
}

fn aura1_parse_to_rows(bytes: &[u8], canonical_hash_mode: CanonicalHashMode) -> Result<RunOutcome> {
    let plan_start = Instant::now();
    let layout = records::aura1_fixed_layout_info(bytes)?;
    let plan_setup_time = plan_start.elapsed();
    let decoded = records::decode_i64_file(bytes)?;
    let hash_start = Instant::now();
    let mut hash = canonical_hash_init();
    if canonical_hash_mode == CanonicalHashMode::Verify {
        for row in &decoded.rows {
            update_canonical_hash_row(&mut hash, row);
        }
    }
    let canonical_hash_time = if canonical_hash_mode == CanonicalHashMode::Verify {
        hash_start.elapsed()
    } else {
        Duration::ZERO
    };
    Ok(RunOutcome {
        record_count: decoded.rows.len(),
        output_bytes: 0,
        guard: decoded.rows.len() as u64,
        canonical_hash: (canonical_hash_mode == CanonicalHashMode::Verify).then_some(hash),
        canonical_hash_time,
        canonical_hash_equality: None,
        guard_mode: "no_guard",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: Some(layout.conversion_plan_hash),
        compiled_plan_used: true,
        plan_setup_time,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats: Some(ReplayStats {
            record_width: layout.record_width,
            bytes_scanned: layout.body_bytes,
            callback_count: 0,
            callback_time: Duration::ZERO,
            materialized_row_count: decoded.rows.len(),
            materialized_column_count: 0,
            allocations: None,
        }),
        zstd_stats: None,
        fair_bytes_stats: None,
        preserved_output: None,
    })
}

fn fair_aura1_bytes_operation(
    operation: Operation,
    bytes: &[u8],
    context: &FairBytesContext,
) -> Result<RunOutcome> {
    let verify = operation.is_fair_verify();
    let is_zstd = operation.is_fair_zstd();
    let mut timings = None;
    let mut stats = None;
    let mut conversion_plan_hash = None;
    let mut compiled_plan_used = false;

    let output = if is_zstd {
        zstd::stream::decode_all(Cursor::new(bytes)).context("zstd-decompress Aura1 bytes")?
    } else if let Some(profiled) = records::try_compile_i64_file_profiled(
        bytes,
        Profile::Aura1,
        OutputGuardMode::NoGuard,
        TranscodePath::Auto,
        Aura0EncoderPath::Materialized,
        Aura0DecodePath::Materialized,
    )? {
        conversion_plan_hash = profiled.conversion_plan_hash;
        compiled_plan_used = conversion_plan_hash.is_some();
        timings = Some(profiled.timings);
        stats = Some(profiled.stats);
        profiled.bytes
    } else {
        writer::compile_i64(bytes, Profile::Aura1)?
    };

    black_box(&output);
    let output_bytes_equal = verify.then_some(output == context.aura1_bytes);
    let output_byte_hash = verify.then_some(bytes_guard(&output));
    let compressed_input_bytes = if is_zstd {
        context.aura1_zst_bytes.len()
    } else {
        context.aura0_bytes.len()
    };
    let output_len = output.len();

    Ok(RunOutcome {
        record_count: context.record_count,
        output_bytes: output_len,
        guard: output_byte_hash.unwrap_or(output_len as u64),
        canonical_hash: None,
        canonical_hash_time: Duration::ZERO,
        canonical_hash_equality: None,
        guard_mode: "no_guard",
        transcode_path: if is_zstd { "none" } else { "auto" },
        encoder_path: "none",
        conversion_plan_hash,
        compiled_plan_used,
        plan_setup_time: Duration::ZERO,
        output_byte_guard: output_byte_hash,
        post_process_duration: Duration::ZERO,
        timings,
        stats,
        replay_stats: None,
        zstd_stats: is_zstd.then_some(ZstdStats {
            compressed_input_bytes: context.aura1_zst_bytes.len(),
            decompressed_output_bytes: output_len,
            logical_record_count: context.record_count,
            work_included: "zstd Aura1.zst -> Aura1 bytes",
            parse_time: Duration::ZERO,
            replay_time: Duration::ZERO,
        }),
        fair_bytes_stats: Some(FairBytesStats {
            dataset_sha256_aura0: context.aura0_sha256.clone(),
            dataset_sha256_aura1: context.aura1_sha256.clone(),
            dataset_sha256_aura1_zst: context.aura1_zst_sha256.clone(),
            aura0_compressed_bytes: context.aura0_bytes.len(),
            aura1_zstd_compressed_bytes: context.aura1_zst_bytes.len(),
            aura1_uncompressed_bytes: context.aura1_bytes.len(),
            zstd_level: context.zstd_level,
            output_sink: "memory_vec",
            compressed_input_bytes,
            uncompressed_output_bytes: output_len,
            output_bytes_equal,
            output_byte_hash,
        }),
        preserved_output: None,
    })
}

fn zstd_baseline(
    operation: Operation,
    bytes: &[u8],
    record_count_hint: usize,
    canonical_hash_mode: CanonicalHashMode,
) -> Result<RunOutcome> {
    let decompressed = zstd::stream::decode_all(Cursor::new(bytes)).context("zstd-decompress")?;
    let decompressed_output_bytes = decompressed.len();
    let mut parse_time = Duration::ZERO;
    let mut replay_time = Duration::ZERO;
    let mut record_count = record_count_hint;
    let mut canonical_hash = None;
    let mut canonical_hash_time = Duration::ZERO;
    let mut replay_stats = None;
    let work_included = match operation {
        Operation::ZstdDecompressOnly => "zstd decompress only",
        Operation::ZstdDecompressPlusParse => {
            let parse_start = Instant::now();
            let decoded = records::decode_i64_file(&decompressed)?;
            parse_time = parse_start.elapsed();
            record_count = decoded.rows.len();
            if canonical_hash_mode == CanonicalHashMode::Verify {
                let hash_start = Instant::now();
                let mut hash = canonical_hash_init();
                for row in &decoded.rows {
                    update_canonical_hash_row(&mut hash, row);
                }
                canonical_hash_time = hash_start.elapsed();
                canonical_hash = Some(hash);
            }
            "zstd decompress + parse rows"
        }
        Operation::ZstdDecompressPlusReplay => {
            let layout = records::aura1_fixed_layout_info(&decompressed)?;
            let replay_start = Instant::now();
            let mut hash = canonical_hash_init();
            let mut count = 0usize;
            records::visit_i64_rows_file(&decompressed, |row| {
                if canonical_hash_mode == CanonicalHashMode::Verify {
                    update_canonical_hash_row(&mut hash, row);
                }
                black_box(row);
                count += 1;
                Ok(())
            })?;
            replay_time = replay_start.elapsed();
            record_count = count;
            if canonical_hash_mode == CanonicalHashMode::Verify {
                canonical_hash_time = replay_time;
                canonical_hash = Some(hash);
            }
            replay_stats = Some(ReplayStats {
                record_width: layout.record_width,
                bytes_scanned: layout.body_bytes,
                callback_count: count,
                callback_time: replay_time,
                materialized_row_count: 0,
                materialized_column_count: 0,
                allocations: Some(0),
            });
            "zstd decompress + replay callback"
        }
        Operation::ZstdDecompressPlusAura1Output => {
            black_box(&decompressed);
            "zstd decompress + aura1 output bytes"
        }
        _ => unreachable!("zstd baseline operation"),
    };
    Ok(RunOutcome {
        record_count,
        output_bytes: decompressed_output_bytes,
        guard: decompressed_output_bytes as u64,
        canonical_hash,
        canonical_hash_time,
        canonical_hash_equality: None,
        guard_mode: "no_guard",
        transcode_path: "none",
        encoder_path: "none",
        conversion_plan_hash: None,
        compiled_plan_used: false,
        plan_setup_time: Duration::ZERO,
        output_byte_guard: None,
        post_process_duration: Duration::ZERO,
        timings: None,
        stats: None,
        replay_stats,
        zstd_stats: Some(ZstdStats {
            compressed_input_bytes: bytes.len(),
            decompressed_output_bytes,
            logical_record_count: record_count,
            work_included,
            parse_time,
            replay_time,
        }),
        fair_bytes_stats: None,
        preserved_output: None,
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

fn git_dirty() -> bool {
    std::process::Command::new("git")
        .arg("status")
        .arg("--short")
        .output()
        .ok()
        .is_none_or(|output| !output.status.success() || !output.stdout.is_empty())
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
        "usage: aura-bench --operation <parse-aura1|decode-aura0|transcode-aura1-to-aura0|transcode-aura0-to-aura1|aura1-scan-fixed|aura1-replay-callback|aura1-parse-to-rows|zstd-decompress-only|zstd-decompress-plus-parse|zstd-decompress-plus-replay|zstd-decompress-plus-aura1-output|aura0-to-aura1-bytes|aura0-to-aura1-bytes-verify|zstd-aura1-to-aura1-bytes|zstd-aura1-to-aura1-bytes-verify> --dataset <name> --input <path> [--iterations N] [--warmups N] [--format json|csv] [--output path] [--cache-mode warm|cold] [--guard-mode no_guard|fused_output_guard|old_post_output_guard|block_batched_output_guard] [--transcode-path auto|materialized|direct] [--decode-path materialized|cursor] [--encoder-path materialized|direct-streams|column-free] [--canonical-hash-mode none|verify] [--zstd-level N] [--reference-aura0 path] [--reference-aura1 path] [--preserve-output path] [--verify-output-decodes]"
    );
}
