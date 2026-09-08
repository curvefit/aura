//! Bounded Aura0 -> Aura1 and whole-file Zstd probe.
use anyhow::{bail, ensure, Context, Result};
use aura_codec::generic_planner::encode_generic_i64_events;
use aura_codec::instructions::{GenericGroupInstruction, GenericInstructionPlan};
use aura_codec::records::compile_i64_file;
use aura_codec::records::{
    decode_i64_events_file, decode_i64_file, decode_i64_file_metadata,
    encode_i64_events_profile_with_search, encode_ingest_i64_file, DecodedI64FileMetadata,
    I64Event, I64EventFileInput, I64FileInput,
};
use aura_codec::schema::{FieldScope, SchemaDescriptor};
use aura_codec::Profile;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Instant;
#[derive(Debug)]
struct Config {
    input: PathBuf,
    output: PathBuf,
    iterations: usize,
    warmups: usize,
    skip_creation: bool,
    profile_stages: bool,
    file_io: bool,
}
fn usage() {
    println!("usage: transmutation_probe --input <aura0> --output-dir <dir> [--iterations N] [--warmups N] [--skip-creation] [--profile-stages] [--file-io]");
}
fn config() -> Result<Option<Config>> {
    let mut a = std::env::args().skip(1);
    let mut input = None;
    let mut output = None;
    let mut iterations = 3;
    let mut warmups = 1;
    let mut skip_creation = false;
    let mut profile_stages = false;
    let mut file_io = false;
    while let Some(flag) = a.next() {
        let next = |a: &mut std::iter::Skip<std::env::Args>, f: &str| {
            a.next().with_context(|| format!("missing value for {f}"))
        };
        match flag.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(None);
            }
            "--input" => input = Some(PathBuf::from(next(&mut a, "--input")?)),
            "--output-dir" | "--out-dir" => {
                output = Some(PathBuf::from(next(&mut a, "--output-dir")?))
            }
            "--iterations" | "--repetitions" | "--reps" => {
                iterations = next(&mut a, "--iterations")?
                    .parse()
                    .context("iterations")?
            }
            "--warmups" => warmups = next(&mut a, "--warmups")?.parse().context("warmups")?,
            "--skip-creation" => skip_creation = true,
            "--profile-stages" => profile_stages = true,
            "--file-io" => file_io = true,
            _ => bail!("unknown argument {flag}; use --help"),
        }
    }
    if iterations == 0 {
        bail!("--iterations must be at least 1");
    }
    Ok(Some(Config {
        input: input.context("missing --input")?,
        output: output.context("missing --output-dir")?,
        iterations,
        warmups,
        skip_creation,
        profile_stages,
        file_io,
    }))
}
#[derive(Clone)]
struct Facts {
    schema: SchemaDescriptor,
    events: Option<Vec<I64Event>>,
    rows: Option<Vec<Vec<i64>>>,
    stream_id: u16,
    dictionary_id: u16,
    comment: String,
}
impl Facts {
    fn records(&self) -> usize {
        self.events
            .as_ref()
            .map(|e| e.iter().map(|x| x.children.len()).sum())
            .or_else(|| self.rows.as_ref().map(Vec::len))
            .unwrap_or(0)
    }
    fn consumer_count(&self) -> usize {
        self.events
            .as_ref()
            .map(Vec::len)
            .unwrap_or_else(|| self.records())
    }
    fn children(&self) -> Option<usize> {
        self.events
            .as_ref()
            .map(|e| e.iter().map(|x| x.children.len()).sum())
    }
}
fn plan(meta: &DecodedI64FileMetadata) -> Option<GenericInstructionPlan> {
    meta.ingest_footer
        .as_ref()
        .and_then(|f| f.generic_aura0_plan.clone())
        .or_else(|| {
            meta.compiled_footer
                .as_ref()
                .and_then(|f| f.generic_aura0_plan.clone())
        })
}
fn decode_source(
    bytes: &[u8],
) -> Result<(
    Facts,
    DecodedI64FileMetadata,
    Option<GenericInstructionPlan>,
)> {
    let meta = decode_i64_file_metadata(bytes).context("decode Aura0 metadata")?;
    if meta.header.profile != Profile::Aura0 {
        bail!("input profile is {:?}; expected Aura0", meta.header.profile);
    }
    let p = plan(&meta);
    let explicit = p.as_ref().is_some_and(|p| {
        p.groups
            .iter()
            .any(|g| matches!(g, GenericGroupInstruction::ExplicitEvents { .. }))
    });
    if explicit {
        let d = decode_i64_events_file(bytes).context("decode Aura0 events")?;
        Ok((
            Facts {
                schema: d.schema,
                events: Some(d.events),
                rows: None,
                stream_id: d.header.stream_id,
                dictionary_id: d.header.dictionary_id,
                comment: d.header.comment,
            },
            meta,
            p,
        ))
    } else {
        let d = decode_i64_file(bytes).context("decode Aura0 rows")?;
        Ok((
            Facts {
                schema: d.schema,
                events: None,
                rows: Some(d.rows),
                stream_id: d.header.stream_id,
                dictionary_id: d.header.dictionary_id,
                comment: d.header.comment,
            },
            meta,
            p,
        ))
    }
}
#[derive(Clone, Copy)]
struct Proc {
    user: u64,
    system: u64,
    rss: u64,
}
#[cfg(target_os = "linux")]
fn proc() -> Option<Proc> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let fields = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let user = fields.get(11)?.parse().ok()?;
    let system = fields.get(12)?.parse().ok()?;
    let rss = status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key != "VmRSS" {
            return None;
        }
        value.split_whitespace().next()?.parse::<u64>().ok()
    })? * 1024;
    Some(Proc { user, system, rss })
}
#[cfg(not(target_os = "linux"))]
fn proc() -> Option<Proc> {
    None
}
fn timed<T>(name: &str, work: impl FnOnce() -> Result<T>) -> Result<(T, Value)> {
    let before = proc();
    let start = Instant::now();
    let value = work()?;
    let wall_ns = start.elapsed().as_nanos();
    let after = proc();
    black_box(&value);
    let (u, s, rb, ra, rd) = match (before, after) {
        (Some(b), Some(a)) => (
            a.user.checked_sub(b.user),
            a.system.checked_sub(b.system),
            Some(b.rss),
            Some(a.rss),
            i64::try_from(i128::from(a.rss) - i128::from(b.rss)).ok(),
        ),
        _ => (None, None, None, None, None),
    };
    Ok((
        value,
        json!({"operation": name, "wall_ns": wall_ns, "cpu_user_ticks": u,
        "cpu_system_ticks": s, "rss_before_bytes": rb, "rss_after_bytes": ra,
        "rss_delta_bytes": rd}),
    ))
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn with_output(mut sample: Value, bytes: &[u8]) -> Value {
    sample["output_bytes"] = json!(bytes.len());
    sample["output_sha256"] = json!(hash(bytes));
    sample
}
fn runs<T>(
    warmups: usize,
    iterations: usize,
    mut work: impl FnMut() -> Result<T>,
) -> Result<(Vec<T>, Vec<T>)> {
    let total = warmups
        .checked_add(iterations)
        .context("sample count overflow")?;
    let mut all = Vec::with_capacity(total);
    for _ in 0..total {
        all.push(work()?);
    }
    Ok((all.split_off(warmups), all))
}
fn verify_facts(bytes: &[u8], facts: &Facts) -> Result<()> {
    if let Some(expected) = &facts.events {
        let decoded = decode_i64_events_file(bytes)?;
        ensure!(
            &decoded.events == expected,
            "event values or boundaries changed"
        );
        ensure!(decoded.schema == facts.schema, "event schema changed");
        ensure!(
            decoded.header.stream_id == facts.stream_id
                && decoded.header.dictionary_id == facts.dictionary_id
                && decoded.header.comment == facts.comment,
            "event metadata changed"
        );
    } else {
        let decoded = decode_i64_file(bytes)?;
        ensure!(
            Some(&decoded.rows) == facts.rows.as_ref(),
            "row values changed"
        );
        ensure!(decoded.schema == facts.schema, "row schema changed");
        ensure!(
            decoded.header.stream_id == facts.stream_id
                && decoded.header.dictionary_id == facts.dictionary_id
                && decoded.header.comment == facts.comment,
            "row metadata changed"
        );
    }
    Ok(())
}
fn decode_consumer(bytes: &[u8], facts: &Facts) -> Result<usize> {
    if facts.events.is_some() {
        let decoded = decode_i64_events_file(bytes)?;
        black_box(&decoded.schema);
        black_box(&decoded.header);
        for event in &decoded.events {
            black_box(event.children.len());
            for value in event
                .event_values
                .iter()
                .chain(event.children.iter().flatten())
            {
                black_box(*value);
            }
        }
        Ok(decoded.events.len())
    } else {
        let decoded = decode_i64_file(bytes)?;
        black_box(&decoded.schema);
        black_box(&decoded.header);
        for value in decoded.rows.iter().flatten() {
            black_box(*value);
        }
        Ok(decoded.rows.len())
    }
}
fn reconstruct(facts: &Facts) -> Result<Vec<u8>> {
    if let Some(events) = &facts.events {
        return encode_i64_events_profile_with_search(
            I64EventFileInput {
                schema: facts.schema.clone(),
                events: events.clone(),
                stream_id: facts.stream_id,
                dictionary_id: facts.dictionary_id,
                header_comment: Some(facts.comment.clone()),
            },
            Profile::Aura0,
            aura_codec::generic_planner::I64SearchEffort::Fast,
        )
        .context("reconstruct Aura0 events");
    }
    let ingest = encode_ingest_i64_file(I64FileInput {
        schema: facts.schema.clone(),
        rows: facts.rows.clone().context("decoded rows")?,
        stream_id: facts.stream_id,
        dictionary_id: facts.dictionary_id,
        header_comment: Some(facts.comment.clone()),
    })?;
    compile_i64_file(&ingest, Profile::Aura0).context("compile reconstructed Aura0")
}
fn creation(facts: &Facts, c: &Config) -> Result<Value> {
    let work = || timed("create_aura0", || reconstruct(facts)).map(|(b, s)| with_output(s, &b));
    let (samples, warm) = runs(c.warmups, c.iterations, work)?;
    let last = reconstruct(facts)?;
    verify_facts(&last, facts)?;
    let checked = true;
    Ok(
        json!({"api": if facts.events.is_some() {"encode_i64_events_profile_with_search(Aura0, Fast)"} else {"encode_ingest_i64_file + compile_i64_file(Aura0)"},
        "warmup_samples": warm, "samples": samples, "output_bytes": last.len(),
        "output_sha256": hash(&last), "output_semantics_match_input": checked}),
    )
}
fn transmutation_sample(input: &[u8], facts: &Facts) -> Result<(Value, Vec<u8>)> {
    let (output, compile_sample) = timed("compile_i64_file_aura1", || {
        Ok(compile_i64_file(input, Profile::Aura1)?)
    })?;
    let (count, validation_sample) = timed("decode_consumer", || decode_consumer(&output, facts))?;
    let valid = count == facts.consumer_count();
    let (count, pipeline_sample) = timed("compile_aura1_and_consume", || {
        let compiled = compile_i64_file(input, Profile::Aura1)?;
        decode_consumer(&compiled, facts)
    })?;
    if !valid || count != facts.consumer_count() {
        bail!("Aura1 consumer validation changed source facts");
    }
    let compile_ns = compile_sample["wall_ns"].as_u64().unwrap_or(0) as u128;
    let validation_ns = validation_sample["wall_ns"].as_u64().unwrap_or(0) as u128;
    let mut sample = json!({"compile": compile_sample, "validation": validation_sample,
        "compile_plus_validation_ns": compile_ns + validation_ns,
        "compile_and_consume": pipeline_sample, "validated_semantics": valid});
    sample = with_output(sample, &output);
    Ok((sample, output))
}
fn transmutation(input: &[u8], facts: &Facts, c: &Config) -> Result<(Value, Vec<u8>)> {
    let mut warm = Vec::with_capacity(c.warmups);
    for _ in 0..c.warmups {
        let (sample, _) = transmutation_sample(input, facts)?;
        warm.push(sample);
    }
    let mut samples = Vec::with_capacity(c.iterations);
    let mut last = None;
    for _ in 0..c.iterations {
        let (sample, output) = transmutation_sample(input, facts)?;
        samples.push(sample);
        last = Some(output);
    }
    let last = last.context("Aura1 output")?;
    verify_facts(&last, facts)?;
    Ok((
        json!({"compile_api": "compile_i64_file(input, Aura1)",
        "validation_api": if facts.events.is_some() {"decode_i64_events_file"} else {"decode_i64_file"},
        "warmup_samples": warm, "samples": samples, "output_bytes": last.len(),
        "output_sha256": hash(&last), "output_semantics_match_input": true}),
        last,
    ))
}
fn profile_stages(input: &[u8], facts: &Facts, c: &Config) -> Result<Value> {
    let Some(events) = &facts.events else {
        return Ok(
            json!({"supported": false, "reason": "flat Aura0 input has no explicit event stream"}),
        );
    };
    let values = events
        .iter()
        .map(|e| e.event_values.clone())
        .collect::<Vec<_>>();
    let children = events
        .iter()
        .map(|e| e.children.clone())
        .collect::<Vec<_>>();
    let decode_work = || {
        timed("decode_i64_events_file", || {
            Ok(decode_i64_events_file(input)?.events.len())
        })
        .and_then(|(count, sample)| {
            if count == events.len() {
                Ok(sample)
            } else {
                bail!("profiled event decode changed facts")
            }
        })
    };
    let (decode_samples, decode_warm) = runs(c.warmups, c.iterations, decode_work)?;
    let encode_work = || {
        timed("encode_generic_i64_events", || {
            Ok(encode_generic_i64_events(
                &facts.schema,
                &values,
                &children,
            )?)
        })
        .map(|(encoded, mut sample)| {
            sample["output_stream_count"] = json!(encoded.streams.len());
            sample["output_group_count"] = json!(encoded.plan.groups.len());
            sample["output_record_count"] = json!(encoded.record_count);
            sample
        })
    };
    let (encode_samples, encode_warm) = runs(c.warmups, c.iterations, encode_work)?;
    Ok(
        json!({"supported": true, "decode_api": "decode_i64_events_file",
        "encode_api": "encode_generic_i64_events", "warmup_decode_samples": decode_warm,
        "decode_samples": decode_samples, "warmup_encode_samples": encode_warm,
        "encode_samples": encode_samples}),
    )
}
fn zstd_sample(source: &[u8], level: i32, facts: &Facts) -> Result<(Value, Vec<u8>)> {
    let (compressed, compression) = timed("zstd_compress", || {
        zstd::stream::encode_all(Cursor::new(source), level).context("zstd compression")
    })?;
    let (decompressed, decompression) = timed("zstd_decompress", || {
        zstd::stream::decode_all(Cursor::new(&compressed)).context("zstd decompression")
    })?;
    if decompressed != source {
        bail!("Zstd round trip changed bytes");
    }
    let (count, consumer) = timed("zstd_output_consumer", || {
        decode_consumer(&decompressed, facts)
    })?;
    let valid = count == facts.consumer_count();
    let (count, end_to_end) = timed("zstd_decompress_and_consume", || {
        let bytes =
            zstd::stream::decode_all(Cursor::new(&compressed)).context("zstd decompression")?;
        decode_consumer(&bytes, facts)
    })?;
    if !valid || count != facts.consumer_count() {
        bail!("Zstd consumer validation changed facts");
    }
    let cn = compression["wall_ns"].as_u64().unwrap_or(0) as u128;
    let dn = decompression["wall_ns"].as_u64().unwrap_or(0) as u128;
    let mut sample = json!({"compression": compression, "decompression": decompression,
        "consumer": consumer, "decompression_plus_consumer_ns": dn + consumer["wall_ns"].as_u64().unwrap_or(0) as u128,
        "compression_plus_decompression_ns": cn + dn, "decompression_and_consume": end_to_end,
        "validated_semantics": valid, "compressed_bytes": compressed.len(), "compressed_sha256": hash(&compressed),
        "source_bytes": source.len()});
    sample["output_bytes"] = json!(compressed.len());
    Ok((sample, compressed))
}
fn zstd(
    source: &[u8],
    name: &'static str,
    ext: &'static str,
    stem: &str,
    facts: &Facts,
    c: &Config,
) -> Result<Value> {
    let mut reports = Vec::new();
    for level in [3_i32, 19_i32] {
        let mut warm = Vec::new();
        for _ in 0..c.warmups {
            warm.push(zstd_sample(source, level, facts)?.0);
        }
        let mut samples = Vec::new();
        let mut last = None;
        for _ in 0..c.iterations {
            let (sample, compressed) = zstd_sample(source, level, facts)?;
            samples.push(sample);
            last = Some(compressed);
        }
        let last = last.context("Zstd output")?;
        let path = c.output.join(format!("{stem}.{ext}.zst{level}"));
        fs::write(&path, &last).with_context(|| format!("write {}", path.display()))?;
        reports.push(
            json!({"representation": name, "level": level, "source_bytes": source.len(),
            "warmup_samples": warm, "samples": samples, "compressed_bytes": last.len(),
            "compressed_sha256": hash(&last), "output_path": path.display().to_string()}),
        );
    }
    Ok(Value::Array(reports))
}
fn file_io(c: &Config, aura1: &[u8]) -> Result<Value> {
    let compressed = zstd::stream::encode_all(Cursor::new(aura1), 3)?;
    let compressed_path = c.output.join("file-input.aura1.zst3");
    let output_path = c.output.join("file-output.aura1");
    fs::write(&compressed_path, compressed)?;
    let mut routes = Vec::new();
    for route in ["aura0", "zstd3"] {
        let work = || {
            let (_, sample) = timed(route, || {
                let input = fs::read(if route == "aura0" {
                    &c.input
                } else {
                    &compressed_path
                })?;
                let output = if route == "aura0" {
                    compile_i64_file(&input, Profile::Aura1)?
                } else {
                    zstd::stream::decode_all(Cursor::new(input))?
                };
                fs::write(&output_path, &output)?;
                black_box(output.len());
                Ok(())
            })?;
            Ok(sample)
        };
        let (samples, warmup_samples) = runs(c.warmups, c.iterations, work)?;
        ensure!(
            fs::read(&output_path)? == aura1,
            "file output differs from complete Aura1"
        );
        routes.push(
            json!({"route": route, "samples": samples, "warmup_samples": warmup_samples,
            "output_bytes_equal": true}),
        );
    }
    Ok(
        json!({"condition": "warm filesystem cache on staged files; whole-file reads and buffered writes including close; no fsync/cold-disk claim", "routes": routes}),
    )
}
fn debug_name<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}
fn main() -> Result<()> {
    let Some(c) = config()? else {
        return Ok(());
    };
    fs::create_dir_all(&c.output).with_context(|| format!("create {}", c.output.display()))?;
    let input = fs::read(&c.input).with_context(|| format!("read {}", c.input.display()))?;
    let (facts, meta, generic) = decode_source(&input)?;
    let stem = c
        .input
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("input");
    let field_count = facts.schema.fields.len();
    let event_fields = facts
        .schema
        .fields
        .iter()
        .filter(|f| f.scope == FieldScope::Event)
        .count();
    let repeated_fields = field_count - event_fields;
    let input_json = json!({"path": c.input.display().to_string(), "profile": "aura0", "bytes": input.len(), "sha256": hash(&input),
        "schema": {"schema_id": facts.schema.schema_id, "name": facts.schema.name, "encoding_version": debug_name(&facts.schema.encoding_version),
        "field_count": field_count, "event_field_count": event_fields, "repeated_field_count": repeated_fields},
        "counts": {"schema_count": 1, "field_count": field_count, "event_count": facts.events.as_ref().map(Vec::len),
            "child_count": facts.children(), "record_count": facts.records()},
        "byte_accounting": {"header_bytes": meta.header_len, "body_bytes": meta.footer_start - meta.header_len,
            "footer_bytes": meta.footer_len_offset - meta.footer_start, "trailer_bytes": input.len() - meta.footer_len_offset,
            "total_bytes": input.len(), "header_end": meta.header_len, "body_end": meta.footer_start, "footer_end": meta.footer_len_offset}});
    let plan_json = match generic.as_ref() {
        None => json!({"present": false}),
        Some(p) => {
            let mut names = p
                .streams
                .iter()
                .map(|s| debug_name(&s.op))
                .chain(p.groups.iter().map(debug_name))
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            json!({"present": true, "stream_count": p.streams.len(), "group_count": p.groups.len(),
                "op_names": names})
        }
    };
    let creation_json = if c.skip_creation {
        None
    } else {
        Some(creation(&facts, &c)?)
    };
    let (transmutation_json, aura1) = transmutation(&input, &facts, &c)?;
    let aura1_path = c.output.join(format!("{stem}.aura1"));
    fs::write(&aura1_path, &aura1).with_context(|| format!("write {}", aura1_path.display()))?;
    let stages_json = c
        .profile_stages
        .then(|| profile_stages(&input, &facts, &c))
        .transpose()?;
    let aura1_zstd = zstd(&aura1, "aura1_complete", "aura1", stem, &facts, &c)?;
    let aura0_zstd = zstd(&input, "aura0_residual", "aura0", stem, &facts, &c)?;
    let file_io_report = c.file_io.then(|| file_io(&c, &aura1)).transpose()?;
    let report_path = c.output.join("transmutation_probe.json");
    let report = json!({"tool": "aura-transmutation-probe", "version": env!("CARGO_PKG_VERSION"), "zstd_version": zstd::zstd_safe::version_string(),
        "configuration": {"warmups": c.warmups, "iterations": c.iterations, "skip_creation": c.skip_creation, "profile_stages": c.profile_stages},
        "input": input_json, "source_kind": if facts.events.is_some() {"explicit_events"} else {"flat_rows"}, "generic_plan": plan_json,
        "creation": creation_json, "transmutation": transmutation_json, "profile_stages": stages_json, "file_io": file_io_report,
        "zstd": [aura1_zstd, aura0_zstd], "outputs": {"report_path": report_path.display().to_string(), "aura1_path": aura1_path.display().to_string()},
        "notes": ["wall_ns is the timed API boundary; warmups and measured samples are retained",
            "cpu ticks and VmRSS are Linux /proc/self values when available",
            "rss is whole-process before/after state, not peak allocation; compile_and_consume and zstd_decompress_and_consume include decoded-object drop",
            "Zstd levels 3 and 19 save only final outputs under --output-dir"]});
    let bytes = serde_json::to_vec_pretty(&report).context("serialize report")?;
    fs::write(&report_path, &bytes).with_context(|| format!("write {}", report_path.display()))?;
    println!("{}", String::from_utf8(bytes).context("report UTF-8")?);
    Ok(())
}
