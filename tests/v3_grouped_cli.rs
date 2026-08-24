use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Int64Array, ListArray, StructArray, TimestampMillisecondArray, UInt8Array,
};
use arrow::buffer::{OffsetBuffer, ScalarBuffer};
use arrow::datatypes::{DataType, Field, Fields, Schema, TimeUnit};
use arrow::ipc::writer::{IpcWriteOptions, StreamWriter};
use arrow::ipc::MetadataVersion;
use arrow::record_batch::RecordBatch;
use aura_codec::{
    FieldRole, FieldType, RelationshipPermissions, SchemaBuilder, V3GroupedAura0Reader,
    MAX_V3_GROUPED_FOOTER_BYTES,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aura-v3-grouped-cli-{}-{}",
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

fn grouped_schema() -> aura_codec::SchemaDescriptor {
    let schema = SchemaBuilder::new("grouped_cli_complete")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .dual_domain_repeated_group(
            1,
            vec![1, 2, 3],
            1,
            RelationshipPermissions::none()
                .with_split()
                .with_within_domain(),
        )
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn arrow_schema() -> Arc<Schema> {
    let child_fields = Fields::from(vec![
        Field::new("side", DataType::UInt8, false),
        Field::new("price", DataType::Int64, false),
        Field::new("quantity", DataType::Int64, false),
    ]);
    let item = Arc::new(Field::new("item", DataType::Struct(child_fields), false));
    Arc::new(Schema::new(vec![
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("__aura_repeated_v1", DataType::List(item), false),
    ]))
}

fn record_batch(
    schema: Arc<Schema>,
    timestamps: Vec<i64>,
    offsets: Vec<i32>,
    sides: Vec<u8>,
    prices: Vec<i64>,
    quantities: Vec<i64>,
) -> RecordBatch {
    let list_field = schema.field(1);
    let DataType::List(item) = list_field.data_type() else {
        panic!("list field")
    };
    let DataType::Struct(child_fields) = item.data_type() else {
        panic!("struct item")
    };
    let children: Vec<ArrayRef> = vec![
        Arc::new(UInt8Array::from(sides)),
        Arc::new(Int64Array::from(prices)),
        Arc::new(Int64Array::from(quantities)),
    ];
    let values = StructArray::new(child_fields.clone(), children, None);
    let list = ListArray::new(
        item.clone(),
        OffsetBuffer::new(ScalarBuffer::from(offsets)),
        Arc::new(values),
        None,
    );
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(TimestampMillisecondArray::from(timestamps)),
            Arc::new(list),
        ],
    )
    .unwrap()
}

fn ipc(batches: Vec<RecordBatch>) -> Vec<u8> {
    let schema = arrow_schema();
    let options = IpcWriteOptions::try_new(64, false, MetadataVersion::V5).unwrap();
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new_with_options(&mut bytes, &schema, options).unwrap();
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
        child.stdin.take().unwrap().write_all(stdin).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn seal(dir: &TestDir, name: &str, input: &[u8]) -> (PathBuf, Output) {
    let schema_path = dir.path("schema.json");
    if !schema_path.exists() {
        fs::write(&schema_path, grouped_schema().to_canonical_json().unwrap()).unwrap();
    }
    let output = dir.path(name);
    let result = run(
        &[
            "v3",
            "aura0",
            "seal",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--json",
        ],
        Some(input),
    );
    (output, result)
}

fn verify(path: &std::path::Path) -> Output {
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
fn grouped_complete_cli_multibatch_seals_and_auto_verifies() {
    let dir = TestDir::new();
    let schema = arrow_schema();
    let input = ipc(vec![
        record_batch(
            schema.clone(),
            vec![10],
            vec![0, 1],
            vec![0],
            vec![100],
            vec![5],
        ),
        record_batch(
            schema,
            vec![20, 30],
            vec![0, 0, 2],
            vec![1, 0],
            vec![101, 102],
            vec![6, 7],
        ),
    ]);
    let (path, sealed) = seal(&dir, "events.aura0", &input);
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    let seal_json: serde_json::Value = serde_json::from_slice(&sealed.stdout).unwrap();
    assert_eq!(
        seal_json["result_schema"],
        "aura-v3-grouped-aura0-seal-result-v1"
    );
    assert_eq!(seal_json["protocol"], "aura-logical-arrow-ipc-v2");
    assert_eq!(seal_json["container_target"], "grouped-aura0-v3-exact-v1");
    assert_eq!(seal_json["body_encoding"], "grouped_exact_events_v1");
    assert_eq!(seal_json["body_layout_version"], 1);
    assert_eq!(seal_json["event_block_version"], 1);
    assert_eq!(seal_json["compression"], "none");
    assert_eq!(seal_json["event_count"], 3);
    assert_eq!(seal_json["child_count"], 3);
    assert_eq!(seal_json["chunk_count"], 1);
    assert_eq!(seal_json["file_bytes"], fs::metadata(&path).unwrap().len());
    assert_eq!(seal_json["stale_temp_cleanup_required"], false);

    let mut reader = V3GroupedAura0Reader::open(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(reader.embedded_footer().schema, grouped_schema());
    assert_eq!(reader.verify_all().unwrap().event_count, 3);

    let verified = verify(&path);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verify_json: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        verify_json["result_schema"],
        "aura-v3-grouped-aura0-verify-result-v1"
    );
    assert_eq!(verify_json["verified"], true);
    assert_eq!(verify_json["schema_id"], grouped_schema().schema_id);
    assert_eq!(verify_json["logical_sha256"], seal_json["logical_sha256"]);
    assert_eq!(verify_json["artifact_sha256"], seal_json["artifact_sha256"]);

    let committed = fs::read(&path).unwrap();
    let collision = seal(&dir, "events.aura0", &input).1;
    assert!(!collision.status.success());
    assert_eq!(fs::read(&path).unwrap(), committed);
}

#[test]
fn grouped_complete_cli_empty_and_zero_child_are_exact() {
    let dir = TestDir::new();
    let (empty_path, empty) = seal(&dir, "empty.aura0", &ipc(Vec::new()));
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let empty_json: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(empty_json["event_count"], 0);
    assert_eq!(empty_json["child_count"], 0);
    assert_eq!(empty_json["chunk_count"], 0);
    assert!(verify(&empty_path).status.success());

    let schema = arrow_schema();
    let zero_child_ipc = ipc(vec![record_batch(
        schema,
        vec![10, 20],
        vec![0, 0, 0],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )]);
    let (path, result) = seal(&dir, "zero-child.aura0", &zero_child_ipc);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["event_count"], 2);
    assert_eq!(value["child_count"], 0);
    assert_eq!(value["chunk_count"], 1);
    assert!(verify(&path).status.success());
}

#[test]
fn grouped_complete_cli_rejects_malformed_and_wrong_footer_tuple_without_side_effects() {
    let dir = TestDir::new();
    let (path, malformed) = seal(&dir, "malformed.aura0", b"not arrow ipc");
    assert!(!malformed.status.success());
    assert!(!path.exists());
    assert!(fs::read_dir(&dir.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("aura-tmp")));

    let schema = arrow_schema();
    let valid = ipc(vec![record_batch(
        schema,
        vec![10],
        vec![0, 0],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )]);
    let (valid_path, sealed) = seal(&dir, "valid.aura0", &valid);
    assert!(sealed.status.success());
    let mut bytes = fs::read(&valid_path).unwrap();
    let footer_len =
        u32::from_le_bytes(bytes[bytes.len() - 12..bytes.len() - 8].try_into().unwrap());
    let footer_start = bytes.len() - 12 - footer_len as usize;
    bytes[footer_start + 8] = 99;
    let wrong = dir.path("wrong-tuple.aura0");
    fs::write(&wrong, bytes).unwrap();
    let rejected = verify(&wrong);
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "invalid_artifact");

    let oversize = dir.path("oversize-footer.aura0");
    let mut oversized_bytes = vec![0u8; 24];
    let trailer = oversized_bytes.len() - 12;
    oversized_bytes[trailer..trailer + 4]
        .copy_from_slice(&((MAX_V3_GROUPED_FOOTER_BYTES as u32) + 1).to_le_bytes());
    oversized_bytes[trailer + 4..].copy_from_slice(b"sealed:)");
    fs::write(&oversize, oversized_bytes).unwrap();
    let rejected = verify(&oversize);
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "invalid_artifact");
}

#[cfg(unix)]
#[test]
fn grouped_complete_cli_stdout_loss_keeps_verifiable_create_once_file() {
    let dir = TestDir::new();
    let schema_path = dir.path("schema.json");
    let output = dir.path("stdout-loss.aura0");
    fs::write(&schema_path, grouped_schema().to_canonical_json().unwrap()).unwrap();
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
            "aura-logical-arrow-ipc-v2",
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
        .write_all(&ipc(Vec::new()))
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["code"], "publication_committed_result_unavailable");
    assert!(verify(&output).status.success());
}
