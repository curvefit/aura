use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use arrow::array::{Int64Array, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use aura_codec::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, canonicalize_schema_json,
    decode_v3_value_block, parse_schema_json, FieldRole, FieldType, RelationshipPermissions,
    SchemaBuilder, ShadowProtocolLimits, V3ValueLimits, MAX_SCHEMA_JSON_BYTES,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const MINIMAL: &str = r#"{
  "schema_format": "aura-schema",
  "schema_version": 1,
  "schema_encoding": "v3",
  "name": "cli_schema",
  "fields": [
    {
      "id": 0,
      "name": "ts",
      "type": "timestamp_ns",
      "role": "timestamp",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["absolute", "delta_base", "delta_previous", "fixed_step", "rough_step"]
    },
    {
      "id": 1,
      "name": "value",
      "type": "i64",
      "role": "value",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["absolute", "delta_base", "delta_previous", "delta2", "midpoint", "zigzag_varint", "bitpack"]
    }
  ],
  "groups": [],
  "derived_expressions": []
}"#;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aura-cli-test-{}-{suffix}", std::process::id()));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ignored = fs::remove_dir_all(&self.0);
    }
}

fn aura(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aura"))
        .args(args)
        .output()
        .unwrap()
}

fn aura_with_stdin(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

fn minimal_ipc() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("value", DataType::Int64, false),
    ]));
    let first = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(TimestampNanosecondArray::from(vec![1, 2])),
            Arc::new(Int64Array::from(vec![10, 20])),
        ],
    )
    .unwrap();
    let second = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(TimestampNanosecondArray::from(vec![3])),
            Arc::new(Int64Array::from(vec![30])),
        ],
    )
    .unwrap();
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.write(&first).unwrap();
    writer.write(&second).unwrap();
    writer.finish().unwrap();
    drop(writer);
    bytes
}

#[test]
fn help_and_future_subcommands_are_clear() {
    let help = aura(&["--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).unwrap();
    assert!(stdout.contains("aura schema validate"));
    assert!(stdout.contains("aura schema inspect"));
    assert!(stdout.contains("aura schema canonicalize"));
    assert!(stdout.contains("aura shadow verify"));

    let nested_help = aura(&["schema", "validate", "--help"]);
    assert!(nested_help.status.success());

    let unsupported = aura(&["encode"]);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8(unsupported.stderr)
        .unwrap()
        .contains("unsupported"));
}

fn grouped_schema_json() -> String {
    SchemaBuilder::new("cli_grouped_schema")
        .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .dual_domain_repeated_group(
            1,
            vec![2, 3, 4],
            2,
            RelationshipPermissions::none()
                .with_split()
                .with_within_domain()
                .with_across_domain_same_field()
                .with_joint_same_field(),
        )
        .finish()
        .unwrap()
        .to_canonical_json()
        .unwrap()
}

fn assert_schema_inspection(source: &str, value: &serde_json::Value) {
    let schema = parse_schema_json(source).unwrap();
    let canonical = canonicalize_schema_json(source).unwrap();
    let canonical_hash: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
    let fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
    let mapping = schema.compact_schema_map.as_ref().unwrap();
    assert_json_keys(
        value,
        &[
            "result_schema",
            "valid",
            "schema_format",
            "schema_format_version",
            "schema_encoding",
            "schema_encoding_version",
            "schema_id",
            "name",
            "schema_fingerprint_sha256",
            "canonical_json_sha256",
            "compact_schema_map",
            "compact_schema_map_hex",
            "field_count",
            "group_count",
            "dual_domain_discriminators",
            "build",
        ],
    );
    assert_eq!(value["result_schema"], "aura-schema-inspect-v1");
    assert_eq!(value["valid"], true);
    assert_eq!(value["schema_format"], "aura-schema");
    assert_eq!(value["schema_format_version"], 1);
    assert_eq!(value["schema_encoding"], "v3");
    assert_eq!(value["schema_encoding_version"], 3);
    assert_eq!(value["schema_id"], schema.schema_id);
    assert_eq!(value["name"], schema.name);
    assert_eq!(value["schema_fingerprint_sha256"], test_hex(&fingerprint));
    assert_eq!(value["canonical_json_sha256"], test_hex(&canonical_hash));
    assert_eq!(
        value["compact_schema_map"],
        serde_json::to_value(mapping).unwrap()
    );
    assert_eq!(value["compact_schema_map_hex"], test_hex(mapping));
    assert_eq!(value["field_count"], schema.fields.len());
    assert_eq!(value["group_count"], schema.groups.len());
    assert_json_keys(
        &value["build"],
        &[
            "package_version",
            "git_commit",
            "dirty",
            "provenance_source",
            "cargo_lock_sha256",
        ],
    );
    assert_eq!(
        value["build"]["cargo_lock_sha256"].as_str().unwrap().len(),
        64
    );
}

#[test]
fn schema_inspect_flat_is_deterministic_hashed_and_read_only() {
    let dir = TestDir::new();
    let input = dir.path("flat.json");
    fs::write(&input, format!("\n{MINIMAL}\n")).unwrap();
    let before = fs::read_dir(&dir.0).unwrap().count();
    let args = [
        "schema",
        "inspect",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ];
    let first = aura(&args);
    let second = aura(&args);
    assert!(first.status.success());
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(before, fs::read_dir(&dir.0).unwrap().count());
    assert_eq!(
        fs::read_to_string(&input).unwrap(),
        format!("\n{MINIMAL}\n")
    );
    let value: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_schema_inspection(MINIMAL, &value);
    assert_eq!(
        value["dual_domain_discriminators"],
        serde_json::Value::Array(Vec::new())
    );
    assert_no_temp_files(&dir);
}

#[test]
fn schema_inspect_grouped_exposes_actual_marker_200() {
    let dir = TestDir::new();
    let input = dir.path("grouped.json");
    let source = grouped_schema_json();
    fs::write(&input, &source).unwrap();
    let result = aura(&[
        "schema",
        "inspect",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_schema_inspection(&source, &value);
    assert_eq!(value["compact_schema_map"][2], 200);
    let discriminators = value["dual_domain_discriminators"].as_array().unwrap();
    assert_eq!(discriminators.len(), 1);
    assert_json_keys(
        &discriminators[0],
        &[
            "group_id",
            "domain_count",
            "discriminator_slot",
            "schema_map_byte",
        ],
    );
    assert_eq!(discriminators[0]["group_id"], 1);
    assert_eq!(discriminators[0]["domain_count"], 2);
    assert_eq!(discriminators[0]["discriminator_slot"], 2);
    assert_eq!(discriminators[0]["schema_map_byte"], 200);
    assert_no_temp_files(&dir);
}

#[test]
fn schema_inspect_errors_are_stable_sanitized_and_side_effect_free() {
    let dir = TestDir::new();
    let malformed = dir.path("malformed.json");
    let oversize = dir.path("oversize.json");
    fs::write(&malformed, r#"{"secret":"DO_NOT_LEAK_INSPECT_CONTENT"}"#).unwrap();
    fs::File::create(&oversize)
        .unwrap()
        .set_len((MAX_SCHEMA_JSON_BYTES + 1) as u64)
        .unwrap();
    for input in [&malformed, &oversize, &dir.0] {
        let result = aura(&[
            "schema",
            "inspect",
            "--input",
            input.to_str().unwrap(),
            "--json",
        ]);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_json_keys(&error, &["error_schema", "valid", "code", "error"]);
        assert_eq!(error["error_schema"], "aura-schema-inspect-error-v1");
        assert_eq!(error["valid"], false);
        assert_eq!(error["code"], "schema_inspect_failed");
        assert_eq!(error["error"], "schema inspection failed");
        assert!(!String::from_utf8_lossy(&result.stderr).contains("DO_NOT_LEAK"));
    }
    let future = aura(&["schema", "inspect", "--future", "value", "--json"]);
    assert!(!future.status.success());
    let error: serde_json::Value = serde_json::from_slice(&future.stderr).unwrap();
    assert_eq!(error["error_schema"], "aura-schema-inspect-error-v1");
    let missing_json = aura(&["schema", "inspect", "--input", malformed.to_str().unwrap()]);
    assert!(!missing_json.status.success());
    assert!(serde_json::from_slice::<serde_json::Value>(&missing_json.stderr).is_ok());
    assert_no_temp_files(&dir);
}

#[cfg(unix)]
#[test]
fn schema_inspect_rejects_symlink_input_without_writes() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new();
    let target = dir.path("target.json");
    let input = dir.path("input.json");
    fs::write(&target, MINIMAL).unwrap();
    symlink(&target, &input).unwrap();
    let result = aura(&[
        "schema",
        "inspect",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error_schema"], "aura-schema-inspect-error-v1");
    assert!(fs::symlink_metadata(input)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(target).unwrap(), MINIMAL);
    assert_no_temp_files(&dir);
}

#[test]
fn schema_validate_supports_text_and_json_results() {
    let dir = TestDir::new();
    let input = dir.path("schema.json");
    fs::write(&input, MINIMAL).unwrap();

    let text = aura(&["schema", "validate", "--input", input.to_str().unwrap()]);
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout)
        .unwrap()
        .contains("valid Aura schema \"cli_schema\""));

    let json_output = aura(&[
        "schema",
        "validate",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(json_output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(value["valid"], true);
    assert_eq!(value["name"], "cli_schema");
    assert_eq!(value["field_count"], 2);
    assert_eq!(value["group_count"], 0);
    assert!(value["schema_id"].as_u64().is_some());
}

#[test]
fn canonicalize_writes_exact_stdout_or_explicit_file() {
    let dir = TestDir::new();
    let input = dir.path("schema.json");
    let output = dir.path("canonical.json");
    fs::write(&input, format!("\n {MINIMAL} \n")).unwrap();
    let expected = canonicalize_schema_json(MINIMAL).unwrap();

    let stdout = aura(&["schema", "canonicalize", "--input", input.to_str().unwrap()]);
    assert!(stdout.status.success());
    assert_eq!(stdout.stdout, expected.as_bytes());

    fs::write(&output, "replace me").unwrap();
    let file = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(file.status.success());
    assert!(file.stdout.is_empty());
    assert_eq!(fs::read_to_string(output).unwrap(), expected);
    assert_no_temp_files(&dir);
}

#[test]
fn canonicalize_is_safe_when_input_and_output_are_the_same_file() {
    let dir = TestDir::new();
    let path = dir.path("same.json");
    fs::write(&path, MINIMAL).unwrap();
    let expected = canonicalize_schema_json(MINIMAL).unwrap();
    let result = aura(&[
        "schema",
        "canonicalize",
        "--input",
        path.to_str().unwrap(),
        "--output",
        path.to_str().unwrap(),
    ]);
    assert!(result.status.success());
    assert_eq!(fs::read_to_string(path).unwrap(), expected);
    assert_no_temp_files(&dir);
}

#[test]
fn validation_errors_are_nonzero_and_do_not_overwrite_output() {
    let dir = TestDir::new();
    let input = dir.path("invalid.json");
    let output = dir.path("existing.json");
    fs::write(&input, r#"{"schema_format":"not-aura"}"#).unwrap();
    fs::write(&output, "preserve me").unwrap();

    let failed = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!failed.status.success());
    assert!(!failed.stderr.is_empty());
    assert_eq!(fs::read_to_string(output).unwrap(), "preserve me");
    assert_no_temp_files(&dir);

    let json_error = aura(&[
        "schema",
        "validate",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(!json_error.status.success());
    let error: serde_json::Value = serde_json::from_slice(&json_error.stderr).unwrap();
    assert_eq!(error["valid"], false);
    assert!(error["error"].as_str().is_some());
}

#[cfg(unix)]
#[test]
fn canonicalize_rejects_output_symlinks_without_touching_target_or_temp() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new();
    let input = dir.path("schema.json");
    let target = dir.path("target.json");
    let output = dir.path("output.json");
    fs::write(&input, MINIMAL).unwrap();
    fs::write(&target, "target stays").unwrap();
    symlink(&target, &output).unwrap();
    let failed = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!failed.status.success());
    assert!(fs::symlink_metadata(&output)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(target).unwrap(), "target stays");
    assert_no_temp_files(&dir);
}

#[test]
fn validation_text_escapes_untrusted_schema_names() {
    let dir = TestDir::new();
    let input = dir.path("escaped.json");
    fs::write(&input, MINIMAL.replace("cli_schema", "line\\nbreak")).unwrap();
    let result = aura(&["schema", "validate", "--input", input.to_str().unwrap()]);
    assert!(result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("\"line\\nbreak\""));
    assert_eq!(1, stdout.lines().count());
}

#[test]
fn schema_input_must_be_a_regular_file() {
    let dir = TestDir::new();
    let failed = aura(&["schema", "validate", "--input", dir.0.to_str().unwrap()]);
    assert!(!failed.status.success());
    assert!(String::from_utf8(failed.stderr)
        .unwrap()
        .contains("regular file"));
}

#[test]
fn shadow_handshake_is_stable_json_and_has_no_side_effects() {
    let result = aura(&[
        "shadow",
        "handshake",
        "--protocol",
        "aura-logical-arrow-ipc-v1",
        "--json",
    ]);
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_json_keys(
        &value,
        &[
            "handshake_schema",
            "package",
            "package_version",
            "git_commit",
            "dirty",
            "provenance_source",
            "protocols",
            "schema_formats",
            "artifact_kinds",
            "operations",
            "complete_container_targets",
            "hash_contracts",
            "arrow",
            "arrow_crate_version",
            "cargo_lock_sha256",
        ],
    );
    assert_eq!(value["handshake_schema"], "aura-shadow-handshake-v1");
    assert_eq!(value["package"], "aura-codec");
    assert_eq!(
        value["protocols"],
        serde_json::json!(["aura-logical-arrow-ipc-v1", "aura-logical-arrow-ipc-v2"])
    );
    assert_eq!(
        value["operations"],
        serde_json::json!(["encode", "verify", "v3-aura0-seal", "v3-aura0-verify"])
    );
    assert_eq!(
        value["complete_container_targets"],
        serde_json::json!(["flat-aura0-v3-v1"])
    );
    assert!(value["package_version"].is_string());
    assert!(value["arrow_crate_version"].is_string());
    assert_eq!(value["cargo_lock_sha256"].as_str().unwrap().len(), 64);
    assert_json_keys(
        &value["hash_contracts"],
        &[
            "schema_fingerprint",
            "logical_values",
            "logical_events",
            "artifact",
        ],
    );
    assert_json_keys(&value["arrow"], &["rust_version", "protocol"]);
    assert_eq!(
        value["schema_formats"],
        serde_json::json!(["aura-schema-json-v1"])
    );
    assert_eq!(
        value["artifact_kinds"],
        serde_json::json!([
            "standalone-aura-v3-value-block-v1",
            "standalone-aura-v3-event-block-v1"
        ])
    );
    assert!(matches!(
        value["provenance_source"].as_str(),
        Some("git-informational" | "override-untrusted" | "unavailable")
    ));
    if let Some(commit) = value["git_commit"].as_str() {
        assert_eq!(commit.len(), 40);
        assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(value["dirty"].is_boolean());
    } else {
        assert!(value["dirty"].is_null());
    }
}

#[test]
fn v3_aura0_seal_from_arrow_stdin_and_verify_embedded_schema() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("events.aura0");
    fs::write(&schema_path, canonicalize_schema_json(MINIMAL).unwrap()).unwrap();
    let sealed = aura_with_stdin(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    let seal_json: serde_json::Value = serde_json::from_slice(&sealed.stdout).unwrap();
    assert_eq!(seal_json["container_target"], "flat-aura0-v3-v1");
    assert_eq!(seal_json["complete_aura_file"], true);
    assert_eq!(seal_json["container_version"], 3);
    assert_eq!(seal_json["profile"], "aura0");
    assert_eq!(seal_json["body_encoding"], "flat_exact_blocks_v1");
    assert_eq!(seal_json["footer_layout_version"], 1);
    assert!(seal_json["build"]["cargo_lock_sha256"].is_string());
    assert_eq!(seal_json["row_count"], 3);
    assert_eq!(seal_json["chunk_count"], 1);
    assert_eq!(seal_json["stale_temp_cleanup_required"], false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let verified = aura(&[
        "v3",
        "aura0",
        "verify",
        "--input",
        output.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verify_json: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verify_json["verified"], true);
    assert_eq!(verify_json["complete_aura_file"], true);
    assert_eq!(verify_json["protocol"], "aura-logical-arrow-ipc-v1");
    assert_eq!(verify_json["body_encoding"], "flat_exact_blocks_v1");
    assert_eq!(verify_json["footer_layout_version"], 1);
    assert!(verify_json["build"]["cargo_lock_sha256"].is_string());
    assert_eq!(verify_json["row_count"], 3);
    assert_eq!(verify_json["logical_sha256"], seal_json["logical_sha256"]);
    assert_eq!(verify_json["artifact_sha256"], seal_json["artifact_sha256"]);

    let valid_bytes = fs::read(&output).unwrap();
    for (name, corrupt) in [
        ("bad-magic.aura0", {
            let mut bytes = valid_bytes.clone();
            bytes[0] ^= 1;
            bytes
        }),
        ("bad-version.aura0", {
            let mut bytes = valid_bytes.clone();
            bytes[4] ^= 1;
            bytes
        }),
        ("bad-body.aura0", {
            let mut bytes = valid_bytes.clone();
            let header = aura_codec::AuraHeader::encoded_len(&bytes).unwrap();
            bytes[header + 70] ^= 1;
            bytes
        }),
        (
            "truncated.aura0",
            valid_bytes[..valid_bytes.len() - 1].to_vec(),
        ),
    ] {
        let path = dir.path(name);
        fs::write(&path, corrupt).unwrap();
        let rejected = aura(&[
            "v3",
            "aura0",
            "verify",
            "--input",
            path.to_str().unwrap(),
            "--json",
        ]);
        assert!(!rejected.status.success());
        let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
        assert_eq!(error["error_schema"], "aura-v3-flat-error-v1");
        assert_eq!(error["code"], "invalid_artifact", "{name}");
    }

    let collision = aura_with_stdin(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(!collision.status.success());
    let collision_error: serde_json::Value = serde_json::from_slice(&collision.stderr).unwrap();
    assert_eq!(collision_error["error_schema"], "aura-v3-flat-error-v1");
    assert_eq!(collision_error["code"], "output_exists");
    assert_eq!(
        verify_json["file_bytes"],
        fs::metadata(&output).unwrap().len()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = dir.path("linked.aura0");
        symlink(&output, &link).unwrap();
        let rejected = aura(&[
            "v3",
            "aura0",
            "verify",
            "--input",
            link.to_str().unwrap(),
            "--json",
        ]);
        assert!(!rejected.status.success());
        let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
        assert_eq!(error["error_schema"], "aura-v3-flat-error-v1");
        assert_eq!(error["code"], "invalid_input_path");
    }
}

#[test]
fn shadow_encode_publishes_verified_create_once_block() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("values.aurav3vb");
    fs::write(&schema_path, MINIMAL).unwrap();
    let result = aura_with_stdin(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_json_keys(
        &json,
        &[
            "result_schema",
            "protocol",
            "artifact_kind",
            "complete_aura_file",
            "reference_block_version",
            "package_version",
            "schema_id",
            "schema_fingerprint_sha256",
            "row_count",
            "logical_sha256",
            "artifact_bytes",
            "artifact_sha256",
            "stale_temp_cleanup_required",
            "build",
        ],
    );
    assert_eq!(json["result_schema"], "aura-shadow-encode-result-v1");
    assert_eq!(json["complete_aura_file"], false);
    assert_eq!(json["reference_block_version"], 1);
    assert_eq!(json["row_count"], 3);
    assert_eq!(json["stale_temp_cleanup_required"], false);
    assert!(json["build"]["arrow_crate_version"].is_string());
    assert_json_keys(
        &json["build"],
        &[
            "git_commit",
            "dirty",
            "provenance_source",
            "arrow_crate_version",
            "cargo_lock_sha256",
        ],
    );
    assert_eq!(
        json["build"]["cargo_lock_sha256"].as_str().unwrap().len(),
        64
    );
    assert!(json.get("output").is_none());
    let bytes = fs::read(&output).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(json["artifact_bytes"], bytes.len());
    let schema = parse_schema_json(MINIMAL).unwrap();
    let decoded = decode_v3_value_block(&schema, &bytes, V3ValueLimits::default()).unwrap();
    assert_eq!(decoded.row_count, 3);
    assert_no_temp_files(&dir);

    let collision = aura_with_stdin(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(!collision.status.success());
    assert_eq!(fs::read(output).unwrap(), bytes);
    assert_no_temp_files(&dir);
}

#[test]
fn shadow_encode_supports_documented_bare_relative_paths() {
    let dir = TestDir::new();
    fs::write(dir.path("schema.json"), MINIMAL).unwrap();
    let run = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
            .current_dir(&dir.0)
            .args([
                "shadow",
                "encode",
                "--protocol",
                "aura-logical-arrow-ipc-v1",
                "--schema",
                "schema.json",
                "--artifact-kind",
                "standalone-aura-v3-value-block-v1",
                "--output",
                "values.aurav3vb",
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(&minimal_ipc());
        child.wait_with_output().unwrap()
    };

    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let schema = parse_schema_json(MINIMAL).unwrap();
    let artifact = fs::read(dir.path("values.aurav3vb")).unwrap();
    assert!(decode_v3_value_block(&schema, &artifact, Default::default()).is_ok());
    assert_no_temp_files(&dir);

    let collision = run();
    assert!(!collision.status.success());
    assert_eq!(fs::read(dir.path("values.aurav3vb")).unwrap(), artifact);
    assert_no_temp_files(&dir);
}

#[test]
fn shadow_encode_invalid_input_never_publishes_or_leaks_environment() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("values.aurav3vb");
    fs::write(&schema_path, MINIMAL).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_aura"));
    command
        .args([
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            output.to_str().unwrap(),
            "--json",
        ])
        .env("AURA_TEST_SECRET", "do-not-emit-this-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"not ipc").unwrap();
    let failed = child.wait_with_output().unwrap();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(!output.exists());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("do-not-emit-this-secret"));
    let error: serde_json::Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(error["error_schema"], "aura-shadow-error-v1");
    assert_no_temp_files(&dir);
}

#[test]
fn shadow_encode_rejects_dash_path_spellings() {
    let failed = aura_with_stdin(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            "-",
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            "-.aurav3vb",
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn shadow_stdout_failure_leaves_an_adopted_valid_artifact() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("values.aurav3vb");
    fs::write(&schema_path, MINIMAL).unwrap();
    let stdout = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
        .args([
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            output.to_str().unwrap(),
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&minimal_ipc())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let committed_error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(
        committed_error["code"],
        "publication_committed_result_unavailable"
    );
    let schema = parse_schema_json(MINIMAL).unwrap();
    let bytes = fs::read(&output).unwrap();
    let decoded = decode_v3_value_block(&schema, &bytes, Default::default()).unwrap();
    let verify = aura(&[
        "shadow",
        "verify",
        "--protocol",
        "aura-logical-arrow-ipc-v1",
        "--schema",
        schema_path.to_str().unwrap(),
        "--input",
        output.to_str().unwrap(),
        "--json",
    ]);
    assert!(verify.status.success());
    assert!(verify.stderr.is_empty());
    let verified: serde_json::Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert_json_keys(
        &verified,
        &[
            "result_schema",
            "protocol",
            "artifact_kind",
            "complete_aura_file",
            "reference_block_version",
            "package_version",
            "schema_id",
            "schema_fingerprint_sha256",
            "row_count",
            "logical_sha256",
            "artifact_bytes",
            "artifact_sha256",
            "build",
        ],
    );
    assert_eq!(verified["result_schema"], "aura-shadow-verify-result-v1");
    assert_eq!(verified["protocol"], "aura-logical-arrow-ipc-v1");
    assert_eq!(
        verified["artifact_kind"],
        "standalone-aura-v3-value-block-v1"
    );
    assert_eq!(verified["complete_aura_file"], false);
    assert_eq!(verified["reference_block_version"], 1);
    assert!(verified["package_version"].is_string());
    assert!(verified["schema_id"].is_number());
    assert!(verified["artifact_bytes"].is_number());
    assert_json_keys(
        &verified["build"],
        &[
            "git_commit",
            "dirty",
            "provenance_source",
            "arrow_crate_version",
            "cargo_lock_sha256",
        ],
    );
    assert_eq!(verified["row_count"], decoded.row_count);
    assert_eq!(
        verified["schema_fingerprint_sha256"],
        test_hex(&canonical_v3_schema_fingerprint(&schema).unwrap())
    );
    assert_eq!(
        verified["logical_sha256"],
        test_hex(&canonical_v3_batch_sha256(&schema, &decoded, V3ValueLimits::default()).unwrap())
    );
    assert_eq!(
        verified["artifact_sha256"],
        test_hex(Sha256::digest(&bytes).as_ref())
    );
    assert_no_temp_files(&dir);
}

#[cfg(unix)]
#[test]
fn v3_stdout_failure_leaves_committed_verifiable_artifact() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("events.aura0");
    fs::write(&schema_path, canonicalize_schema_json(MINIMAL).unwrap()).unwrap();
    let stdout = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
        .args([
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&minimal_ipc())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error_schema"], "aura-v3-flat-error-v1");
    assert_eq!(error["code"], "publication_committed_result_unavailable");
    assert!(output.exists());
    let verified = aura(&[
        "v3",
        "aura0",
        "verify",
        "--input",
        output.to_str().unwrap(),
        "--json",
    ]);
    assert!(verified.status.success());
}

#[test]
fn shadow_verify_rejects_malformed_and_oversize_inputs_without_writes() {
    let dir = TestDir::new();
    let schema = dir.path("schema.json");
    let malformed = dir.path("malformed.aurav3vb");
    let oversize = dir.path("oversize.aurav3vb");
    fs::write(&schema, MINIMAL).unwrap();
    fs::write(&malformed, b"not a value block").unwrap();
    fs::File::create(&oversize)
        .unwrap()
        .set_len((ShadowProtocolLimits::default().values.max_block_bytes as u64) + 1)
        .unwrap();
    for input in [&malformed, &oversize] {
        let result = aura(&[
            "shadow",
            "verify",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema.to_str().unwrap(),
            "--input",
            input.to_str().unwrap(),
            "--json",
        ]);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
    assert_no_temp_files(&dir);
}

#[test]
fn shadow_verify_rejects_wrong_schema_dash_and_extension() {
    let dir = TestDir::new();
    let schema = dir.path("schema.json");
    let wrong_schema = dir.path("wrong-schema.json");
    let artifact = dir.path("values.aurav3vb");
    fs::write(&schema, MINIMAL).unwrap();
    fs::write(
        &wrong_schema,
        MINIMAL.replace("\"name\": \"value\"", "\"name\": \"different\""),
    )
    .unwrap();
    let encoded = aura_with_stdin(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            artifact.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(encoded.status.success());

    let wrong = aura(&[
        "shadow",
        "verify",
        "--protocol",
        "aura-logical-arrow-ipc-v1",
        "--schema",
        wrong_schema.to_str().unwrap(),
        "--input",
        artifact.to_str().unwrap(),
        "--json",
    ]);
    assert!(!wrong.status.success());
    assert!(wrong.stdout.is_empty());

    for input in ["-", "values.bin"] {
        let failed = aura(&[
            "shadow",
            "verify",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema.to_str().unwrap(),
            "--input",
            input,
            "--json",
        ]);
        assert!(!failed.status.success());
        assert!(failed.stdout.is_empty());
    }
    assert_no_temp_files(&dir);
}

#[cfg(unix)]
#[test]
fn shadow_verify_rejects_symlink_input() {
    use std::os::unix::fs::symlink;
    let dir = TestDir::new();
    let schema = dir.path("schema.json");
    let target = dir.path("target.aurav3vb");
    let input = dir.path("linked.aurav3vb");
    fs::write(&schema, MINIMAL).unwrap();
    fs::write(&target, b"target stays").unwrap();
    symlink(&target, &input).unwrap();
    let result = aura(&[
        "shadow",
        "verify",
        "--protocol",
        "aura-logical-arrow-ipc-v1",
        "--schema",
        schema.to_str().unwrap(),
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(!result.status.success());
    assert_eq!(fs::read(target).unwrap(), b"target stays");
    assert_no_temp_files(&dir);
}

#[cfg(unix)]
#[test]
fn shadow_encode_rejects_symlink_output() {
    use std::os::unix::fs::symlink;
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let target = dir.path("target.aurav3vb");
    let output = dir.path("values.aurav3vb");
    fs::write(&schema_path, MINIMAL).unwrap();
    fs::write(&target, b"preserve").unwrap();
    symlink(&target, &output).unwrap();
    let result = aura_with_stdin(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-value-block-v1",
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        &minimal_ipc(),
    );
    assert!(!result.status.success());
    assert_eq!(fs::read(target).unwrap(), b"preserve");
    assert_no_temp_files(&dir);
}

fn assert_no_temp_files(dir: &TestDir) {
    let names = fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(names.iter().all(|name| !name.contains(".aura-tmp-")));
}

fn assert_json_keys(value: &serde_json::Value, expected: &[&str]) {
    let actual = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = expected
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
}

fn test_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
