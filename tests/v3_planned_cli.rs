use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arrow::array::{Int64Array, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use aura_codec::{
    decode_v3_planned_flat, decode_v3_planned_flat_footer, encode_v3_planned_flat_footer,
    AuraHeader, FieldRole, FieldType, SchemaBuilder, V3FlatAura0Reader, V3FlatLimits,
    V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION, V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION,
};
use sha2::{Digest, Sha256};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aura-v3-planned-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
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
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn planned_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("anonymous_planned_cli")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("value", FieldType::I64, FieldRole::Value)
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap()
}

fn timestamp_only_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("anonymous_planned_fallback")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .finish()
        .unwrap()
}

fn planned_arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("value", DataType::Int64, false),
        Field::new("text", DataType::Utf8, false),
    ]))
}

fn timestamp_only_arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        "ts",
        DataType::Timestamp(TimeUnit::Nanosecond, None),
        false,
    )]))
}

fn planned_ipc(rows: usize) -> Vec<u8> {
    let schema = planned_arrow_schema();
    let midpoint = rows / 2;
    let make_batch = |start: usize, end: usize| {
        RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(TimestampNanosecondArray::from(
                    (start..end).map(|row| row as i64).collect::<Vec<_>>(),
                )),
                Arc::new(Int64Array::from(
                    (start..end)
                        .map(|row| (row % 5) as i64 - 2)
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    (start..end)
                        .map(|row| {
                            if row.is_multiple_of(3) {
                                "alpha"
                            } else {
                                "beta"
                            }
                        })
                        .collect::<Vec<_>>(),
                )),
            ],
        )
        .unwrap()
    };
    ipc(
        Arc::clone(&schema),
        if rows == 0 {
            Vec::new()
        } else {
            vec![make_batch(0, midpoint), make_batch(midpoint, rows)]
        },
    )
}

fn ipc(schema: Arc<Schema>, batches: Vec<RecordBatch>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    for batch in batches {
        writer.write(&batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn run(args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(stdin) = stdin {
        let _ = child.stdin.take().unwrap().write_all(stdin);
    }
    child.wait_with_output().unwrap()
}

fn seal(dir: &TestDir, name: &str, mode: Option<&str>, input: &[u8]) -> (PathBuf, Output) {
    let schema_path = dir.path("schema.json");
    if !schema_path.exists() {
        fs::write(&schema_path, planned_schema().to_canonical_json().unwrap()).unwrap();
    }
    let output = dir.path(name);
    let mut args = vec![
        "v3",
        "aura0",
        "seal",
        "--protocol",
        "aura-logical-arrow-ipc-v1",
    ];
    if let Some(mode) = mode {
        args.extend(["--mode", mode]);
    }
    args.extend([
        "--schema",
        schema_path.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--json",
    ]);
    let result = run(&args, Some(input));
    (output, result)
}

fn verify(path: &Path) -> Output {
    run(
        &[
            "v3",
            "aura0",
            "verify",
            "--input",
            path.to_str().unwrap(),
            "--json",
        ],
        None,
    )
}

#[test]
fn planned_v1_seal_selects_planned_tuple_and_auto_verifies() {
    let dir = TestDir::new();
    let input = planned_ipc(512);
    let (path, sealed) = seal(&dir, "planned.aura0", Some("planned"), &input);
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    let seal: serde_json::Value = serde_json::from_slice(&sealed.stdout).unwrap();
    assert_eq!(
        seal["result_schema"],
        "aura-v3-flat-aura0-planned-request-seal-result-v3"
    );
    assert_eq!(seal["requested_mode"], "planned");
    assert_eq!(seal["planner_requested"], true);
    assert_eq!(seal["selected_candidate"], "planned-flat-zstd19-wrapper");
    assert_eq!(seal["container_target"], "flat-aura0-v3-planned-v3");
    assert_eq!(seal["body_encoding"], "planned_flat_codecs_zstd19_v3");
    assert_eq!(seal["footer_layout_version"], 3);
    assert_eq!(seal["body_encoding_code"], 4);
    assert_eq!(seal["body_layout_version"], 3);
    assert_eq!(seal["block_version"], 3);
    assert_eq!(seal["compression"], "zstd");
    assert_eq!(seal["compression_level"], 19);
    assert_eq!(seal["wrapper_version"], 1);
    assert_eq!(seal["window_log"], 23);
    assert_eq!(seal["development_only"], true);
    assert_eq!(seal["streaming"], false);
    assert_eq!(seal["plan_sha256"].as_str().unwrap().len(), 64);
    let candidates = seal["planner_candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 5);
    assert_eq!(candidates[0]["candidate_id"], "exact-flat");
    assert_eq!(candidates[1]["candidate_id"], "planned-flat-fixed");
    assert_eq!(candidates[2]["candidate_id"], "planned-flat-integer-codecs");
    assert_eq!(
        candidates[3]["candidate_id"],
        "planned-flat-variable-dictionary"
    );
    assert_eq!(candidates[4]["candidate_id"], "planned-flat-zstd19-wrapper");
    assert!(candidates
        .iter()
        .all(|candidate| candidate["applicable"] == true && candidate["complete_bytes"].is_u64()));
    assert!(candidates.iter().all(|candidate| {
        if candidate["selected"] == true {
            candidate["rejection"].is_null()
        } else {
            candidate["rejection"] == "complete cost did not win"
        }
    }));
    let codecs = seal["physical_codecs"].as_array().unwrap();
    assert_eq!(codecs.len(), planned_schema().fields.len());
    assert_eq!(codecs[0]["slot"], 0);
    assert_eq!(codecs[0]["logical_field_type"], "timestamp_ns");
    assert!(codecs[0]["fixed_bytes"].is_u64());
    assert!(codecs[0]["varint_bytes"].is_u64());
    assert_eq!(
        codecs[0]["selected_physical_codec"],
        "signed_zigzag_uleb128"
    );
    assert_eq!(
        codecs[2]["selected_physical_codec"],
        "variable_byte_dictionary_bitpacked"
    );
    assert_eq!(codecs[2]["dictionary_selected"], true);
    assert!(
        codecs[2]["dictionary_bytes"].as_u64().unwrap()
            < codecs[2]["direct_bytes"].as_u64().unwrap()
    );
    let dictionary_codecs = seal["dictionary_candidate_codecs"].as_array().unwrap();
    assert_eq!(dictionary_codecs.len(), planned_schema().fields.len());
    assert_eq!(dictionary_codecs[2]["dictionary_entries"], 2);
    assert_eq!(dictionary_codecs[2]["max_chunk_dictionary_entries"], 2);
    assert_eq!(
        seal["zstd_candidate"]["base_candidate_id"],
        "planned-flat-variable-dictionary"
    );
    assert_eq!(seal["zstd_candidate"]["base_registry_version"], 2);
    assert_eq!(seal["zstd_candidate"]["wrapper_version"], 1);
    assert_eq!(
        seal["zstd_candidate"]["compressed_payload_bytes"]
            .as_u64()
            .unwrap()
            + seal["zstd_candidate"]["wrapper_overhead_bytes"]
                .as_u64()
                .unwrap(),
        seal["body_bytes"].as_u64().unwrap()
    );
    assert_eq!(seal["file_bytes"], fs::metadata(&path).unwrap().len());

    let bytes = fs::read(&path).unwrap();
    let decoded = decode_v3_planned_flat(&bytes, V3FlatLimits::DEFAULT_IN_MEMORY).unwrap();
    assert_eq!(decoded.footer.schema, planned_schema());
    assert_eq!(decoded.summary.row_count, 512);
    assert_eq!(
        seal["logical_sha256"],
        hex(&decoded.summary.global_logical_sha256)
    );
    assert_eq!(
        seal["schema_fingerprint_sha256"],
        hex(&decoded.summary.schema_fingerprint)
    );

    let verified = verify(&path);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verify: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        verify["result_schema"],
        "aura-v3-flat-aura0-planned-verify-result-v3"
    );
    assert_eq!(verify["verified"], true);
    assert_eq!(verify["footer_layout_version"], 3);
    assert_eq!(verify["body_encoding_code"], 4);
    assert_eq!(verify["body_layout_version"], 3);
    assert_eq!(verify["block_version"], 3);
    assert_eq!(verify["inner_body_layout_version"], 2);
    assert_eq!(verify["inner_block_version"], 2);
    assert_eq!(verify["compression"], "zstd");
    assert_eq!(verify["logical_sha256"], seal["logical_sha256"]);
    assert_eq!(
        verify["schema_fingerprint_sha256"],
        seal["schema_fingerprint_sha256"]
    );
    assert_eq!(verify["artifact_sha256"], seal["artifact_sha256"]);
    assert_eq!(verify["plan_sha256"], seal["plan_sha256"]);
}

#[test]
fn registry2_unwrapped_file_preserves_v2_verify_receipt() {
    let dir = TestDir::new();
    let input = planned_ipc(512);
    let (wrapped_path, sealed) = seal(&dir, "wrapped-source.aura0", Some("planned"), &input);
    assert!(sealed.status.success());
    let wrapped = fs::read(&wrapped_path).unwrap();
    let header_len = AuraHeader::encoded_len(&wrapped).unwrap();
    let footer = footer_slice(&wrapped);
    let footer_start = wrapped.len() - 12 - footer.len();
    let wrapper = &wrapped[header_len..footer_start];
    assert_eq!(&wrapper[..8], b"AUFPZB01");
    let mut decoder =
        zstd::stream::read::Decoder::new(std::io::Cursor::new(&wrapper[68..])).unwrap();
    decoder.window_log_max(23).unwrap();
    let mut inner = Vec::new();
    decoder.read_to_end(&mut inner).unwrap();

    let mut footer = decode_v3_planned_flat_footer(footer, V3FlatLimits::HARD).unwrap();
    footer.body_layout_version = V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION;
    footer.block_version = V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION;
    footer.body_len = inner.len() as u64;
    footer.body_sha256 = domain_hash(b"aura-v3-planned-flat-body-v1\0", &inner);
    footer.chunks[0].stored_len = inner.len() as u64;
    footer.chunks[0].stored_sha256 = Sha256::digest(&inner).into();
    let footer = encode_v3_planned_flat_footer(&footer, V3FlatLimits::HARD).unwrap();
    let mut unwrapped = wrapped[..header_len].to_vec();
    unwrapped.extend_from_slice(&inner);
    unwrapped.extend_from_slice(&footer);
    unwrapped.extend_from_slice(&(footer.len() as u32).to_le_bytes());
    unwrapped.extend_from_slice(b"sealed:)");
    let path = dir.path("registry2-unwrapped.aura0");
    fs::write(&path, unwrapped).unwrap();
    let verified = verify(&path);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        value["result_schema"],
        "aura-v3-flat-aura0-planned-verify-result-v2"
    );
    assert_eq!(value["body_layout_version"], 2);
    assert_eq!(value["compression"], "none");
}

#[test]
fn default_and_explicit_exact_modes_remain_byte_and_receipt_identical() {
    let dir = TestDir::new();
    let help = run(&["--help"], None);
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("--mode planned"));
    let input = planned_ipc(32);
    let (default_path, default) = seal(&dir, "default.aura0", None, &input);
    let (exact_path, exact) = seal(&dir, "exact.aura0", Some("exact"), &input);
    assert!(default.status.success());
    assert!(exact.status.success());
    assert_eq!(default.stdout, exact.stdout);
    assert_eq!(
        fs::read(default_path).unwrap(),
        fs::read(exact_path).unwrap()
    );
    let value: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(value["result_schema"], "aura-v3-flat-aura0-seal-result-v1");
    assert!(value.get("planner_requested").is_none());
}

#[test]
fn planned_request_exact_fallback_is_auditable_and_verifies_as_exact() {
    let dir = TestDir::new();
    let schema_path = dir.path("fallback-schema.json");
    let output = dir.path("fallback.aura0");
    fs::write(
        &schema_path,
        timestamp_only_schema().to_canonical_json().unwrap(),
    )
    .unwrap();
    let input = ipc(timestamp_only_arrow_schema(), Vec::new());
    let sealed = run(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--mode",
            "planned",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        Some(&input),
    );
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    let seal: serde_json::Value = serde_json::from_slice(&sealed.stdout).unwrap();
    assert_eq!(
        seal["result_schema"],
        "aura-v3-flat-aura0-planned-request-seal-result-v3"
    );
    assert_eq!(seal["selected_candidate"], "exact-flat");
    assert_eq!(seal["container_target"], "flat-aura0-v3-v1");
    assert_eq!(seal["footer_layout_version"], 1);
    assert_eq!(seal["body_encoding_code"], 1);
    assert!(seal["plan_sha256"].is_null());
    assert_eq!(seal["planner_candidates"].as_array().unwrap().len(), 5);
    assert!(seal["physical_codecs"].as_array().unwrap().is_empty());
    assert_eq!(seal["planner_candidates"][3]["applicable"], false);

    let verified = verify(&output);
    assert!(verified.status.success());
    let verify: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        verify["result_schema"],
        "aura-v3-flat-aura0-verify-result-v1"
    );
    assert_eq!(verify["logical_sha256"], seal["logical_sha256"]);
    assert_eq!(verify["artifact_sha256"], seal["artifact_sha256"]);
    let mut reader = V3FlatAura0Reader::open(fs::File::open(output).unwrap()).unwrap();
    assert_eq!(reader.verify_all().unwrap().record_count, 0);
}

#[test]
fn registry1_planned_file_preserves_v1_verify_receipt() {
    let dir = TestDir::new();
    let schema_path = dir.path("registry1-schema.json");
    let output = dir.path("registry1.aura0");
    fs::write(
        &schema_path,
        timestamp_only_schema().to_canonical_json().unwrap(),
    )
    .unwrap();
    let arrow_schema = timestamp_only_arrow_schema();
    let batch = RecordBatch::try_new(
        Arc::clone(&arrow_schema),
        vec![Arc::new(TimestampNanosecondArray::from(
            (-32..32).map(i64::from).collect::<Vec<_>>(),
        ))],
    )
    .unwrap();
    let sealed = run(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v1",
            "--mode",
            "planned",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        Some(&ipc(arrow_schema, vec![batch])),
    );
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    let seal: serde_json::Value = serde_json::from_slice(&sealed.stdout).unwrap();
    assert_eq!(
        seal["result_schema"],
        "aura-v3-flat-aura0-planned-request-seal-result-v3"
    );
    assert_eq!(seal["selected_candidate"], "planned-flat-integer-codecs");
    assert_eq!(seal["container_target"], "flat-aura0-v3-planned-v1");
    assert_eq!(seal["body_encoding"], "planned_flat_codecs_v1");
    assert_eq!(seal["body_layout_version"], 1);

    let verified = verify(&output);
    assert!(verified.status.success());
    let verify: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        verify["result_schema"],
        "aura-v3-flat-aura0-planned-verify-result-v1"
    );
    assert_eq!(verify["container_target"], "flat-aura0-v3-planned-v1");
    assert_eq!(verify["body_encoding"], "planned_flat_codecs_v1");
    assert_eq!(verify["logical_sha256"], seal["logical_sha256"]);
    assert_eq!(verify["artifact_sha256"], seal["artifact_sha256"]);
}

#[cfg(unix)]
#[test]
fn planned_cli_rejects_corruption_collision_symlink_and_grouped_mode() {
    let dir = TestDir::new();
    let input = planned_ipc(512);
    let (path, sealed) = seal(&dir, "valid.aura0", Some("planned"), &input);
    assert!(sealed.status.success());
    let committed = fs::read(&path).unwrap();

    let collision = seal(&dir, "valid.aura0", Some("planned"), &input).1;
    assert!(!collision.status.success());
    assert_eq!(fs::read(&path).unwrap(), committed);

    let mut corrupted = committed.clone();
    let middle = corrupted.len() / 2;
    corrupted[middle] ^= 1;
    let corrupt_path = dir.path("corrupt.aura0");
    fs::write(&corrupt_path, corrupted).unwrap();
    assert!(!verify(&corrupt_path).status.success());
    let truncated_path = dir.path("truncated.aura0");
    fs::write(&truncated_path, &committed[..committed.len() - 1]).unwrap();
    assert!(!verify(&truncated_path).status.success());

    let target = dir.path("target.aura0");
    let symlink = dir.path("symlink.aura0");
    fs::write(&target, b"winner").unwrap();
    std::os::unix::fs::symlink(&target, &symlink).unwrap();
    let rejected = seal(&dir, "symlink.aura0", Some("planned"), &input).1;
    assert!(!rejected.status.success());
    assert_eq!(fs::read(target).unwrap(), b"winner");

    let schema_path = dir.path("schema.json");
    let grouped_output = dir.path("grouped-planned.aura0");
    let grouped = run(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--mode",
            "planned",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            grouped_output.to_str().unwrap(),
            "--json",
        ],
        Some(&input),
    );
    assert!(!grouped.status.success());
    assert!(!grouped_output.exists());
}

#[cfg(unix)]
#[test]
fn planned_cli_stdout_loss_keeps_durable_verifiable_artifact() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("stdout-loss.aura0");
    fs::write(&schema_path, planned_schema().to_canonical_json().unwrap()).unwrap();
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
            "--mode",
            "planned",
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
        .write_all(&planned_ipc(512))
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["code"], "publication_committed_result_unavailable");
    let verified = verify(&output);
    assert!(verified.status.success());
    let value: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        value["result_schema"],
        "aura-v3-flat-aura0-planned-verify-result-v3"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn footer_slice(file: &[u8]) -> &[u8] {
    let offset = file.len() - 12;
    let length = u32::from_le_bytes(file[offset..offset + 4].try_into().unwrap()) as usize;
    &file[offset - length..offset]
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}
