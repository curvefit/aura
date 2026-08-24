use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use aura_codec::{
    arrow_rust_version, build_provenance, canonical_v3_batch_sha256,
    canonical_v3_event_batch_sha256, canonical_v3_schema_fingerprint, cargo_lock_sha256,
    compile_shadow_grouped_arrow_ipc, compile_v3_planned_flat, decode_shadow_arrow_ipc_batch,
    decode_shadow_grouped_arrow_ipc_batch, decode_v3_event_block, decode_v3_planned_flat,
    decode_v3_selected_flat, decode_v3_value_block, encode_shadow_arrow_ipc, parse_schema_json,
    DecodedV3SelectedFlat, SchemaEncodingVersion, ShadowEncodeResult, ShadowGroupedEncodeResult,
    ShadowGroupedProtocolLimits, ShadowProtocolLimits, V3FlatAura0Reader, V3FlatAura0Writer,
    V3FlatLimits, V3FlatWriteSummary, V3FlatWriterOptions, V3GroupedAura0Reader,
    V3GroupedAura0Writer, V3GroupedWriteSummary, V3GroupedWriterOptions, V3PlannedFlatArtifact,
    V3PlannedFlatSummary, V3ValueLimits, DEFAULT_V3_FLAT_IN_MEMORY_BODY_BYTES,
    MAX_SCHEMA_JSON_BYTES, MAX_V3_EVENT_BLOCK_BYTES, MAX_V3_FLAT_FOOTER_BYTES,
    MAX_V3_FLAT_SCHEMA_BYTES, MAX_V3_GROUPED_FOOTER_BYTES, MAX_V3_PLANNED_FLAT_FOOTER_BYTES,
    MAX_V3_VALUE_BLOCK_BYTES, SHADOW_ARROW_PROTOCOL, SHADOW_ARTIFACT_KIND, SHADOW_ARTIFACT_KIND_V2,
    SHADOW_HANDSHAKE_SCHEMA, SHADOW_PROTOCOL, SHADOW_PROTOCOL_V2, SHADOW_RESULT_SCHEMA,
    SHADOW_RESULT_SCHEMA_V2, SHADOW_SCHEMA_FORMAT, SHADOW_VERIFY_RESULT_SCHEMA,
    SHADOW_VERIFY_RESULT_SCHEMA_V2, V3_FLAT_BODY_ENCODING_EXACT_BLOCKS,
    V3_FLAT_FOOTER_LAYOUT_VERSION, V3_GROUPED_BODY_ENCODING_EXACT_EVENTS,
    V3_GROUPED_BODY_LAYOUT_VERSION, V3_GROUPED_EVENT_BLOCK_VERSION,
    V3_GROUPED_FOOTER_LAYOUT_VERSION, V3_PLANNED_FLAT_BLOCK_VERSION, V3_PLANNED_FLAT_BODY_ENCODING,
    V3_PLANNED_FLAT_BODY_LAYOUT_VERSION, V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const HELP: &str = "Aura developer CLI

Usage:
  aura schema validate --input <path> [--json]
  aura schema inspect --input <path> --json
  aura schema canonicalize --input <path> [--output <path>]
  aura shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
  aura shadow encode --protocol aura-logical-arrow-ipc-v1 --schema <path> \
    --artifact-kind standalone-aura-v3-value-block-v1 --output <path> --json
  aura shadow verify --protocol aura-logical-arrow-ipc-v1 --schema <path> \
    --input <existing.aurav3vb> --json
  aura shadow encode --protocol aura-logical-arrow-ipc-v2 --schema <path> \
    --artifact-kind standalone-aura-v3-event-block-v1 --output <path.aurav3eb> --json
  aura shadow verify --protocol aura-logical-arrow-ipc-v2 --schema <path> \
    --input <existing.aurav3eb> --json
  aura v3 aura0 seal --protocol aura-logical-arrow-ipc-v1 --schema <canonical.json> \
    --output <new.aura0> --json
  aura v3 aura0 seal --protocol aura-logical-arrow-ipc-v1 --mode planned \
    --schema <canonical.json> --output <new.aura0> --json
  aura v3 aura0 seal --protocol aura-logical-arrow-ipc-v2 --schema <canonical.json> \
    --output <new.aura0> --json
  aura v3 aura0 verify --input <file.aura0> --json
  aura --help
";

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowProtocolVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum V3SealMode {
    #[default]
    Exact,
    Planned,
}

#[derive(Debug)]
struct CliError(String);

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let shadow_error = args.first().is_some_and(|value| value == "shadow");
            let v3_error = args.first().is_some_and(|value| value == "v3");
            let schema_inspect_error = matches!(
                args.as_slice(),
                [namespace, command, ..] if namespace == "schema" && command == "inspect"
            );
            let (error_code, error_text) = if shadow_error {
                shadow_safe_error(&error.0)
            } else if v3_error {
                v3_safe_error(&error.0)
            } else {
                ("schema_command_failed", bounded_error(&error.0))
            };
            if schema_inspect_error {
                eprintln!(
                    "{{\"error_schema\":\"aura-schema-inspect-error-v1\",\"valid\":false,\"code\":\"schema_inspect_failed\",\"error\":\"schema inspection failed\"}}"
                );
            } else if args.iter().any(|arg| arg == "--json") {
                let message = serde_json::to_string(&error_text)
                    .unwrap_or_else(|_| "\"schema command failed\"".to_owned());
                if shadow_error {
                    eprintln!("{{\"error_schema\":\"aura-shadow-error-v1\",\"code\":\"{error_code}\",\"error\":{message}}}");
                } else if v3_error {
                    eprintln!("{{\"error_schema\":\"aura-v3-flat-error-v1\",\"code\":\"{error_code}\",\"error\":{message}}}");
                } else {
                    eprintln!("{{\"valid\":false,\"error\":{message}}}");
                }
            } else {
                eprintln!("error: {error_text}");
            }
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if matches!(args.first().map(String::as_str), Some("v3")) {
        let [_, profile, command, options @ ..] = args else {
            return Err(CliError(format!("invalid command\n\n{HELP}")));
        };
        if profile != "aura0" {
            return Err(CliError(
                "unsupported v3 profile; expected aura0".to_owned(),
            ));
        }
        return match command.as_str() {
            "seal" => v3_aura0_seal_command(options),
            "verify" => v3_aura0_verify_command(options),
            _ => Err(CliError(
                "unsupported v3 aura0 command; expected seal or verify".to_owned(),
            )),
        };
    }
    if let Some(namespace) = args.first() {
        if namespace != "schema" && namespace != "shadow" {
            return Err(CliError(format!(
                "unsupported command {namespace:?}; expected schema or shadow"
            )));
        }
    }
    let [namespace, command, options @ ..] = args else {
        return Err(CliError(format!("invalid command\n\n{HELP}")));
    };
    if command == "--help" || command == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    match (namespace.as_str(), command.as_str()) {
        ("schema", "validate") => validate_command(options),
        ("schema", "inspect") => inspect_command(options),
        ("schema", "canonicalize") => canonicalize_command(options),
        ("shadow", "handshake") => shadow_handshake_command(options),
        ("shadow", "encode") => shadow_encode_command(options),
        ("shadow", "verify") => shadow_verify_command(options),
        ("schema", _) => Err(CliError(format!(
            "unsupported schema command {command:?}; expected validate, inspect, or canonicalize"
        ))),
        ("shadow", _) => Err(CliError(format!(
            "unsupported shadow command {command:?}; expected handshake, encode, or verify"
        ))),
        _ => unreachable!(),
    }
}

fn shadow_handshake_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow handshake option".to_owned())),
    })?;
    require_shadow_protocol(protocol)?;
    if !json_output {
        return Err(CliError("shadow handshake requires --json".to_owned()));
    }
    let provenance = build_provenance();
    let value = json!({
        "handshake_schema": SHADOW_HANDSHAKE_SCHEMA,
        "package": env!("CARGO_PKG_NAME"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "git_commit": provenance.git_commit,
        "dirty": provenance.dirty,
        "provenance_source": provenance.source,
        "protocols": [SHADOW_PROTOCOL, SHADOW_PROTOCOL_V2],
        "schema_formats": [SHADOW_SCHEMA_FORMAT],
        "artifact_kinds": [SHADOW_ARTIFACT_KIND, SHADOW_ARTIFACT_KIND_V2],
        "operations": ["encode", "verify", "v3-aura0-seal", "v3-aura0-verify"],
        "complete_container_targets": ["flat-aura0-v3-v1", "grouped-aura0-v3-exact-v1"],
        "hash_contracts": {
            "schema_fingerprint": "sha256:aura-v3-schema-fingerprint-v1",
            "logical_values": "sha256:aura-v3-canonical-exact-values-v1",
            "logical_events": "sha256:aura-v3-canonical-exact-events-v1",
            "artifact": "sha256"
        },
        "arrow": {
            "rust_version": arrow_rust_version(),
            "protocol": SHADOW_ARROW_PROTOCOL
        },
        "arrow_crate_version": arrow_rust_version(),
        "cargo_lock_sha256": hex(&cargo_lock_sha256())
    });
    write_json_stdout(&value)
}

fn shadow_encode_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut schema_path = None;
    let mut artifact_kind = None;
    let mut output = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--schema" => set_path(&mut schema_path, value, "--schema"),
        "--artifact-kind" => set_string(&mut artifact_kind, value, "--artifact-kind"),
        "--output" => set_path(&mut output, value, "--output"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow encode option".to_owned())),
    })?;
    let protocol = require_shadow_protocol(protocol)?;
    let (expected_kind, expected_extension) = match protocol {
        ShadowProtocolVersion::V1 => (SHADOW_ARTIFACT_KIND, "aurav3vb"),
        ShadowProtocolVersion::V2 => (SHADOW_ARTIFACT_KIND_V2, "aurav3eb"),
    };
    if artifact_kind.as_deref() != Some(expected_kind) {
        return Err(CliError(
            "unsupported or missing --artifact-kind".to_owned(),
        ));
    }
    if !json_output {
        return Err(CliError("shadow encode requires --json".to_owned()));
    }
    let schema_path = schema_path.ok_or_else(|| CliError("missing --schema".to_owned()))?;
    let output = output.ok_or_else(|| CliError("missing --output".to_owned()))?;
    if schema_path == Path::new("-") || output == Path::new("-") {
        return Err(CliError(
            "shadow schema and output must be filesystem paths".to_owned(),
        ));
    }
    if output.extension().and_then(|value| value.to_str()) != Some(expected_extension) {
        return Err(CliError(format!(
            "shadow output must use the .{expected_extension} extension"
        )));
    }
    require_absent_output(&output)?;
    let schema_text = read_bounded_schema(&schema_path)?;
    let schema = parse_schema_json(&schema_text).map_err(|error| CliError(error.to_string()))?;
    let (publication, value) = match protocol {
        ShadowProtocolVersion::V1 => {
            let result = encode_shadow_arrow_ipc(
                &schema,
                io::stdin().lock(),
                ShadowProtocolLimits::default(),
            )
            .map_err(|error| CliError(error.to_string()))?;
            let publication = publish_verified_block(&output, &schema, &result)?;
            let provenance = build_provenance();
            let value = json!({
                "result_schema": SHADOW_RESULT_SCHEMA,
                "protocol": SHADOW_PROTOCOL,
                "artifact_kind": SHADOW_ARTIFACT_KIND,
                "complete_aura_file": false,
                "reference_block_version": 1,
                "package_version": env!("CARGO_PKG_VERSION"),
                "schema_id": schema.schema_id,
                "schema_fingerprint_sha256": hex(&result.schema_fingerprint),
                "row_count": result.row_count,
                "logical_sha256": hex(&result.logical_sha256),
                "artifact_bytes": result.block.len(),
                "artifact_sha256": hex(&result.block_sha256),
                "stale_temp_cleanup_required": publication.stale_temp_cleanup_required,
                "build": build_json(provenance)
            });
            (publication, value)
        }
        ShadowProtocolVersion::V2 => {
            let result = compile_shadow_grouped_arrow_ipc(
                &schema,
                io::stdin().lock(),
                ShadowGroupedProtocolLimits::default(),
            )
            .map_err(|error| CliError(error.to_string()))?;
            let publication = publish_verified_grouped_block(&output, &schema, &result)?;
            let provenance = build_provenance();
            let value = json!({
                "result_schema": SHADOW_RESULT_SCHEMA_V2,
                "protocol": SHADOW_PROTOCOL_V2,
                "artifact_kind": SHADOW_ARTIFACT_KIND_V2,
                "complete_aura_file": false,
                "reference_block_version": 1,
                "package_version": env!("CARGO_PKG_VERSION"),
                "schema_id": schema.schema_id,
                "schema_fingerprint_sha256": hex(&result.schema_fingerprint),
                "event_count": result.event_count,
                "child_count": result.child_count,
                "logical_sha256": hex(&result.logical_sha256),
                "artifact_bytes": result.block.len(),
                "artifact_sha256": hex(&result.block_sha256),
                "stale_temp_cleanup_required": publication.stale_temp_cleanup_required,
                "build": build_json(provenance)
            });
            (publication, value)
        }
    };
    let _ = publication;
    write_json_stdout(&value)
        .map_err(|_| CliError("publication committed result unavailable".to_owned()))
}

fn shadow_verify_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut schema_path = None;
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--schema" => set_path(&mut schema_path, value, "--schema"),
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow verify option".to_owned())),
    })?;
    let protocol = require_shadow_protocol(protocol)?;
    if !json_output {
        return Err(CliError("shadow verify requires --json".to_owned()));
    }
    let schema_path = schema_path.ok_or_else(|| CliError("missing --schema".to_owned()))?;
    let input = input.ok_or_else(|| CliError("missing --input".to_owned()))?;
    if schema_path == Path::new("-") || input == Path::new("-") {
        return Err(CliError(
            "shadow schema and input must be filesystem paths".to_owned(),
        ));
    }
    let expected_extension = match protocol {
        ShadowProtocolVersion::V1 => "aurav3vb",
        ShadowProtocolVersion::V2 => "aurav3eb",
    };
    if input.extension().and_then(|value| value.to_str()) != Some(expected_extension) {
        return Err(CliError(format!(
            "shadow input must use the .{expected_extension} extension"
        )));
    }
    let schema_text = read_bounded_schema(&schema_path)?;
    let schema = parse_schema_json(&schema_text).map_err(|error| CliError(error.to_string()))?;
    let value = match protocol {
        ShadowProtocolVersion::V1 => verify_shadow_v1_value(&schema, &input)?,
        ShadowProtocolVersion::V2 => verify_shadow_v2_value(&schema, &input)?,
    };
    write_json_stdout(&value)
}

fn v3_aura0_seal_command(args: &[String]) -> Result<(), CliError> {
    let mut protocol = None;
    let mut mode = None;
    let mut schema_path = None;
    let mut output = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--mode" => set_string(&mut mode, value, "--mode"),
        "--schema" => set_path(&mut schema_path, value, "--schema"),
        "--output" => set_path(&mut output, value, "--output"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown v3 aura0 seal option".to_owned())),
    })?;
    let protocol = require_shadow_protocol(protocol)?;
    let mode = require_v3_seal_mode(mode)?;
    if protocol == ShadowProtocolVersion::V2 && mode == V3SealMode::Planned {
        return Err(CliError(
            "planned mode is unsupported for grouped protocol v2".to_owned(),
        ));
    }
    if !json_output {
        return Err(CliError("v3 aura0 seal requires --json".to_owned()));
    }
    let schema_path = schema_path.ok_or_else(|| CliError("missing --schema".to_owned()))?;
    let output = output.ok_or_else(|| CliError("missing --output".to_owned()))?;
    if schema_path == Path::new("-") || output == Path::new("-") {
        return Err(CliError(
            "v3 schema and output must be filesystem paths".to_owned(),
        ));
    }
    if output.extension().and_then(|value| value.to_str()) != Some("aura0") {
        return Err(CliError(
            "v3 output must use the .aura0 extension".to_owned(),
        ));
    }
    require_absent_output(&output)?;
    let schema_text = read_bounded_schema(&schema_path)?;
    let schema = parse_schema_json(&schema_text).map_err(|error| CliError(error.to_string()))?;
    if schema
        .to_canonical_json()
        .map_err(|error| CliError(error.to_string()))?
        != schema_text
    {
        return Err(CliError("v3 schema must be canonical JSON".to_owned()));
    }
    let (publication, value) = match protocol {
        ShadowProtocolVersion::V1 => {
            let batch = decode_shadow_arrow_ipc_batch(
                &schema,
                io::stdin().lock(),
                ShadowProtocolLimits::default(),
            )
            .map_err(|error| CliError(error.to_string()))?;
            match mode {
                V3SealMode::Exact => {
                    let (publication, summary, artifact_sha256) =
                        publish_verified_v3_flat(&output, &schema, &batch)?;
                    let value = flat_v3_seal_json(
                        &schema,
                        &summary,
                        artifact_sha256,
                        publication.stale_temp_cleanup_required,
                    );
                    (publication, value)
                }
                V3SealMode::Planned => {
                    let artifact = if batch.row_count == 0 {
                        compile_v3_planned_flat(&schema, &[], planned_cli_limits())
                    } else {
                        compile_v3_planned_flat(
                            &schema,
                            std::slice::from_ref(&batch),
                            planned_cli_limits(),
                        )
                    }
                    .map_err(|_| CliError("could not compile planned v3 output".to_owned()))?;
                    let selection = planned_flat_selection(&artifact)?;
                    let (publication, artifact_sha256) =
                        publish_verified_v3_planned(&output, &artifact, selection)?;
                    let value = planned_flat_v3_seal_json(
                        &schema,
                        &artifact,
                        selection,
                        artifact_sha256,
                        publication.stale_temp_cleanup_required,
                    );
                    (publication, value)
                }
            }
        }
        ShadowProtocolVersion::V2 => {
            let batch = decode_shadow_grouped_arrow_ipc_batch(
                &schema,
                io::stdin().lock(),
                ShadowGroupedProtocolLimits::default(),
            )
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("schema") {
                    CliError(message)
                } else {
                    CliError("grouped v3 invalid ipc".to_owned())
                }
            })?;
            let (publication, summary, artifact_sha256) =
                publish_verified_v3_grouped(&output, &schema, &batch)?;
            let value = grouped_v3_seal_json(
                &schema,
                &summary,
                artifact_sha256,
                publication.stale_temp_cleanup_required,
            );
            (publication, value)
        }
    };
    write_json_stdout(&value).map_err(|_| {
        if publication.stale_temp_cleanup_required {
            CliError("publication_committed_result_unavailable_cleanup_required".to_owned())
        } else {
            CliError("publication committed result unavailable".to_owned())
        }
    })
}

fn v3_aura0_verify_command(args: &[String]) -> Result<(), CliError> {
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown v3 aura0 verify option".to_owned())),
    })?;
    if !json_output {
        return Err(CliError("v3 aura0 verify requires --json".to_owned()));
    }
    let input = input.ok_or_else(|| CliError("missing --input".to_owned()))?;
    if input == Path::new("-")
        || input.extension().and_then(|value| value.to_str()) != Some("aura0")
    {
        return Err(CliError("v3 input path rejected".to_owned()));
    }
    let metadata =
        fs::symlink_metadata(&input).map_err(|_| CliError("v3 input path rejected".to_owned()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError("v3 input path rejected".to_owned()));
    }
    let mut file = open_v3_input(&input, &metadata)?;
    let kind = inspect_v3_footer_kind(&mut file)?;
    let value = match kind {
        CompleteV3Kind::Flat => verify_v3_flat_value(file)?,
        CompleteV3Kind::PlannedFlat => verify_v3_planned_flat_value(file)?,
        CompleteV3Kind::Grouped => verify_v3_grouped_value(file)?,
    };
    write_json_stdout(&value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompleteV3Kind {
    Flat,
    PlannedFlat,
    Grouped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedFlatSelection {
    Exact,
    Planned,
}

impl PlannedFlatSelection {
    const fn footer_layout_version(self) -> u16 {
        match self {
            Self::Exact => V3_FLAT_FOOTER_LAYOUT_VERSION,
            Self::Planned => V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION,
        }
    }

    const fn body_encoding_code(self) -> u8 {
        match self {
            Self::Exact => V3_FLAT_BODY_ENCODING_EXACT_BLOCKS,
            Self::Planned => V3_PLANNED_FLAT_BODY_ENCODING,
        }
    }

    const fn body_encoding_name(self) -> &'static str {
        match self {
            Self::Exact => "flat_exact_blocks_v1",
            Self::Planned => "planned_flat_codecs_v1",
        }
    }

    const fn container_target(self) -> &'static str {
        match self {
            Self::Exact => "flat-aura0-v3-v1",
            Self::Planned => "flat-aura0-v3-planned-v1",
        }
    }
}

const MAX_PLANNED_CLI_FILE_BYTES: u64 = DEFAULT_V3_FLAT_IN_MEMORY_BODY_BYTES
    + MAX_V3_PLANNED_FLAT_FOOTER_BYTES as u64
    + MAX_V3_FLAT_SCHEMA_BYTES as u64
    + 4096;

fn planned_cli_limits() -> V3FlatLimits {
    V3FlatLimits::DEFAULT_IN_MEMORY
}

fn planned_flat_selection(
    artifact: &V3PlannedFlatArtifact,
) -> Result<PlannedFlatSelection, CliError> {
    let bytes = &artifact.bytes;
    if bytes.len() < 21 || bytes.get(bytes.len() - 8..) != Some(b"sealed:)".as_slice()) {
        return Err(CliError(
            "planned v3 compiler returned invalid artifact".to_owned(),
        ));
    }
    let trailer = bytes
        .get(bytes.len() - 12..bytes.len() - 8)
        .and_then(|value| <[u8; 4]>::try_from(value).ok())
        .ok_or_else(|| CliError("planned v3 compiler returned invalid artifact".to_owned()))?;
    let footer_len = usize::try_from(u32::from_le_bytes(trailer))
        .map_err(|_| CliError("planned v3 compiler returned invalid artifact".to_owned()))?;
    let footer_start = bytes
        .len()
        .checked_sub(12)
        .and_then(|value| value.checked_sub(footer_len))
        .ok_or_else(|| CliError("planned v3 compiler returned invalid artifact".to_owned()))?;
    let footer = bytes
        .get(footer_start..)
        .ok_or_else(|| CliError("planned v3 compiler returned invalid artifact".to_owned()))?;
    let tuple = footer
        .get(..9)
        .ok_or_else(|| CliError("planned v3 compiler returned invalid artifact".to_owned()))?;
    if &tuple[..4] != b"AURP" || u16::from_le_bytes([tuple[4], tuple[5]]) != 3 {
        return Err(CliError(
            "planned v3 compiler returned invalid artifact".to_owned(),
        ));
    }
    let selection = match (u16::from_le_bytes([tuple[6], tuple[7]]), tuple[8]) {
        (V3_FLAT_FOOTER_LAYOUT_VERSION, V3_FLAT_BODY_ENCODING_EXACT_BLOCKS) => {
            PlannedFlatSelection::Exact
        }
        (V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION, V3_PLANNED_FLAT_BODY_ENCODING) => {
            PlannedFlatSelection::Planned
        }
        _ => {
            return Err(CliError(
                "planned v3 compiler returned invalid artifact".to_owned(),
            ))
        }
    };
    let selected_candidate = artifact
        .inspection
        .candidates
        .iter()
        .find(|candidate| candidate.selected)
        .ok_or_else(|| CliError("planned v3 compiler omitted selection".to_owned()))?;
    if (selection == PlannedFlatSelection::Exact)
        != (selected_candidate.candidate_id == "exact-flat")
    {
        return Err(CliError(
            "planned v3 compiler selection mismatch".to_owned(),
        ));
    }
    Ok(selection)
}

fn inspect_v3_footer_kind(file: &mut File) -> Result<CompleteV3Kind, CliError> {
    let file_len = file
        .metadata()
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?
        .len();
    if file_len < 24 {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    file.seek(SeekFrom::End(-12))
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let mut trailer = [0u8; 12];
    file.read_exact(&mut trailer)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    if &trailer[4..] != b"sealed:)" {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    let footer_len = u64::from(u32::from_le_bytes(trailer[..4].try_into().unwrap()));
    let max_footer = MAX_V3_FLAT_FOOTER_BYTES
        .max(MAX_V3_PLANNED_FLAT_FOOTER_BYTES)
        .max(MAX_V3_GROUPED_FOOTER_BYTES) as u64;
    if !(12..=max_footer).contains(&footer_len) {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    let footer_start = file_len
        .checked_sub(12)
        .and_then(|value| value.checked_sub(footer_len))
        .ok_or_else(|| CliError("v3 invalid artifact".to_owned()))?;
    file.seek(SeekFrom::Start(footer_start))
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let mut tuple = [0u8; 12];
    file.read_exact(&mut tuple)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    if &tuple[..4] != b"AURP"
        || u16::from_le_bytes(tuple[4..6].try_into().unwrap()) != 3
        || tuple[9..12] != [0, 0, 0]
    {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    let layout = u16::from_le_bytes(tuple[6..8].try_into().unwrap());
    match (layout, tuple[8]) {
        (V3_FLAT_FOOTER_LAYOUT_VERSION, V3_FLAT_BODY_ENCODING_EXACT_BLOCKS) => {
            Ok(CompleteV3Kind::Flat)
        }
        (V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION, V3_PLANNED_FLAT_BODY_ENCODING) => {
            Ok(CompleteV3Kind::PlannedFlat)
        }
        (V3_GROUPED_FOOTER_LAYOUT_VERSION, V3_GROUPED_BODY_ENCODING_EXACT_EVENTS) => {
            Ok(CompleteV3Kind::Grouped)
        }
        _ => Err(CliError("v3 invalid artifact".to_owned())),
    }
}

fn flat_v3_seal_json(
    schema: &aura_codec::SchemaDescriptor,
    summary: &V3FlatWriteSummary,
    artifact_sha256: [u8; 32],
    stale_temp_cleanup_required: bool,
) -> serde_json::Value {
    let provenance = build_provenance();
    json!({
        "result_schema": "aura-v3-flat-aura0-seal-result-v1",
        "protocol": SHADOW_PROTOCOL,
        "complete_aura_file": true,
        "container_target": "flat-aura0-v3-v1",
        "container_version": 3,
        "profile": "aura0",
        "body_encoding": "flat_exact_blocks_v1",
        "footer_layout_version": V3_FLAT_FOOTER_LAYOUT_VERSION,
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&summary.schema_fingerprint),
        "row_count": summary.record_count,
        "chunk_count": summary.chunk_count,
        "body_bytes": summary.body_bytes,
        "footer_bytes": summary.footer_bytes,
        "file_bytes": summary.file_bytes,
        "logical_sha256": hex(&summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "stale_temp_cleanup_required": stale_temp_cleanup_required,
        "build": {
            "git_commit": provenance.git_commit,
            "dirty": provenance.dirty,
            "provenance_source": provenance.source,
            "arrow_crate_version": arrow_rust_version(),
            "cargo_lock_sha256": hex(&cargo_lock_sha256())
        }
    })
}

fn planned_flat_v3_seal_json(
    schema: &aura_codec::SchemaDescriptor,
    artifact: &V3PlannedFlatArtifact,
    selection: PlannedFlatSelection,
    artifact_sha256: [u8; 32],
    stale_temp_cleanup_required: bool,
) -> serde_json::Value {
    let provenance = build_provenance();
    let selected_candidate = artifact
        .inspection
        .candidates
        .iter()
        .find(|candidate| candidate.selected)
        .map(|candidate| candidate.candidate_id.as_str())
        .unwrap_or("unavailable");
    let candidates = artifact
        .inspection
        .candidates
        .iter()
        .map(|candidate| {
            json!({
                "candidate_id": candidate.candidate_id,
                "applicable": candidate.applicable,
                "complete_bytes": candidate.complete_bytes,
                "selected": candidate.selected,
                "rejection": candidate.rejection,
            })
        })
        .collect::<Vec<_>>();
    let codecs = artifact
        .inspection
        .codecs
        .iter()
        .map(|codec| {
            let selected_physical_codec = match codec.selected {
                aura_codec::PlanV2PhysicalCodec::FixedWidth => "fixed_width",
                aura_codec::PlanV2PhysicalCodec::UnsignedUleb128 => "unsigned_uleb128",
                aura_codec::PlanV2PhysicalCodec::SignedZigZagUleb128 => "signed_zigzag_uleb128",
            };
            json!({
                "slot": codec.slot,
                "logical_field_type": codec.field_type.name(),
                "fixed_bytes": codec.fixed_bytes,
                "varint_bytes": codec.varint_bytes,
                "selected_physical_codec": selected_physical_codec,
            })
        })
        .collect::<Vec<_>>();
    let plan_sha256 =
        (selection == PlannedFlatSelection::Planned).then(|| hex(&artifact.summary.plan_sha256));
    let body_layout_version =
        (selection == PlannedFlatSelection::Planned).then_some(V3_PLANNED_FLAT_BODY_LAYOUT_VERSION);
    let block_version =
        (selection == PlannedFlatSelection::Planned).then_some(V3_PLANNED_FLAT_BLOCK_VERSION);
    json!({
        "result_schema": "aura-v3-flat-aura0-planned-request-seal-result-v1",
        "protocol": SHADOW_PROTOCOL,
        "complete_aura_file": true,
        "container_target": selection.container_target(),
        "container_version": 3,
        "profile": "aura0",
        "requested_mode": "planned",
        "planner_requested": true,
        "selected_candidate": selected_candidate,
        "planner_candidates": candidates,
        "physical_codecs": codecs,
        "body_encoding": selection.body_encoding_name(),
        "body_encoding_code": selection.body_encoding_code(),
        "body_layout_version": body_layout_version,
        "block_version": block_version,
        "footer_layout_version": selection.footer_layout_version(),
        "plan_sha256": plan_sha256,
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&artifact.summary.schema_fingerprint),
        "row_count": artifact.summary.row_count,
        "chunk_count": artifact.summary.chunk_count,
        "body_bytes": artifact.summary.body_bytes,
        "footer_bytes": artifact.summary.footer_bytes,
        "file_bytes": artifact.summary.file_bytes,
        "logical_sha256": hex(&artifact.summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "stale_temp_cleanup_required": stale_temp_cleanup_required,
        "development_only": true,
        "streaming": false,
        "memory_model": "bounded_all_memory_exact_fixed_mixed_complete_candidates_v1",
        "all_memory_limitation": "retains exact, fixed, and mixed complete candidate artifacts plus lane scratch during scoring",
        "build": build_json(provenance)
    })
}

fn grouped_v3_seal_json(
    schema: &aura_codec::SchemaDescriptor,
    summary: &V3GroupedWriteSummary,
    artifact_sha256: [u8; 32],
    stale_temp_cleanup_required: bool,
) -> serde_json::Value {
    let provenance = build_provenance();
    json!({
        "result_schema": "aura-v3-grouped-aura0-seal-result-v1",
        "protocol": SHADOW_PROTOCOL_V2,
        "complete_aura_file": true,
        "container_target": "grouped-aura0-v3-exact-v1",
        "container_version": 3,
        "profile": "aura0",
        "body_encoding": "grouped_exact_events_v1",
        "body_layout_version": V3_GROUPED_BODY_LAYOUT_VERSION,
        "event_block_version": V3_GROUPED_EVENT_BLOCK_VERSION,
        "footer_layout_version": V3_GROUPED_FOOTER_LAYOUT_VERSION,
        "compression": "none",
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&summary.schema_fingerprint),
        "event_count": summary.event_count,
        "child_count": summary.child_count,
        "chunk_count": summary.chunk_count,
        "body_bytes": summary.body_bytes,
        "footer_bytes": summary.footer_bytes,
        "file_bytes": summary.file_bytes,
        "logical_sha256": hex(&summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "stale_temp_cleanup_required": stale_temp_cleanup_required,
        "build": build_json(provenance)
    })
}

fn verify_v3_flat_value(file: File) -> Result<serde_json::Value, CliError> {
    let mut reader =
        V3FlatAura0Reader::open(file).map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let schema_id = reader.embedded_footer().schema.schema_id;
    let schema_fingerprint = reader.embedded_footer().schema_fingerprint;
    let summary = reader
        .verify_all()
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let mut file = reader.into_inner();
    let artifact_sha256 = hash_file_exact(&mut file, summary.file_bytes)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let provenance = build_provenance();
    Ok(json!({
        "result_schema": "aura-v3-flat-aura0-verify-result-v1",
        "protocol": SHADOW_PROTOCOL,
        "complete_aura_file": true,
        "container_target": "flat-aura0-v3-v1",
        "container_version": 3,
        "profile": "aura0",
        "body_encoding": "flat_exact_blocks_v1",
        "footer_layout_version": V3_FLAT_FOOTER_LAYOUT_VERSION,
        "verified": true,
        "schema_id": schema_id,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "row_count": summary.record_count,
        "chunk_count": summary.chunk_count,
        "body_bytes": summary.body_bytes,
        "footer_bytes": summary.footer_bytes,
        "file_bytes": summary.file_bytes,
        "logical_sha256": hex(&summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "build": {
            "git_commit": provenance.git_commit,
            "dirty": provenance.dirty,
            "provenance_source": provenance.source,
            "arrow_crate_version": arrow_rust_version(),
            "cargo_lock_sha256": hex(&cargo_lock_sha256())
        }
    }))
}

fn verify_v3_planned_flat_value(mut file: File) -> Result<serde_json::Value, CliError> {
    let bytes = read_bounded_planned_v3_file(&mut file)?;
    let decoded = decode_v3_planned_flat(&bytes, planned_cli_limits())
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    if decoded.summary.file_bytes
        != u64::try_from(bytes.len()).map_err(|_| CliError("v3 invalid artifact".to_owned()))?
        || decoded.summary.accounted_file_bytes != decoded.summary.file_bytes
    {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    let artifact_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    let provenance = build_provenance();
    Ok(json!({
        "result_schema": "aura-v3-flat-aura0-planned-verify-result-v1",
        "protocol": SHADOW_PROTOCOL,
        "complete_aura_file": true,
        "container_target": PlannedFlatSelection::Planned.container_target(),
        "container_version": 3,
        "profile": "aura0",
        "body_encoding": PlannedFlatSelection::Planned.body_encoding_name(),
        "body_encoding_code": V3_PLANNED_FLAT_BODY_ENCODING,
        "body_layout_version": V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
        "block_version": V3_PLANNED_FLAT_BLOCK_VERSION,
        "footer_layout_version": V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION,
        "compression": "none",
        "verified": true,
        "schema_id": decoded.footer.schema.schema_id,
        "schema_fingerprint_sha256": hex(&decoded.summary.schema_fingerprint),
        "row_count": decoded.summary.row_count,
        "chunk_count": decoded.summary.chunk_count,
        "body_bytes": decoded.summary.body_bytes,
        "footer_bytes": decoded.summary.footer_bytes,
        "file_bytes": decoded.summary.file_bytes,
        "plan_sha256": hex(&decoded.summary.plan_sha256),
        "logical_sha256": hex(&decoded.summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "development_only": true,
        "streaming": false,
        "memory_model": "bounded_all_memory_complete_file_decode_v1",
        "build": build_json(provenance)
    }))
}

fn read_bounded_planned_v3_file(file: &mut File) -> Result<Vec<u8>, CliError> {
    let before = file
        .metadata()
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    if !before.is_file() || before.len() > MAX_PLANNED_CLI_FILE_BYTES || before.len() < 24 {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    #[cfg(unix)]
    let before_identity = file_identity(&before);
    let capacity =
        usize::try_from(before.len()).map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let reserve = capacity
        .checked_add(1)
        .ok_or_else(|| CliError("v3 invalid artifact".to_owned()))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(reserve)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    (&mut *file)
        .take(before.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let after = file
        .metadata()
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    if bytes.len() != capacity || after.len() != before.len() {
        return Err(CliError("v3 invalid artifact".to_owned()));
    }
    #[cfg(unix)]
    if file_identity(&after) != before_identity {
        return Err(CliError("v3 input identity changed".to_owned()));
    }
    Ok(bytes)
}

fn verify_v3_grouped_value(file: File) -> Result<serde_json::Value, CliError> {
    let mut reader =
        V3GroupedAura0Reader::open(file).map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let schema_id = reader.embedded_footer().schema.schema_id;
    let schema_fingerprint = reader.embedded_footer().schema_fingerprint;
    let summary = reader
        .verify_all()
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let mut file = reader.into_inner();
    let artifact_sha256 = hash_file_exact(&mut file, summary.file_bytes)
        .map_err(|_| CliError("v3 invalid artifact".to_owned()))?;
    let provenance = build_provenance();
    Ok(json!({
        "result_schema": "aura-v3-grouped-aura0-verify-result-v1",
        "protocol": SHADOW_PROTOCOL_V2,
        "complete_aura_file": true,
        "container_target": "grouped-aura0-v3-exact-v1",
        "container_version": 3,
        "profile": "aura0",
        "body_encoding": "grouped_exact_events_v1",
        "body_layout_version": V3_GROUPED_BODY_LAYOUT_VERSION,
        "event_block_version": V3_GROUPED_EVENT_BLOCK_VERSION,
        "footer_layout_version": V3_GROUPED_FOOTER_LAYOUT_VERSION,
        "compression": "none",
        "verified": true,
        "schema_id": schema_id,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "event_count": summary.event_count,
        "child_count": summary.child_count,
        "chunk_count": summary.chunk_count,
        "body_bytes": summary.body_bytes,
        "footer_bytes": summary.footer_bytes,
        "file_bytes": summary.file_bytes,
        "logical_sha256": hex(&summary.global_logical_sha256),
        "artifact_sha256": hex(&artifact_sha256),
        "build": build_json(provenance)
    }))
}

fn require_shadow_protocol(protocol: Option<String>) -> Result<ShadowProtocolVersion, CliError> {
    match protocol.as_deref() {
        Some(SHADOW_PROTOCOL) => Ok(ShadowProtocolVersion::V1),
        Some(SHADOW_PROTOCOL_V2) => Ok(ShadowProtocolVersion::V2),
        _ => Err(CliError("unsupported or missing --protocol".to_owned())),
    }
}

fn require_v3_seal_mode(mode: Option<String>) -> Result<V3SealMode, CliError> {
    match mode.as_deref() {
        None | Some("exact") => Ok(V3SealMode::Exact),
        Some("planned") => Ok(V3SealMode::Planned),
        Some(_) => Err(CliError("unsupported v3 seal mode".to_owned())),
    }
}

fn build_json(provenance: aura_codec::BuildProvenance) -> serde_json::Value {
    json!({
        "git_commit": provenance.git_commit,
        "dirty": provenance.dirty,
        "provenance_source": provenance.source,
        "arrow_crate_version": arrow_rust_version(),
        "cargo_lock_sha256": hex(&cargo_lock_sha256())
    })
}

fn verify_shadow_v1_value(
    schema: &aura_codec::SchemaDescriptor,
    input: &Path,
) -> Result<serde_json::Value, CliError> {
    let verify_limits = ShadowProtocolLimits::default().values;
    let bytes = read_bounded_reference_block(input, verify_limits.max_block_bytes)?;
    let decoded = decode_v3_value_block(schema, &bytes, verify_limits)
        .map_err(|_| CliError("invalid shadow reference block".to_owned()))?;
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)
        .map_err(|_| CliError("invalid shadow schema fingerprint".to_owned()))?;
    let logical_sha256 = canonical_v3_batch_sha256(schema, &decoded, verify_limits)
        .map_err(|_| CliError("invalid shadow reference block".to_owned()))?;
    let artifact_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    let provenance = build_provenance();
    Ok(json!({
        "result_schema": SHADOW_VERIFY_RESULT_SCHEMA,
        "protocol": SHADOW_PROTOCOL,
        "artifact_kind": SHADOW_ARTIFACT_KIND,
        "complete_aura_file": false,
        "reference_block_version": 1,
        "package_version": env!("CARGO_PKG_VERSION"),
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "row_count": decoded.row_count,
        "logical_sha256": hex(&logical_sha256),
        "artifact_bytes": bytes.len(),
        "artifact_sha256": hex(&artifact_sha256),
        "build": build_json(provenance)
    }))
}

fn verify_shadow_v2_value(
    schema: &aura_codec::SchemaDescriptor,
    input: &Path,
) -> Result<serde_json::Value, CliError> {
    let limits = ShadowGroupedProtocolLimits::default();
    let bytes = read_bounded_reference_block(input, limits.events.max_block_bytes)?;
    let decoded = decode_v3_event_block(schema, &bytes, limits.events)
        .map_err(|_| CliError("invalid shadow grouped reference block".to_owned()))?;
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)
        .map_err(|_| CliError("invalid shadow grouped schema fingerprint".to_owned()))?;
    let logical_sha256 = canonical_v3_event_batch_sha256(schema, &decoded, limits.events)
        .map_err(|_| CliError("invalid shadow grouped reference block".to_owned()))?;
    let artifact_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    let provenance = build_provenance();
    Ok(json!({
        "result_schema": SHADOW_VERIFY_RESULT_SCHEMA_V2,
        "protocol": SHADOW_PROTOCOL_V2,
        "artifact_kind": SHADOW_ARTIFACT_KIND_V2,
        "complete_aura_file": false,
        "reference_block_version": 1,
        "package_version": env!("CARGO_PKG_VERSION"),
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "event_count": decoded.event_count,
        "child_count": decoded.child_count(),
        "logical_sha256": hex(&logical_sha256),
        "artifact_bytes": bytes.len(),
        "artifact_sha256": hex(&artifact_sha256),
        "build": build_json(provenance)
    }))
}

fn write_json_stdout(value: &serde_json::Value) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|_| CliError("could not serialize shadow result".to_owned()))?;
    stdout
        .write_all(b"\n")
        .map_err(|_| CliError("could not write stdout".to_owned()))
}

fn validate_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError(format!("unknown validate option {name:?}"))),
    })?;
    let input = input.ok_or_else(|| CliError("missing required --input <path>".to_owned()))?;
    let text = read_bounded_schema(&input)?;
    let schema = parse_schema_json(&text).map_err(|error| CliError(error.to_string()))?;
    if json_output {
        let name = serde_json::to_string(&schema.name)
            .map_err(|_| CliError("could not serialize validation result".to_owned()))?;
        println!(
            "{{\"valid\":true,\"schema_id\":{},\"name\":{},\"field_count\":{},\"group_count\":{}}}",
            schema.schema_id,
            name,
            schema.fields.len(),
            schema.groups.len()
        );
    } else {
        let escaped_name = serde_json::to_string(&schema.name)
            .map_err(|_| CliError("could not serialize schema name".to_owned()))?;
        println!("valid Aura schema {escaped_name} ({})", schema.schema_id);
    }
    Ok(())
}

fn inspect_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown schema inspect option".to_owned())),
    })?;
    if !json_output {
        return Err(CliError("schema inspect requires --json".to_owned()));
    }
    let input = input.ok_or_else(|| CliError("missing schema inspect input".to_owned()))?;
    let text = read_bounded_schema(&input)?;
    let schema = parse_schema_json(&text).map_err(|_| CliError("invalid schema".to_owned()))?;
    let canonical = schema
        .to_canonical_json()
        .map_err(|_| CliError("invalid canonical schema".to_owned()))?;
    let canonical_json_sha256: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
    let schema_fingerprint = canonical_v3_schema_fingerprint(&schema)
        .map_err(|_| CliError("invalid schema fingerprint".to_owned()))?;
    let compact_schema_map = schema
        .compact_schema_map
        .as_deref()
        .ok_or_else(|| CliError("schema mapping unavailable".to_owned()))?;
    let mut dual_domain_discriminators = Vec::new();
    dual_domain_discriminators
        .try_reserve_exact(schema.groups.len())
        .map_err(|_| CliError("could not allocate schema inspection result".to_owned()))?;
    for group in &schema.groups {
        let Some(dual) = group.dual_domain else {
            continue;
        };
        let map_byte = compact_schema_map
            .get(usize::from(dual.discriminator_slot))
            .copied()
            .ok_or_else(|| CliError("invalid discriminator mapping".to_owned()))?;
        dual_domain_discriminators.push(json!({
            "group_id": group.group_id,
            "domain_count": dual.domain_count,
            "discriminator_slot": dual.discriminator_slot,
            "schema_map_byte": map_byte
        }));
    }
    let (schema_encoding, schema_encoding_version) = match schema.encoding_version {
        SchemaEncodingVersion::V2 => ("v2", 2),
        SchemaEncodingVersion::V3 => ("v3", 3),
    };
    let provenance = build_provenance();
    write_json_stdout(&json!({
        "result_schema": "aura-schema-inspect-v1",
        "valid": true,
        "schema_format": "aura-schema",
        "schema_format_version": 1,
        "schema_encoding": schema_encoding,
        "schema_encoding_version": schema_encoding_version,
        "schema_id": schema.schema_id,
        "name": schema.name,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "canonical_json_sha256": hex(&canonical_json_sha256),
        "compact_schema_map": compact_schema_map,
        "compact_schema_map_hex": hex(compact_schema_map),
        "field_count": schema.fields.len(),
        "group_count": schema.groups.len(),
        "dual_domain_discriminators": dual_domain_discriminators,
        "build": {
            "package_version": env!("CARGO_PKG_VERSION"),
            "git_commit": provenance.git_commit,
            "dirty": provenance.dirty,
            "provenance_source": provenance.source,
            "cargo_lock_sha256": hex(&cargo_lock_sha256())
        }
    }))
}

fn canonicalize_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut input = None;
    let mut output = None;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--output" => set_path(&mut output, value, "--output"),
        _ => Err(CliError(format!("unknown canonicalize option {name:?}"))),
    })?;
    let input = input.ok_or_else(|| CliError("missing required --input <path>".to_owned()))?;
    let text = read_bounded_schema(&input)?;
    let schema = parse_schema_json(&text).map_err(|error| CliError(error.to_string()))?;
    let canonical = schema
        .to_canonical_json()
        .map_err(|error| CliError(error.to_string()))?;
    if let Some(output) = output {
        write_schema_atomically(&output, canonical.as_bytes())?;
    } else {
        io::stdout()
            .lock()
            .write_all(canonical.as_bytes())
            .map_err(|error| CliError(format!("could not write stdout: {error}")))?;
    }
    Ok(())
}

fn write_schema_atomically(output: &Path, bytes: &[u8]) -> Result<(), CliError> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CliError(format!(
                "schema output is not a regular file: {}",
                output.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CliError(format!(
                "could not inspect output {}: {error}",
                output.display()
            )));
        }
    }

    let parent = normalized_parent(output);
    let file_name = output
        .file_name()
        .ok_or_else(|| CliError("output path has no file name".to_owned()))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".aura-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let mut temp = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CliError(format!(
                    "could not create temporary output beside {}: {error}",
                    output.display()
                )));
            }
        };

        let write_result = temp
            .write_all(bytes)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all());
        drop(temp);
        if let Err(error) = write_result {
            let _ignored = fs::remove_file(&temp_path);
            return Err(CliError(format!(
                "could not write output {}: {error}",
                output.display()
            )));
        }
        if let Err(error) = fs::rename(&temp_path, output) {
            let _ignored = fs::remove_file(&temp_path);
            return Err(CliError(format!(
                "could not replace output {}: {error}",
                output.display()
            )));
        }
        return Ok(());
    }
    Err(CliError(format!(
        "could not create a unique temporary output beside {}",
        output.display()
    )))
}

fn require_absent_output(output: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(output) {
        Ok(_) => Err(CliError(
            "shadow output already exists or is a symlink".to_owned(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CliError("could not inspect shadow output".to_owned())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PublicationOutcome {
    stale_temp_cleanup_required: bool,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
}

#[cfg(unix)]
trait PublicationOps {
    fn open_parent(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }

    fn create_temp(&self, path: &Path) -> io::Result<File> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "shadow temporary output mode",
            ));
        }
        Ok(file)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(path)
    }

    fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
        fs::hard_link(source, destination)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn sync_parent(&self, directory: &File) -> io::Result<()> {
        directory.sync_all()
    }

    fn before_link(&self, _temp: &Path, _output: &Path) -> io::Result<()> {
        Ok(())
    }

    fn after_link(&self, _temp: &Path, _output: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
struct RealPublicationOps;

#[cfg(unix)]
impl PublicationOps for RealPublicationOps {}

fn publish_verified_block(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowEncodeResult,
) -> Result<PublicationOutcome, CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, schema, result);
        return Err(CliError(
            "trusted shadow publication is unsupported on this platform".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        publish_verified_block_with_ops(output, schema, result, &RealPublicationOps)
    }
}

fn publish_verified_v3_flat(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3Batch,
) -> Result<(PublicationOutcome, V3FlatWriteSummary, [u8; 32]), CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, schema, batch);
        Err(CliError(
            "trusted v3 publication is unsupported on this platform".to_owned(),
        ))
    }
    #[cfg(unix)]
    {
        publish_verified_v3_flat_with_ops(output, schema, batch, &RealPublicationOps)
    }
}

fn publish_verified_v3_planned(
    output: &Path,
    artifact: &V3PlannedFlatArtifact,
    selection: PlannedFlatSelection,
) -> Result<(PublicationOutcome, [u8; 32]), CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, artifact, selection);
        Err(CliError(
            "trusted v3 publication is unsupported on this platform".to_owned(),
        ))
    }
    #[cfg(unix)]
    {
        publish_verified_v3_planned_with_ops(output, artifact, selection, &RealPublicationOps)
    }
}

fn publish_verified_v3_grouped(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3EventBatch,
) -> Result<(PublicationOutcome, V3GroupedWriteSummary, [u8; 32]), CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, schema, batch);
        Err(CliError(
            "trusted v3 publication is unsupported on this platform".to_owned(),
        ))
    }
    #[cfg(unix)]
    {
        publish_verified_v3_grouped_with_ops(output, schema, batch, &RealPublicationOps)
    }
}

#[cfg(unix)]
fn publish_verified_v3_flat_with_ops(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3Batch,
    ops: &impl PublicationOps,
) -> Result<(PublicationOutcome, V3FlatWriteSummary, [u8; 32]), CliError> {
    publish_verified_complete_v3_with_ops(
        output,
        ops,
        |temp| seal_v3_flat_temp(temp, schema, batch),
        verify_temp_v3_flat,
        |summary| summary.file_bytes,
    )
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct V3PlannedPublicationExpected {
    summary: V3PlannedFlatSummary,
    selection: PlannedFlatSelection,
    artifact_sha256: [u8; 32],
}

#[cfg(unix)]
fn publish_verified_v3_planned_with_ops(
    output: &Path,
    artifact: &V3PlannedFlatArtifact,
    selection: PlannedFlatSelection,
    ops: &impl PublicationOps,
) -> Result<(PublicationOutcome, [u8; 32]), CliError> {
    let expected = V3PlannedPublicationExpected {
        summary: artifact.summary.clone(),
        selection,
        artifact_sha256: Sha256::digest(&artifact.bytes).into(),
    };
    let (publication, _, artifact_sha256) = publish_verified_complete_v3_with_ops(
        output,
        ops,
        |mut temp| {
            temp.write_all(&artifact.bytes)
                .map_err(|_| CliError("could not write planned v3 temporary output".to_owned()))?;
            temp.sync_all()
                .map_err(|_| CliError("could not sync planned v3 temporary output".to_owned()))?;
            Ok((temp, expected.clone()))
        },
        verify_temp_v3_planned,
        |expected| expected.summary.file_bytes,
    )?;
    Ok((publication, artifact_sha256))
}

#[cfg(unix)]
fn publish_verified_v3_grouped_with_ops(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3EventBatch,
    ops: &impl PublicationOps,
) -> Result<(PublicationOutcome, V3GroupedWriteSummary, [u8; 32]), CliError> {
    publish_verified_complete_v3_with_ops(
        output,
        ops,
        |temp| seal_v3_grouped_temp(temp, schema, batch),
        verify_temp_v3_grouped,
        |summary| summary.file_bytes,
    )
}

#[cfg(unix)]
fn publish_verified_complete_v3_with_ops<T>(
    output: &Path,
    ops: &impl PublicationOps,
    mut seal: impl FnMut(File) -> Result<(File, T), CliError>,
    mut verify: impl FnMut(&mut File, FileIdentity, &T) -> Result<[u8; 32], CliError>,
    file_bytes: impl Fn(&T) -> u64,
) -> Result<(PublicationOutcome, T, [u8; 32]), CliError> {
    let parent = normalized_parent(output);
    let directory = ops
        .open_parent(parent)
        .map_err(|_| CliError("could not open trusted v3 output directory".to_owned()))?;
    validate_trusted_parent(parent, &directory, ops)?;
    let file_name = output
        .file_name()
        .ok_or_else(|| CliError("v3 output has no file name".to_owned()))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".aura-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let temp = match ops.create_temp(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(CliError("could not create v3 temporary output".to_owned())),
        };
        let held_origin = file_identity(
            &temp
                .metadata()
                .map_err(|_| CliError("could not inspect held v3 output".to_owned()))?,
        );
        let held_guard = match temp.try_clone() {
            Ok(file) => file,
            Err(_) => {
                return Err(prelink_failure(
                    &directory,
                    &temp_path,
                    Some(held_origin),
                    ops,
                    CliError("could not retain held v3 output".to_owned()),
                ))
            }
        };
        let (mut temp, summary) = match seal(temp) {
            Ok(result) => result,
            Err(_) => {
                let identity = held_guard
                    .metadata()
                    .ok()
                    .map(|value| file_identity(&value));
                return Err(prelink_failure(
                    &directory,
                    &temp_path,
                    identity,
                    ops,
                    CliError("could not seal v3 temporary output".to_owned()),
                ));
            }
        };
        let held_identity = file_identity(
            &temp
                .metadata()
                .map_err(|_| CliError("could not inspect sealed v3 output".to_owned()))?,
        );
        if !same_file_object(held_origin, held_identity) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("held v3 output identity changed during write".to_owned()),
            ));
        }
        if !temp
            .metadata()
            .is_ok_and(|value| value.len() == file_bytes(&summary))
        {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("invalid v3 temporary output length".to_owned()),
            ));
        }
        if let Err(error) = verify(&mut temp, held_identity, &summary) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.before_link(&temp_path, output).is_err()
            || !path_matches_identity(&temp_path, held_identity, ops)
        {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("v3 temporary output identity changed before publication".to_owned()),
            ));
        }
        if let Err(error) = validate_trusted_parent(parent, &directory, ops) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.hard_link(&temp_path, output).is_err() {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("could not publish v3 output without overwrite".to_owned()),
            ));
        }
        let identity_ok = ops.after_link(&temp_path, output).is_ok()
            && path_matches_identity(&temp_path, held_identity, ops)
            && path_matches_identity(output, held_identity, ops);
        let final_verification = if identity_ok {
            verify(&mut temp, held_identity, &summary)
        } else {
            Err(CliError("v3 publication identity changed".to_owned()))
        };
        let artifact_sha256 = match final_verification {
            Ok(hash) => hash,
            Err(_) => {
                if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                    return Err(CliError(
                        "v3 publication identity verification failed".to_owned(),
                    ));
                }
                return Err(CliError("publication ambiguous".to_owned()));
            }
        };
        if ops.sync_parent(&directory).is_err() {
            if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                return Err(CliError("could not commit v3 output directory".to_owned()));
            }
            return Err(CliError("publication ambiguous".to_owned()));
        }
        let stale_temp_cleanup_required = if remove_if_owned(&temp_path, held_identity, ops) {
            ops.sync_parent(&directory).is_err()
        } else {
            true
        };
        return Ok((
            PublicationOutcome {
                stale_temp_cleanup_required,
            },
            summary,
            artifact_sha256,
        ));
    }
    Err(CliError(
        "could not create unique v3 temporary output".to_owned(),
    ))
}

#[cfg(unix)]
fn seal_v3_flat_temp(
    temp: File,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3Batch,
) -> Result<(File, V3FlatWriteSummary), CliError> {
    let mut writer =
        V3FlatAura0Writer::try_new(temp, schema.clone(), V3FlatWriterOptions::default())
            .map_err(|_| CliError("could not initialize v3 temporary output".to_owned()))?;
    if batch.row_count != 0 {
        writer
            .write_batch(batch)
            .map_err(|_| CliError("could not write v3 temporary output".to_owned()))?;
    }
    writer
        .finish_and_sync()
        .map_err(|_| CliError("could not seal v3 temporary output".to_owned()))
}

#[cfg(unix)]
fn seal_v3_grouped_temp(
    temp: File,
    schema: &aura_codec::SchemaDescriptor,
    batch: &aura_codec::AuraV3EventBatch,
) -> Result<(File, V3GroupedWriteSummary), CliError> {
    let mut writer =
        V3GroupedAura0Writer::try_new(temp, schema.clone(), V3GroupedWriterOptions::default())
            .map_err(|_| CliError("could not initialize grouped v3 temporary output".to_owned()))?;
    if batch.event_count != 0 {
        writer
            .write_batch(batch)
            .map_err(|_| CliError("could not write grouped v3 temporary output".to_owned()))?;
    }
    writer
        .finish_and_sync()
        .map_err(|_| CliError("could not seal grouped v3 temporary output".to_owned()))
}

#[cfg(unix)]
fn verify_temp_v3_flat(
    temp: &mut File,
    expected_identity: FileIdentity,
    expected: &V3FlatWriteSummary,
) -> Result<[u8; 32], CliError> {
    let metadata = temp
        .metadata()
        .map_err(|_| CliError("could not inspect v3 temporary output".to_owned()))?;
    if !metadata.is_file()
        || metadata.len() != expected.file_bytes
        || file_identity(&metadata) != expected_identity
    {
        return Err(CliError("invalid v3 temporary output length".to_owned()));
    }
    temp.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("could not seek v3 temporary output".to_owned()))?;
    let mut reader = V3FlatAura0Reader::open(&mut *temp)
        .map_err(|_| CliError("v3 temporary output open failed".to_owned()))?;
    let verified = reader
        .verify_all()
        .map_err(|_| CliError("v3 temporary output verification failed".to_owned()))?;
    if verified.record_count != expected.record_count
        || verified.chunk_count != expected.chunk_count
        || verified.header_bytes != expected.header_bytes
        || verified.body_bytes != expected.body_bytes
        || verified.footer_bytes != expected.footer_bytes
        || verified.file_bytes != expected.file_bytes
        || verified.schema_fingerprint != expected.schema_fingerprint
        || verified.global_logical_sha256 != expected.global_logical_sha256
    {
        return Err(CliError("v3 temporary output summary mismatch".to_owned()));
    }
    hash_held_file(temp, expected.file_bytes)
}

#[cfg(unix)]
fn verify_temp_v3_planned(
    temp: &mut File,
    expected_identity: FileIdentity,
    expected: &V3PlannedPublicationExpected,
) -> Result<[u8; 32], CliError> {
    let metadata = temp
        .metadata()
        .map_err(|_| CliError("could not inspect planned v3 temporary output".to_owned()))?;
    if !metadata.is_file()
        || metadata.len() != expected.summary.file_bytes
        || file_identity(&metadata) != expected_identity
    {
        return Err(CliError(
            "invalid planned v3 temporary output length".to_owned(),
        ));
    }
    let bytes = read_bounded_planned_v3_file(temp)
        .map_err(|_| CliError("planned v3 temporary output read failed".to_owned()))?;
    let decoded = decode_v3_selected_flat(&bytes, planned_cli_limits())
        .map_err(|_| CliError("planned v3 temporary output decode failed".to_owned()))?;
    let summary_matches = match (expected.selection, decoded) {
        (PlannedFlatSelection::Exact, DecodedV3SelectedFlat::Exact(decoded)) => {
            let header_bytes = aura_codec::AuraHeader::encoded_len(&bytes).ok();
            let footer_bytes = bytes
                .get(bytes.len().saturating_sub(12)..bytes.len().saturating_sub(8))
                .and_then(|value| <[u8; 4]>::try_from(value).ok())
                .map(u32::from_le_bytes);
            decoded.footer.record_count == expected.summary.row_count
                && decoded.footer.body_len == expected.summary.body_bytes
                && decoded.footer.chunks.len() == expected.summary.chunk_count as usize
                && decoded.footer.schema_fingerprint == expected.summary.schema_fingerprint
                && decoded.footer.global_logical_sha256 == expected.summary.global_logical_sha256
                && header_bytes == usize::try_from(expected.summary.header_bytes).ok()
                && footer_bytes == Some(expected.summary.footer_bytes)
                && expected.summary.plan_sha256 == [0; 32]
        }
        (PlannedFlatSelection::Planned, DecodedV3SelectedFlat::Planned(decoded)) => {
            decoded.summary == expected.summary
        }
        _ => false,
    };
    let artifact_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    if !summary_matches || artifact_sha256 != expected.artifact_sha256 {
        return Err(CliError(
            "planned v3 temporary output summary mismatch".to_owned(),
        ));
    }
    Ok(artifact_sha256)
}

#[cfg(unix)]
fn verify_temp_v3_grouped(
    temp: &mut File,
    expected_identity: FileIdentity,
    expected: &V3GroupedWriteSummary,
) -> Result<[u8; 32], CliError> {
    let metadata = temp
        .metadata()
        .map_err(|_| CliError("could not inspect grouped v3 temporary output".to_owned()))?;
    if !metadata.is_file()
        || metadata.len() != expected.file_bytes
        || file_identity(&metadata) != expected_identity
    {
        return Err(CliError(
            "invalid grouped v3 temporary output length".to_owned(),
        ));
    }
    temp.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("could not seek grouped v3 temporary output".to_owned()))?;
    let mut reader = V3GroupedAura0Reader::open(&mut *temp)
        .map_err(|_| CliError("grouped v3 temporary output open failed".to_owned()))?;
    let verified = reader
        .verify_all()
        .map_err(|_| CliError("grouped v3 temporary output verification failed".to_owned()))?;
    if verified.event_count != expected.event_count
        || verified.child_count != expected.child_count
        || verified.chunk_count != expected.chunk_count
        || verified.header_bytes != expected.header_bytes
        || verified.body_bytes != expected.body_bytes
        || verified.footer_bytes != expected.footer_bytes
        || verified.file_bytes != expected.file_bytes
        || verified.schema_fingerprint != expected.schema_fingerprint
        || verified.global_logical_sha256 != expected.global_logical_sha256
    {
        return Err(CliError(
            "grouped v3 temporary output summary mismatch".to_owned(),
        ));
    }
    hash_held_file(temp, expected.file_bytes)
}

#[cfg(unix)]
fn hash_held_file(file: &mut File, expected_len: u64) -> Result<[u8; 32], CliError> {
    hash_file_exact(file, expected_len)
}

fn hash_file_exact(file: &mut File, expected_len: u64) -> Result<[u8; 32], CliError> {
    hash_file_exact_with_hook(file, expected_len, || {})
}

fn hash_file_exact_with_hook(
    file: &mut File,
    expected_len: u64,
    after_initial_metadata: impl FnOnce(),
) -> Result<[u8; 32], CliError> {
    let before = file
        .metadata()
        .map_err(|_| CliError("v3 file hash metadata failed".to_owned()))?;
    if !before.is_file() || before.len() != expected_len {
        return Err(CliError("v3 file hash length changed".to_owned()));
    }
    after_initial_metadata();
    file.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("could not seek v3 temporary output".to_owned()))?;
    let mut remaining = expected_len;
    let mut hasher = Sha256::new();
    let mut scratch = [0u8; 16 * 1024];
    while remaining != 0 {
        let request = usize::try_from(remaining.min(scratch.len() as u64)).unwrap();
        let read = file
            .read(&mut scratch[..request])
            .map_err(|_| CliError("could not hash v3 temporary output".to_owned()))?;
        if read == 0 {
            return Err(CliError("v3 temporary output changed length".to_owned()));
        }
        hasher.update(&scratch[..read]);
        remaining -= read as u64;
    }
    let mut probe = [0u8; 1];
    if file
        .read(&mut probe)
        .map_err(|_| CliError("could not hash v3 temporary output".to_owned()))?
        != 0
    {
        return Err(CliError("v3 file hash length changed".to_owned()));
    }
    let after = file
        .metadata()
        .map_err(|_| CliError("v3 file hash metadata failed".to_owned()))?;
    #[cfg(unix)]
    if file_identity(&before) != file_identity(&after) {
        return Err(CliError("v3 file hash identity changed".to_owned()));
    }
    #[cfg(not(unix))]
    if !after.is_file() || after.len() != before.len() {
        return Err(CliError("v3 file hash identity changed".to_owned()));
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn open_v3_input(path: &Path, before: &fs::Metadata) -> Result<File, CliError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW: i32 = 0o400000;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| CliError("v3 input path rejected".to_owned()))?;
    let after =
        fs::symlink_metadata(path).map_err(|_| CliError("v3 input identity changed".to_owned()))?;
    let held = file
        .metadata()
        .map_err(|_| CliError("v3 input identity changed".to_owned()))?;
    if file_identity(before) != file_identity(&after)
        || file_identity(before) != file_identity(&held)
    {
        return Err(CliError("v3 input identity changed".to_owned()));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_v3_input(_path: &Path, _before: &fs::Metadata) -> Result<File, CliError> {
    Err(CliError("v3 verify unsupported platform".to_owned()))
}

fn publish_verified_grouped_block(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowGroupedEncodeResult,
) -> Result<PublicationOutcome, CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, schema, result);
        return Err(CliError(
            "trusted shadow publication is unsupported on this platform".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        publish_verified_grouped_block_with_ops(output, schema, result, &RealPublicationOps)
    }
}

#[cfg(unix)]
fn publish_verified_grouped_block_with_ops(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowGroupedEncodeResult,
    ops: &impl PublicationOps,
) -> Result<PublicationOutcome, CliError> {
    publish_verified_shadow_bytes_with_ops(
        output,
        &result.block,
        MAX_V3_EVENT_BLOCK_BYTES,
        ops,
        |bytes| verify_grouped_block_bytes(schema, result, bytes),
    )
}

#[cfg(unix)]
fn verify_grouped_block_bytes(
    schema: &aura_codec::SchemaDescriptor,
    expected: &ShadowGroupedEncodeResult,
    bytes: &[u8],
) -> Result<(), CliError> {
    let artifact_hash: [u8; 32] = Sha256::digest(bytes).into();
    if artifact_hash != expected.block_sha256 {
        return Err(CliError("shadow temporary output hash mismatch".to_owned()));
    }
    let limits = ShadowGroupedProtocolLimits::default().events;
    let decoded = decode_v3_event_block(schema, bytes, limits)
        .map_err(|_| CliError("shadow temporary grouped output decode failed".to_owned()))?;
    let logical = canonical_v3_event_batch_sha256(schema, &decoded, limits)
        .map_err(|_| CliError("shadow temporary output logical verification failed".to_owned()))?;
    if logical != expected.logical_sha256
        || decoded.event_count != expected.event_count
        || decoded.child_count() != expected.child_count
    {
        return Err(CliError(
            "shadow temporary output logical hash mismatch".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn publish_verified_block_with_ops(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowEncodeResult,
    ops: &impl PublicationOps,
) -> Result<PublicationOutcome, CliError> {
    publish_verified_shadow_bytes_with_ops(
        output,
        &result.block,
        MAX_V3_VALUE_BLOCK_BYTES,
        ops,
        |bytes| verify_block_bytes(schema, result, bytes),
    )
}

#[cfg(unix)]
fn publish_verified_shadow_bytes_with_ops(
    output: &Path,
    bytes: &[u8],
    max_bytes: usize,
    ops: &impl PublicationOps,
    mut verify: impl FnMut(&[u8]) -> Result<(), CliError>,
) -> Result<PublicationOutcome, CliError> {
    if bytes.len() > max_bytes {
        return Err(CliError("shadow artifact exceeds byte limit".to_owned()));
    }
    let expected_sha256: [u8; 32] = Sha256::digest(bytes).into();
    let parent = normalized_parent(output);
    let directory = ops
        .open_parent(parent)
        .map_err(|_| CliError("could not open trusted shadow output directory".to_owned()))?;
    validate_trusted_parent(parent, &directory, ops)?;
    let file_name = output
        .file_name()
        .ok_or_else(|| CliError("shadow output has no file name".to_owned()))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".aura-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let mut temp = match ops.create_temp(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(CliError(
                    "could not create shadow temporary output".to_owned(),
                ))
            }
        };
        let write_result = temp
            .write_all(bytes)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all());
        if write_result.is_err() {
            let identity = temp
                .metadata()
                .ok()
                .map(|metadata| file_identity(&metadata));
            return Err(prelink_failure(
                &directory,
                &temp_path,
                identity,
                ops,
                CliError("could not write shadow temporary output".to_owned()),
            ));
        }
        let held_identity =
            file_identity(&temp.metadata().map_err(|_| {
                CliError("could not inspect held shadow temporary output".to_owned())
            })?);
        if let Err(error) = verify_held_shadow_bytes(
            &mut temp,
            held_identity,
            bytes.len(),
            max_bytes,
            expected_sha256,
            &mut verify,
        ) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.before_link(&temp_path, output).is_err()
            || !path_matches_identity(&temp_path, held_identity, ops)
        {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("shadow temporary output identity changed before publication".to_owned()),
            ));
        }
        if let Err(error) = validate_trusted_parent(parent, &directory, ops) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.hard_link(&temp_path, output).is_err() {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("could not publish shadow output without overwrite".to_owned()),
            ));
        }
        let identity_ok = ops.after_link(&temp_path, output).is_ok()
            && path_matches_identity(&temp_path, held_identity, ops)
            && path_matches_identity(output, held_identity, ops);
        let final_verified = identity_ok
            && verify_held_shadow_bytes(
                &mut temp,
                held_identity,
                bytes.len(),
                max_bytes,
                expected_sha256,
                &mut verify,
            )
            .is_ok();
        if !final_verified {
            if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                return Err(CliError(
                    "shadow publication identity verification failed".to_owned(),
                ));
            }
            return Err(CliError("publication ambiguous".to_owned()));
        }
        if ops.sync_parent(&directory).is_err() {
            if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                return Err(CliError(
                    "could not commit shadow output directory".to_owned(),
                ));
            }
            return Err(CliError("publication ambiguous".to_owned()));
        }
        let stale_temp_cleanup_required = if remove_if_owned(&temp_path, held_identity, ops) {
            ops.sync_parent(&directory).is_err()
        } else {
            true
        };
        return Ok(PublicationOutcome {
            stale_temp_cleanup_required,
        });
    }
    Err(CliError(
        "could not create unique shadow temporary output".to_owned(),
    ))
}

#[cfg(unix)]
fn verify_held_shadow_bytes(
    temp: &mut File,
    expected_identity: FileIdentity,
    expected_len: usize,
    max_bytes: usize,
    expected_sha256: [u8; 32],
    verify: &mut impl FnMut(&[u8]) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let metadata = temp
        .metadata()
        .map_err(|_| CliError("could not inspect shadow temporary output".to_owned()))?;
    if !metadata.is_file()
        || metadata.len() != expected_len as u64
        || metadata.len() > max_bytes as u64
        || file_identity(&metadata) != expected_identity
    {
        return Err(CliError(
            "invalid shadow temporary output length".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_len)
        .map_err(|_| CliError("could not allocate verification buffer".to_owned()))?;
    temp.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("could not seek shadow temporary output".to_owned()))?;
    temp.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError("could not verify shadow temporary output".to_owned()))?;
    if bytes.len() != expected_len || Sha256::digest(&bytes).as_slice() != expected_sha256 {
        return Err(CliError("shadow temporary output hash mismatch".to_owned()));
    }
    verify(&bytes)
}

#[cfg(unix)]
fn validate_trusted_parent(
    parent: &Path,
    directory: &File,
    ops: &impl PublicationOps,
) -> Result<(), CliError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let held = directory
        .metadata()
        .map_err(|_| CliError("could not inspect trusted shadow output directory".to_owned()))?;
    let path = ops
        .symlink_metadata(parent)
        .map_err(|_| CliError("could not inspect trusted shadow output directory".to_owned()))?;
    if !held.is_dir()
        || !path.is_dir()
        || path.file_type().is_symlink()
        || held.dev() != path.dev()
        || held.ino() != path.ino()
        || path.permissions().mode() & 0o022 != 0
    {
        return Err(CliError(
            "shadow output directory is not trusted".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
    }
}

#[cfg(unix)]
const fn same_file_object(left: FileIdentity, right: FileIdentity) -> bool {
    left.device == right.device && left.inode == right.inode
}

#[cfg(unix)]
fn path_matches_identity(path: &Path, expected: FileIdentity, ops: &impl PublicationOps) -> bool {
    ops.symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && file_identity(&metadata) == expected
    })
}

#[cfg(unix)]
fn cleanup_owned_path(
    path: &Path,
    identity: Option<FileIdentity>,
    ops: &impl PublicationOps,
) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    let _ = remove_if_owned(path, identity, ops);
    !path_matches_identity(path, identity, ops)
}

#[cfg(unix)]
fn prelink_failure(
    directory: &File,
    temp_path: &Path,
    identity: Option<FileIdentity>,
    ops: &impl PublicationOps,
    ordinary: CliError,
) -> CliError {
    if cleanup_owned_path(temp_path, identity, ops) && ops.sync_parent(directory).is_ok() {
        ordinary
    } else {
        CliError("publication cleanup required".to_owned())
    }
}

#[cfg(unix)]
fn remove_if_owned(path: &Path, identity: FileIdentity, ops: &impl PublicationOps) -> bool {
    path_matches_identity(path, identity, ops) && ops.remove_file(path).is_ok()
}

#[cfg(unix)]
fn rollback_uncommitted(
    directory: &File,
    temp_path: &Path,
    output: &Path,
    identity: FileIdentity,
    ops: &impl PublicationOps,
) -> bool {
    let _ = remove_if_owned(output, identity, ops);
    let _ = remove_if_owned(temp_path, identity, ops);
    let final_absent = matches!(
        ops.symlink_metadata(output),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    let owned_temp_absent = !path_matches_identity(temp_path, identity, ops);
    final_absent && owned_temp_absent && ops.sync_parent(directory).is_ok()
}

#[cfg(unix)]
fn verify_block_bytes(
    schema: &aura_codec::SchemaDescriptor,
    expected: &ShadowEncodeResult,
    bytes: &[u8],
) -> Result<(), CliError> {
    let artifact_hash: [u8; 32] = Sha256::digest(bytes).into();
    if artifact_hash != expected.block_sha256 {
        return Err(CliError("shadow temporary output hash mismatch".to_owned()));
    }
    let decoded = decode_v3_value_block(schema, bytes, V3ValueLimits::default())
        .map_err(|_| CliError("shadow temporary output decode failed".to_owned()))?;
    let logical = canonical_v3_batch_sha256(schema, &decoded, V3ValueLimits::default())
        .map_err(|_| CliError("shadow temporary output logical verification failed".to_owned()))?;
    if logical != expected.logical_sha256 || decoded.row_count != expected.row_count {
        return Err(CliError(
            "shadow temporary output logical hash mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn parse_options(
    args: &[String],
    mut option: impl FnMut(&str, Option<&str>) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let mut index = 0;
    while index < args.len() {
        let name = args[index].as_str();
        if name == "--json" {
            option(name, None)?;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError(format!("missing value for {name}")))?;
        if value.starts_with("--") {
            return Err(CliError(format!("missing value for {name}")));
        }
        option(name, Some(value))?;
        index += 2;
    }
    Ok(())
}

fn set_path(slot: &mut Option<PathBuf>, value: Option<&str>, option: &str) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError(format!("duplicate option {option}")));
    }
    let value = value.ok_or_else(|| CliError(format!("missing value for {option}")))?;
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn normalized_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn set_string(
    slot: &mut Option<String>,
    value: Option<&str>,
    option: &str,
) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError(format!("duplicate option {option}")));
    }
    let value = value.ok_or_else(|| CliError(format!("missing value for {option}")))?;
    *slot = Some(value.to_owned());
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn bounded_error(message: &str) -> String {
    message
        .chars()
        .take(2048)
        .map(|character| {
            if character.is_control() && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn shadow_safe_error(message: &str) -> (&'static str, String) {
    let (code, safe) = if message == "publication ambiguous" {
        (
            "publication_ambiguous",
            "shadow publication state requires coordinator reconciliation",
        )
    } else if message == "publication cleanup required" {
        (
            "publication_cleanup_required",
            "no final artifact was committed; coordinator cleanup of a stale temporary file is required",
        )
    } else if message == "publication committed result unavailable" {
        (
            "publication_committed_result_unavailable",
            "the artifact was committed; coordinator must verify and adopt the final artifact",
        )
    } else if message.contains("unsupported on this platform") {
        (
            "publication_unsupported",
            "trusted shadow publication is unsupported on this platform",
        )
    } else if message.contains("not trusted") {
        (
            "untrusted_output_directory",
            "shadow output directory is not trusted",
        )
    } else if message.contains("already exists") || message.contains("symlink") {
        ("output_exists", "shadow output is not a new path")
    } else if message.contains("extension") || message.contains("filesystem paths") {
        ("invalid_output_path", "shadow output path is invalid")
    } else if message.contains("protocol") || message.contains("artifact-kind") {
        (
            "unsupported_contract",
            "shadow protocol or artifact kind is unsupported",
        )
    } else if message.contains("schema") && !message.contains("temporary output") {
        ("invalid_schema", "shadow schema is invalid or unavailable")
    } else if message.contains("reference block") {
        (
            "invalid_artifact",
            "shadow reference block is invalid or unavailable",
        )
    } else if message.contains("shadow ipc")
        || message.contains("shadow arrow")
        || message.contains("unexpected end")
    {
        ("invalid_ipc", "shadow Arrow IPC input is invalid")
    } else if message.contains("publish")
        || message.contains("temporary output")
        || message.contains("synchronize")
    {
        ("publication_failed", "shadow artifact publication failed")
    } else {
        ("shadow_command_failed", "shadow command failed")
    };
    (code, safe.to_owned())
}

fn v3_safe_error(message: &str) -> (&'static str, String) {
    let (code, safe) = if message == "publication ambiguous" {
        (
            "publication_ambiguous",
            "v3 publication state requires coordinator reconciliation",
        )
    } else if message == "publication cleanup required" {
        (
            "publication_cleanup_required",
            "no final v3 artifact was committed; owned temporary cleanup is required",
        )
    } else if message == "publication committed result unavailable" {
        (
            "publication_committed_result_unavailable",
            "the v3 artifact was committed; verify and adopt the final artifact",
        )
    } else if message == "publication_committed_result_unavailable_cleanup_required" {
        (
            "publication_committed_result_unavailable_cleanup_required",
            "the v3 artifact was committed; verify the final artifact and reconcile stale temporary cleanup",
        )
    } else if message == "v3 invalid artifact" {
        ("invalid_artifact", "v3 artifact is invalid or unavailable")
    } else if matches!(
        message,
        "v3 input path rejected" | "v3 input identity changed"
    ) {
        (
            "invalid_input_path",
            "v3 input path or held identity is invalid",
        )
    } else if message == "v3 verify unsupported platform" {
        (
            "verification_unsupported",
            "strong-identity v3 verification is unsupported on this platform",
        )
    } else if message.contains("unsupported on this platform") {
        (
            "publication_unsupported",
            "trusted v3 publication is unsupported",
        )
    } else if message.contains("not trusted") {
        (
            "untrusted_output_directory",
            "v3 output directory is not trusted",
        )
    } else if message.contains("already exists") {
        ("output_exists", "v3 output is not a new regular path")
    } else if message.contains("protocol") {
        ("unsupported_contract", "v3 input protocol is unsupported")
    } else if message == "grouped v3 invalid ipc" {
        ("invalid_ipc", "grouped v3 Arrow IPC input is invalid")
    } else if message.contains("schema") {
        ("invalid_schema", "v3 schema is invalid or unavailable")
    } else if message.contains("publish") || message.contains("temporary output") {
        ("publication_failed", "v3 artifact publication failed")
    } else {
        ("v3_command_failed", "v3 command failed")
    };
    (code, safe.to_owned())
}

fn read_bounded_schema(path: &Path) -> Result<String, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        CliError(format!(
            "could not inspect input {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError(format!(
            "schema input is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_SCHEMA_JSON_BYTES as u64 {
        return Err(CliError(format!(
            "schema input exceeds {MAX_SCHEMA_JSON_BYTES} bytes"
        )));
    }
    let file = File::open(path)
        .map_err(|error| CliError(format!("could not open input {}: {error}", path.display())))?;
    let held = file
        .metadata()
        .map_err(|_| CliError("could not inspect held schema input".to_owned()))?;
    #[cfg(unix)]
    if file_identity(&metadata) != file_identity(&held) {
        return Err(CliError("schema input identity changed".to_owned()));
    }
    #[cfg(not(unix))]
    if !held.is_file() || held.len() != metadata.len() {
        return Err(CliError("schema input identity changed".to_owned()));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize)
        .map_err(|_| CliError("could not allocate schema input buffer".to_owned()))?;
    file.take((MAX_SCHEMA_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| CliError(format!("could not read input {}: {error}", path.display())))?;
    if bytes.len() > MAX_SCHEMA_JSON_BYTES {
        return Err(CliError(format!(
            "schema input exceeds {MAX_SCHEMA_JSON_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| CliError("schema input is not UTF-8".to_owned()))
}

fn read_bounded_reference_block(path: &Path, max_block_bytes: usize) -> Result<Vec<u8>, CliError> {
    let max_block_bytes = max_block_bytes.min(MAX_V3_VALUE_BLOCK_BYTES);
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|_| CliError("could not inspect shadow reference block".to_owned()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(CliError(
            "shadow reference block is not a regular non-symlink file".to_owned(),
        ));
    }
    if path_metadata.len() > max_block_bytes as u64 {
        return Err(CliError(
            "shadow reference block exceeds byte limit".to_owned(),
        ));
    }
    let file = File::open(path)
        .map_err(|_| CliError("could not open shadow reference block".to_owned()))?;
    let held_metadata = file
        .metadata()
        .map_err(|_| CliError("could not inspect shadow reference block".to_owned()))?;
    if !held_metadata.is_file() || held_metadata.len() != path_metadata.len() {
        return Err(CliError(
            "shadow reference block identity changed".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if held_metadata.dev() != path_metadata.dev() || held_metadata.ino() != path_metadata.ino()
        {
            return Err(CliError(
                "shadow reference block identity changed".to_owned(),
            ));
        }
    }
    let length = usize::try_from(held_metadata.len())
        .map_err(|_| CliError("shadow reference block exceeds byte limit".to_owned()))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| CliError("could not allocate shadow reference block buffer".to_owned()))?;
    file.take((max_block_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError("could not read shadow reference block".to_owned()))?;
    if bytes.len() != length || bytes.len() > max_block_bytes {
        return Err(CliError(
            "shadow reference block identity changed".to_owned(),
        ));
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod publication_tests {
    use std::cell::Cell;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    use aura_codec::{
        canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, encode_v3_value_block,
        AuraV3Batch, AuraV3Column, AuraV3ColumnValues, AuraV3EventBatch, FieldRole, FieldType,
        RelationshipPermissions, SchemaBuilder,
    };

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "aura-publication-unit-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn output(&self) -> PathBuf {
            self.0.join("values.aurav3vb")
        }

        fn temp_paths(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(".aura-tmp-"))
                })
                .collect()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FaultOps {
        fail_open: bool,
        replace_temp_before_link: bool,
        fail_temp_remove_after_link: bool,
        fail_temp_remove_before_link: bool,
        fail_all_remove_after_link: bool,
        fail_sync_call: Option<usize>,
        sync_calls: Cell<usize>,
        linked: Cell<bool>,
    }

    impl PublicationOps for FaultOps {
        fn open_parent(&self, path: &Path) -> io::Result<File> {
            if self.fail_open {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                File::open(path)
            }
        }

        fn create_temp(&self, path: &Path) -> io::Result<File> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        }

        fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
            let result = fs::hard_link(source, destination);
            if result.is_ok() {
                self.linked.set(true);
            }
            result
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            let is_temp = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".aura-tmp-"));
            if (is_temp && self.fail_temp_remove_before_link && !self.linked.get())
                || (self.linked.get()
                    && (self.fail_all_remove_after_link
                        || (is_temp && self.fail_temp_remove_after_link)))
            {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                fs::remove_file(path)
            }
        }

        fn sync_parent(&self, directory: &File) -> io::Result<()> {
            let call = self.sync_calls.get() + 1;
            self.sync_calls.set(call);
            if self.fail_sync_call == Some(call) {
                Err(io::Error::other("injected"))
            } else {
                directory.sync_all()
            }
        }

        fn before_link(&self, temp: &Path, _output: &Path) -> io::Result<()> {
            if self.replace_temp_before_link {
                fs::remove_file(temp)?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(temp)?
                    .write_all(b"replacement")?;
            }
            Ok(())
        }
    }

    fn reference() -> (aura_codec::SchemaDescriptor, ShadowEncodeResult) {
        let schema = SchemaBuilder::new("publication")
            .v3()
            .field("ts", FieldType::I64, FieldRole::Timestamp)
            .finish()
            .unwrap();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 1,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::I64(vec![42]),
            }],
        };
        let schema_fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
        let logical_sha256 =
            canonical_v3_batch_sha256(&schema, &batch, V3ValueLimits::default()).unwrap();
        let block = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
        let block_sha256 = Sha256::digest(&block).into();
        (
            schema,
            ShadowEncodeResult {
                schema_fingerprint,
                row_count: 1,
                logical_sha256,
                block,
                block_sha256,
            },
        )
    }

    fn v3_reference() -> (aura_codec::SchemaDescriptor, AuraV3Batch) {
        let schema = SchemaBuilder::new("v3-publication")
            .v3()
            .field("ts", FieldType::I64, FieldRole::Timestamp)
            .finish()
            .unwrap();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 1,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::I64(vec![42]),
            }],
        };
        (schema, batch)
    }

    fn grouped_v3_reference() -> (aura_codec::SchemaDescriptor, AuraV3EventBatch) {
        let schema = SchemaBuilder::new("grouped-v3-publication")
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .repeated_field("side", FieldType::U8, FieldRole::Side)
            .repeated_field("price", FieldType::I64, FieldRole::Price)
            .dual_domain_repeated_group(
                1,
                vec![1, 2],
                1,
                RelationshipPermissions::none()
                    .with_split()
                    .with_within_domain(),
            )
            .finish()
            .unwrap();
        let groups = schema.groups.clone();
        let schema = schema.with_v3_groups(groups).unwrap();
        let batch = AuraV3EventBatch {
            schema_id: schema.schema_id,
            event_count: 1,
            child_offsets: vec![0, 1],
            event_columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::TimestampMs(vec![42]),
            }],
            repeated_columns: vec![
                AuraV3Column {
                    slot: 1,
                    validity: None,
                    values: AuraV3ColumnValues::U8(vec![0]),
                },
                AuraV3Column {
                    slot: 2,
                    validity: None,
                    values: AuraV3ColumnValues::I64(vec![100]),
                },
            ],
        };
        (schema, batch)
    }

    #[test]
    fn directory_open_failure_creates_nothing() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_open: true,
            ..Default::default()
        };
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn hardlink_collision_preserves_existing_and_removes_owned_temp() {
        let dir = TestDirectory::new();
        fs::write(dir.output(), b"winner").unwrap();
        let (schema, result) = reference();
        let ops = FaultOps::default();
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert_eq!(fs::read(dir.output()).unwrap(), b"winner");
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn hardlink_collision_with_unlink_failure_requires_coordinator_cleanup() {
        let dir = TestDirectory::new();
        fs::write(dir.output(), b"winner").unwrap();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_temp_remove_before_link: true,
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_eq!(
            shadow_safe_error(&error.0).0,
            "publication_cleanup_required"
        );
        assert_eq!(fs::read(dir.output()).unwrap(), b"winner");
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), result.block);
        assert_eq!(
            fs::metadata(&temps[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn replaced_temp_identity_is_not_removed_or_published() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            replace_temp_before_link: true,
            ..Default::default()
        };
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert!(!dir.output().exists());
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), b"replacement");
    }

    #[test]
    fn v3_replaced_temp_identity_is_not_removed_or_published() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let ops = FaultOps {
            replace_temp_before_link: true,
            ..Default::default()
        };
        assert!(publish_verified_v3_flat_with_ops(&dir.output(), &schema, &batch, &ops).is_err());
        assert!(!dir.output().exists());
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), b"replacement");
    }

    #[test]
    fn grouped_v3_uses_shared_publication_identity_guard() {
        let dir = TestDirectory::new();
        let (schema, batch) = grouped_v3_reference();
        let ops = FaultOps {
            replace_temp_before_link: true,
            ..Default::default()
        };
        assert!(
            publish_verified_v3_grouped_with_ops(&dir.output(), &schema, &batch, &ops).is_err()
        );
        assert!(!dir.output().exists());
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), b"replacement");
    }

    #[test]
    fn planned_v3_uses_shared_publication_identity_guard() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let artifact =
            compile_v3_planned_flat(&schema, std::slice::from_ref(&batch), planned_cli_limits())
                .unwrap();
        let selection = planned_flat_selection(&artifact).unwrap();
        let ops = FaultOps {
            replace_temp_before_link: true,
            ..Default::default()
        };
        assert!(
            publish_verified_v3_planned_with_ops(&dir.output(), &artifact, selection, &ops)
                .is_err()
        );
        assert!(!dir.output().exists());
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), b"replacement");
    }

    #[test]
    fn complete_v3_commit_fsync_failure_rolls_back() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            ..Default::default()
        };
        let error =
            publish_verified_v3_flat_with_ops(&dir.output(), &schema, &batch, &ops).unwrap_err();
        assert_ne!(v3_safe_error(&error.0).0, "publication_ambiguous");
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn complete_v3_rollback_failure_is_ambiguous() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            fail_all_remove_after_link: true,
            ..Default::default()
        };
        let error =
            publish_verified_v3_flat_with_ops(&dir.output(), &schema, &batch, &ops).unwrap_err();
        assert_eq!(v3_safe_error(&error.0).0, "publication_ambiguous");
        assert!(dir.output().exists());
    }

    #[test]
    fn complete_v3_committed_cleanup_fault_sets_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let ops = FaultOps {
            fail_temp_remove_after_link: true,
            ..Default::default()
        };
        let (outcome, _, _) =
            publish_verified_v3_flat_with_ops(&dir.output(), &schema, &batch, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert!(dir.output().exists());
        assert_eq!(dir.temp_paths().len(), 1);
    }

    #[test]
    fn complete_v3_hardlink_collision_preserves_winner() {
        let dir = TestDirectory::new();
        fs::write(dir.output(), b"winner").unwrap();
        let (schema, batch) = v3_reference();
        assert!(publish_verified_v3_flat_with_ops(
            &dir.output(),
            &schema,
            &batch,
            &FaultOps::default()
        )
        .is_err());
        assert_eq!(fs::read(dir.output()).unwrap(), b"winner");
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn complete_v3_cleanup_fsync_fault_sets_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, batch) = v3_reference();
        let ops = FaultOps {
            fail_sync_call: Some(2),
            ..Default::default()
        };
        let (outcome, _, _) =
            publish_verified_v3_flat_with_ops(&dir.output(), &schema, &batch, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert!(dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn exact_file_hash_rejects_append_before_and_during_hash() {
        let dir = TestDirectory::new();
        let path = dir.0.join("hash.aura0");
        fs::write(&path, b"abc").unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut appender = OpenOptions::new().append(true).open(&path).unwrap();
        appender.write_all(b"d").unwrap();
        assert!(hash_file_exact(&mut file, 3).is_err());

        file.set_len(3).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(hash_file_exact_with_hook(&mut file, 3, || {
            appender.write_all(b"d").unwrap();
            appender.flush().unwrap();
        })
        .is_err());
    }

    #[test]
    fn v3_stale_committed_result_code_is_distinct() {
        assert_eq!(
            v3_safe_error("publication_committed_result_unavailable_cleanup_required").0,
            "publication_committed_result_unavailable_cleanup_required"
        );
        assert_eq!(
            v3_safe_error("publication committed result unavailable").0,
            "publication_committed_result_unavailable"
        );
    }

    #[test]
    fn commit_fsync_failure_rolls_back_to_no_final() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_ne!(shadow_safe_error(&error.0).0, "publication_ambiguous");
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn rollback_failure_reports_publication_ambiguous_with_final_present() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            fail_all_remove_after_link: true,
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_eq!(shadow_safe_error(&error.0).0, "publication_ambiguous");
        assert!(dir.output().exists());
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
    }

    #[test]
    fn committed_final_survives_temp_unlink_failure_with_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_temp_remove_after_link: true,
            ..Default::default()
        };
        let outcome =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
        assert_eq!(dir.temp_paths().len(), 1);
    }

    #[test]
    fn cleanup_fsync_failure_reports_success_with_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(2),
            ..Default::default()
        };
        let outcome =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn group_or_world_writable_parent_is_rejected_before_temp_creation() {
        let dir = TestDirectory::new();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o770)).unwrap();
        let (schema, result) = reference();
        assert!(publish_verified_block_with_ops(
            &dir.output(),
            &schema,
            &result,
            &FaultOps::default()
        )
        .is_err());
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }
}
