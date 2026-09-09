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
use aura_codec::experimental::{
    canonical_v3_event_batch_sha256, compile_shadow_grouped_arrow_ipc, decode_v3_event_block,
};
use aura_codec::{FieldRole, FieldType, RelationshipPermissions, SchemaBuilder};
use sha2::{Digest, Sha256};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aura-cli-v2-{}-{}",
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
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn schema() -> aura_codec::SchemaDescriptor {
    let schema = SchemaBuilder::new("cli_grouped_v2")
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

fn ipc() -> Vec<u8> {
    let child_fields = Fields::from(vec![
        Field::new("side", DataType::UInt8, false),
        Field::new("price", DataType::Int64, false),
        Field::new("quantity", DataType::Int64, false),
    ]);
    let item = Arc::new(Field::new(
        "item",
        DataType::Struct(child_fields.clone()),
        false,
    ));
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("__aura_repeated_v1", DataType::List(item.clone()), false),
    ]));
    let children: Vec<ArrayRef> = vec![
        Arc::new(UInt8Array::from(vec![0, 1, 0])),
        Arc::new(Int64Array::from(vec![100, 101, 102])),
        Arc::new(Int64Array::from(vec![0, 5, 6])),
    ];
    let values = StructArray::new(child_fields, children, None);
    let list = ListArray::new(
        item,
        OffsetBuffer::new(ScalarBuffer::from(vec![0, 1, 3])),
        Arc::new(values),
        None,
    );
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(TimestampMillisecondArray::from(vec![10, 20])),
            Arc::new(list),
        ],
    )
    .unwrap();
    let options = IpcWriteOptions::try_new(64, false, MetadataVersion::V5).unwrap();
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new_with_options(&mut bytes, &schema, options).unwrap();
    writer.write(&batch).unwrap();
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

#[test]
fn grouped_v2_cli_create_once_verify_and_schema_binding() {
    let dir = TestDir::new();
    let schema = schema();
    let schema_path = dir.0.join("schema.json");
    let other_path = dir.0.join("other.json");
    let artifact = dir.0.join("events.aurav3eb");
    fs::write(&schema_path, schema.to_canonical_json().unwrap()).unwrap();
    let other = SchemaBuilder::new("cli_grouped_v2_other")
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
    let groups = other.groups.clone();
    let other = other.with_v3_groups(groups).unwrap();
    fs::write(&other_path, other.to_canonical_json().unwrap()).unwrap();
    assert_eq!(
        schema.compact_schema_map.as_ref().unwrap()[1],
        200,
        "{:?}",
        schema
    );
    compile_shadow_grouped_arrow_ipc(&schema, std::io::Cursor::new(ipc()), Default::default())
        .unwrap();

    let encoded = run(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-event-block-v1",
            "--output",
            artifact.to_str().unwrap(),
            "--json",
        ],
        Some(&ipc()),
    );
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let encode_json: serde_json::Value = serde_json::from_slice(&encoded.stdout).unwrap();
    assert_eq!(encode_json["protocol"], "aura-logical-arrow-ipc-v2");
    assert_eq!(encode_json["complete_aura_file"], false);
    assert_eq!(encode_json["event_count"], 2);
    assert_eq!(encode_json["child_count"], 3);
    let bytes = fs::read(&artifact).unwrap();
    assert_eq!(&bytes[..8], b"AURAV3EB");
    assert_eq!(
        encode_json["artifact_sha256"],
        hex(Sha256::digest(&bytes).as_ref())
    );
    let decoded = decode_v3_event_block(&schema, &bytes, Default::default()).unwrap();
    assert_eq!(
        encode_json["logical_sha256"],
        hex(&canonical_v3_event_batch_sha256(&schema, &decoded, Default::default()).unwrap())
    );

    let verified = run(
        &[
            "shadow",
            "verify",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--input",
            artifact.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(verified.status.success());
    let verify_json: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        verify_json["result_schema"],
        "aura-shadow-grouped-verify-result-v1"
    );
    assert_eq!(verify_json["logical_sha256"], encode_json["logical_sha256"]);

    let collision = run(
        &[
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-event-block-v1",
            "--output",
            artifact.to_str().unwrap(),
            "--json",
        ],
        Some(&ipc()),
    );
    assert!(!collision.status.success());
    let substituted = run(
        &[
            "shadow",
            "verify",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            other_path.to_str().unwrap(),
            "--input",
            artifact.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(!substituted.status.success());
}

#[test]
fn grouped_v2_handshake_advertises_real_protocol_and_kind() {
    let result = run(
        &[
            "shadow",
            "handshake",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--json",
        ],
        None,
    );
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["protocols"],
        serde_json::json!(["aura-logical-arrow-ipc-v1", "aura-logical-arrow-ipc-v2"])
    );
    assert!(value["artifact_kinds"]
        .as_array()
        .unwrap()
        .contains(&serde_json::Value::from(
            "standalone-aura-v3-event-block-v1"
        )));
    assert_eq!(
        aura_codec::experimental::shadow_grouped_arrow_protocol(),
        "aura-logical-arrow-ipc-v2"
    );
}

#[cfg(unix)]
#[test]
fn grouped_v2_stdout_loss_leaves_verifiable_create_once_artifact() {
    let dir = TestDir::new();
    let schema_path = dir.0.join("schema.json");
    let artifact = dir.0.join("events.aurav3eb");
    fs::write(&schema_path, schema().to_canonical_json().unwrap()).unwrap();
    let stdout = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aura"))
        .args([
            "shadow",
            "encode",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--artifact-kind",
            "standalone-aura-v3-event-block-v1",
            "--output",
            artifact.to_str().unwrap(),
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&ipc()).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["code"], "publication_committed_result_unavailable");
    assert_eq!(&fs::read(&artifact).unwrap()[..8], b"AURAV3EB");
    let verified = run(
        &[
            "shadow",
            "verify",
            "--protocol",
            "aura-logical-arrow-ipc-v2",
            "--schema",
            schema_path.to_str().unwrap(),
            "--input",
            artifact.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(verified.status.success());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
