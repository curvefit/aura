use std::env;
use std::fs;
use std::hint::black_box;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use aura_codec::records::{self, I64Event};
use aura_codec::types::Profile;
use aura_codec::AuraReader;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const DEFAULT_ITERATIONS: usize = 3;
const DEFAULT_WARMUPS: usize = 1;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Decode,
    Consume,
    Open,
}

impl Mode {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "decode" => Ok(Self::Decode),
            "consume" => Ok(Self::Consume),
            "open" => Ok(Self::Open),
            other => bail!("unknown --mode '{other}'; expected decode|consume|open"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Consume => "consume",
            Self::Open => "open",
        }
    }
}

#[derive(Debug)]
struct Args {
    input: PathBuf,
    reference: PathBuf,
    iterations: usize,
    warmups: usize,
    mode: Mode,
    file_backed: bool,
}

fn usage() -> &'static str {
    "usage: aura-replay-bench --input AURA1 --reference AURA0 [--iterations N] [--warmups N] [--output JSON] [--mode decode|consume|open] [--file-backed]"
}

fn parse_args() -> Result<Args> {
    let mut input = None;
    let mut reference = None;
    let mut iterations = DEFAULT_ITERATIONS;
    let mut warmups = DEFAULT_WARMUPS;
    let mut mode = Mode::Decode;
    let mut file_backed = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(next_arg(&mut args, "--input")?)),
            "--reference" => reference = Some(PathBuf::from(next_arg(&mut args, "--reference")?)),
            "--iterations" => {
                iterations = next_arg(&mut args, "--iterations")?
                    .parse()
                    .with_context(|| "invalid --iterations value")?
            }
            "--warmups" => {
                warmups = next_arg(&mut args, "--warmups")?
                    .parse()
                    .with_context(|| "invalid --warmups value")?
            }
            "--mode" => mode = Mode::parse(&next_arg(&mut args, "--mode")?)?,
            "--output" => {
                let output = next_arg(&mut args, "--output")?;
                if !output.eq_ignore_ascii_case("json") {
                    bail!("only JSON output is supported; got --output {output}");
                }
            }
            "--file-backed" => file_backed = true,
            "--no-file-backed" => file_backed = false,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown argument '{other}'\n{}", usage()),
        }
    }
    if iterations == 0 {
        bail!("--iterations must be greater than zero");
    }
    Ok(Args {
        input: input.context("missing --input")?,
        reference: reference.context("missing --reference")?,
        iterations,
        warmups,
        mode,
        file_backed,
    })
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value for {flag}"))
}

fn median(values: &[u128]) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        sorted[middle - 1].saturating_add(sorted[middle]) / 2
    } else {
        sorted[middle]
    }
}

fn summary(values: &[u128]) -> Value {
    let middle = median(values);
    let min = *values.iter().min().unwrap();
    let max = *values.iter().max().unwrap();
    json!({
        "individual": values,
        "median": middle,
        "min": min,
        "max": max,
        "range": max.saturating_sub(min),
    })
}

#[derive(Debug)]
struct Corpus {
    input: Vec<u8>,
    reference: Vec<u8>,
    reference_events: Vec<I64Event>,
    input_sha256: String,
    reference_sha256: String,
    event_count: usize,
    child_count: usize,
    schema_fields: usize,
    schema_equal: bool,
    header_equal_except_profile: bool,
}

#[derive(Debug)]
struct Iteration {
    events: Option<Vec<I64Event>>,
    consume_digest: Option<u64>,
    wall_ns: u128,
    first_event_latency_ns: Option<u128>,
    cpu_ns: Option<u128>,
    rss_kib: Option<u64>,
    file_bytes: Option<Vec<u8>>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let corpus = load_corpus(&args)?;
    let mut warmup_digests = Vec::new();
    for _ in 0..args.warmups {
        let iteration = run_iteration(&args, &corpus)?;
        validate_iteration(&args, &corpus, &iteration)?;
        if let Some(digest) = iteration.consume_digest {
            warmup_digests.push(digest);
        }
    }

    let mut samples = Vec::with_capacity(args.iterations);
    let mut wall = Vec::with_capacity(args.iterations);
    let mut first_event = Vec::with_capacity(args.iterations);
    let mut cpu = Vec::with_capacity(args.iterations);
    let mut cpu_individual = Vec::with_capacity(args.iterations);
    let mut rss = Vec::with_capacity(args.iterations);
    let mut consume_digests = Vec::new();
    for _ in 0..args.iterations {
        let iteration = run_iteration(&args, &corpus)?;
        validate_iteration(&args, &corpus, &iteration)?;
        wall.push(iteration.wall_ns);
        first_event.extend(iteration.first_event_latency_ns);
        cpu_individual.push(iteration.cpu_ns);
        if let Some(value) = iteration.cpu_ns {
            cpu.push(value);
        }
        rss.push(iteration.rss_kib);
        if let Some(value) = iteration.consume_digest {
            consume_digests.push(value);
        }
        samples.push(json!({
            "wall_ns": iteration.wall_ns,
            "cpu_ns": iteration.cpu_ns,
            "rss_kib": iteration.rss_kib,
            "first_event_latency_ns": iteration.first_event_latency_ns,
            "events_per_sec": rate(corpus.event_count, iteration.wall_ns),
            "child_levels_per_sec": rate(corpus.child_count, iteration.wall_ns),
            "consume_digest": iteration.consume_digest,
        }));
    }

    let first_event_summary = (!first_event.is_empty()).then(|| summary(&first_event));
    let cpu_summary = (cpu.len() == args.iterations).then(|| summary(&cpu));
    let output = json!({
        "benchmark": "aura_replay_bench",
        "version": 1,
        "mode": args.mode.as_str(),
        "file_backed": args.file_backed,
        "warmups": args.warmups,
        "iterations": args.iterations,
        "input": file_info(&args.input, &corpus.input, "aura1", corpus.schema_fields, &corpus.input_sha256),
        "reference": file_info(&args.reference, &corpus.reference, "aura0", corpus.schema_fields, &corpus.reference_sha256),
        "workload": {
            "event_count": corpus.event_count,
            "child_level_count": corpus.child_count,
            "exact_event_equality": true,
            "schema_equal": corpus.schema_equal,
            "header_equal_except_profile": corpus.header_equal_except_profile,
            "equality_scope": "all event_values and all child values, checked against independently decoded Aura0 before and after every timed iteration",
        },
        "timings": {
            "wall_ns": summary(&wall),
            "cpu_ns": {
                "individual": cpu_individual,
                "summary": cpu_summary,
                "source": "/proc/self/schedstat running time; null when unavailable",
            },
            "first_event_latency_ns": first_event_summary,
            "first_event_latency_definition": if !matches!(args.mode, Mode::Open) { "decode end; full event materialization is complete before consume begins" } else { "not applicable to open-only mode" },
        },
        "samples": samples,
        "throughput": {
            "median_events_per_sec": rate(corpus.event_count, median(&wall)),
            "median_child_levels_per_sec": rate(corpus.child_count, median(&wall)),
        },
        "rss_kib": {
            "individual": rss,
            "peak_measured": rss.iter().filter_map(|value| *value).max(),
            "source": "/proc/self/status VmRSS sampled after each timed iteration",
            "scope": "includes retained input bytes and the independently decoded Aura0 reference event vectors used for post-timing equality checks",
        },
        "allocation_proxy": allocation_proxy(&corpus.reference_events),
        "consumer": {
            "warmup_digests": warmup_digests,
            "measured_digests": consume_digests,
            "digest_definition": "black_box fold over every event_value and every child value; liveness guard, not a semantic hash",
        },
        "code": build_metadata(),
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn load_corpus(args: &Args) -> Result<Corpus> {
    let input = fs::read(&args.input)
        .with_context(|| format!("read input Aura1 {}", args.input.display()))?;
    let reference = fs::read(&args.reference)
        .with_context(|| format!("read reference Aura0 {}", args.reference.display()))?;
    let input_decoded = records::decode_i64_events_file(&input)
        .context("decode input Aura1 for setup equality validation")?;
    let reference_decoded = records::decode_i64_events_file(&reference)
        .context("decode reference Aura0 for setup equality validation")?;
    if input_decoded.header.profile != Profile::Aura1 {
        bail!(
            "--input must be Aura1, got {:?}",
            input_decoded.header.profile
        );
    }
    if reference_decoded.header.profile != Profile::Aura0 {
        bail!(
            "--reference must be Aura0, got {:?}",
            reference_decoded.header.profile
        );
    }
    let schema_equal = input_decoded.schema == reference_decoded.schema;
    let header_equal_except_profile =
        headers_equal_except_profile(&input_decoded.header, &reference_decoded.header);
    if !schema_equal || !header_equal_except_profile {
        bail!("Aura1/Aura0 schema or header facts differ before benchmarking");
    }
    if input_decoded.events != reference_decoded.events {
        bail!("input Aura1 and reference Aura0 events differ before benchmarking");
    }
    let event_count = input_decoded.events.len();
    let child_count = input_decoded
        .events
        .iter()
        .try_fold(0usize, |total, event| {
            total.checked_add(event.children.len())
        })
        .context("child count overflow")?;
    let schema_fields = input_decoded.schema.fields.len();
    Ok(Corpus {
        input_sha256: sha256_hex(&input),
        reference_sha256: sha256_hex(&reference),
        input,
        reference,
        reference_events: reference_decoded.events,
        event_count,
        child_count,
        schema_fields,
        schema_equal,
        header_equal_except_profile,
    })
}

fn headers_equal_except_profile(a: &aura_codec::AuraHeader, b: &aura_codec::AuraHeader) -> bool {
    a.container_version == b.container_version
        && a.stream_id == b.stream_id
        && a.dictionary_id == b.dictionary_id
        && a.base_time_ns == b.base_time_ns
        && a.schema_mapping == b.schema_mapping
        && a.derived_expressions == b.derived_expressions
        && a.groups == b.groups
        && a.comment == b.comment
}

fn run_iteration(args: &Args, corpus: &Corpus) -> Result<Iteration> {
    let cpu_before = process_cpu_ns();
    let start = Instant::now();
    let file_bytes = if args.file_backed && !matches!(args.mode, Mode::Open) {
        Some(fs::read(&args.input).with_context(|| format!("read {}", args.input.display()))?)
    } else {
        None
    };
    let bytes = file_bytes.as_deref().unwrap_or(&corpus.input);
    let (events, consume_digest, first_event_latency_ns) = match args.mode {
        Mode::Open => {
            // Both maintained APIs perform complete validation; metadata-only parsing would
            // skip the explicit-event sidecar contract.
            let reader = if args.file_backed {
                AuraReader::open_path(&args.input).context("open input path")?
            } else {
                AuraReader::open_bytes(corpus.input.clone()).context("open input bytes")?
            };
            black_box(reader.profile());
            (None, None, None)
        }
        Mode::Decode => {
            let decoded = records::decode_i64_events_file(bytes).context("decode input events")?;
            let decode_end = start.elapsed().as_nanos();
            black_box(decoded.events.len());
            (Some(decoded.events), None, Some(decode_end))
        }
        Mode::Consume => {
            let decoded = records::decode_i64_events_file(bytes).context("decode input events")?;
            let decode_end = start.elapsed().as_nanos();
            let digest = consume_events(&decoded.events);
            (Some(decoded.events), Some(digest), Some(decode_end))
        }
    };
    let wall_ns = start.elapsed().as_nanos();
    let cpu_ns = process_cpu_ns()
        .zip(cpu_before)
        .map(|(after, before)| after.saturating_sub(before));
    Ok(Iteration {
        events,
        consume_digest,
        wall_ns,
        first_event_latency_ns,
        cpu_ns,
        rss_kib: process_rss_kib(),
        file_bytes,
    })
}

fn validate_iteration(args: &Args, corpus: &Corpus, iteration: &Iteration) -> Result<()> {
    if let Some(bytes) = &iteration.file_bytes {
        if bytes.as_slice() != corpus.input.as_slice() {
            bail!("input changed while benchmarking file-backed reads");
        }
    }
    if let Some(events) = &iteration.events {
        if events != &corpus.reference_events {
            bail!("timed input result differs from independent Aura0 events");
        }
        if events.len() != corpus.event_count {
            bail!("timed event count changed");
        }
    } else if !matches!(args.mode, Mode::Open) {
        bail!("mode {} did not materialize events", args.mode.as_str());
    }
    Ok(())
}

fn consume_events(events: &[I64Event]) -> u64 {
    let mut digest = 0xcbf29ce484222325;
    for event in events {
        for &value in &event.event_values {
            digest = fold_digest(digest, value);
            black_box(value);
        }
        for child in &event.children {
            for &value in child {
                digest = fold_digest(digest, value);
                black_box(value);
            }
        }
    }
    black_box(digest)
}

fn fold_digest(digest: u64, value: i64) -> u64 {
    digest
        .wrapping_mul(0x100000001b3)
        .wrapping_add(value as u64)
        .rotate_left(5)
}

fn allocation_proxy(events: &[I64Event]) -> Value {
    let mut event_values_bytes = 0usize;
    let mut children_outer_bytes = 0usize;
    let mut child_values_bytes = 0usize;
    let mut child_vec_count = 0usize;
    for event in events {
        event_values_bytes = event_values_bytes.saturating_add(
            event
                .event_values
                .capacity()
                .saturating_mul(size_of::<i64>()),
        );
        children_outer_bytes = children_outer_bytes.saturating_add(
            event
                .children
                .capacity()
                .saturating_mul(size_of::<Vec<i64>>()),
        );
        child_vec_count = child_vec_count.saturating_add(event.children.len());
        for child in &event.children {
            child_values_bytes = child_values_bytes
                .saturating_add(child.capacity().saturating_mul(size_of::<i64>()));
        }
    }
    json!({
        "kind": "retained_decoded_vec_capacity_estimate",
        "allocation_calls": null,
        "bytes_estimate": event_values_bytes.saturating_add(children_outer_bytes).saturating_add(child_values_bytes),
        "event_struct_bytes": events.len().saturating_mul(size_of::<I64Event>()),
        "event_values_buffer_bytes": event_values_bytes,
        "children_outer_buffer_bytes": children_outer_bytes,
        "child_values_buffer_bytes": child_values_bytes,
        "child_vec_count": child_vec_count,
        "vector_count_proxy": 1usize.saturating_add(events.len().saturating_mul(2)).saturating_add(child_vec_count),
        "caveat": "capacity-based retained materialization estimate; allocator calls and transient decoder buffers are unavailable",
    })
}

fn file_info(
    path: &Path,
    bytes: &[u8],
    profile: &str,
    schema_fields: usize,
    sha256: &str,
) -> Value {
    json!({
        "path": path.display().to_string(),
        "bytes": bytes.len(),
        "sha256": sha256,
        "profile": profile,
        "schema_fields": schema_fields,
    })
}

fn rate(count: usize, wall_ns: u128) -> f64 {
    if wall_ns == 0 {
        0.0
    } else {
        count as f64 * 1_000_000_000.0 / wall_ns as f64
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn process_cpu_ns() -> Option<u128> {
    fs::read_to_string("/proc/self/schedstat")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn process_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next() == Some("VmRSS:"))
            .then(|| parts.next()?.parse::<u64>().ok())
            .flatten()
    })
}

fn build_metadata() -> Value {
    json!({
        "rustc": option_env!("RUSTC_VERSION").or(option_env!("RUSTC")),
        "profile": option_env!("PROFILE"),
        "target": option_env!("TARGET"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "code_revision": env::var("AURA_BENCH_CODE_REVISION").ok().or_else(|| env::var("GIT_COMMIT").ok()),
    })
}
