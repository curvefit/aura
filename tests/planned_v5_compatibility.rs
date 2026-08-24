#![recursion_limit = "256"]

use std::fs;
use std::path::{Path, PathBuf};

use aura_codec::{
    canonical_v3_schema_fingerprint, compile_v3_planned_flat, decode_v3_planned_flat, AuraV3Batch,
    AuraV3Column, AuraV3ColumnValues as Values, AuraV3VariableColumn, FieldRole, FieldType,
    PlanV2PhysicalCodec, SchemaBuilder, V3FlatLimits,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const FIXTURE_DIR: &str = "tests/fixtures/v3-planned-v5";
const FIXTURE_FILE: &str = "prefix-suffix-layout7.aura0";
const SCHEMA_FILE: &str = "schema.json";
const MANIFEST_FILE: &str = "manifest.json";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(name)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn variable(parts: &[&[u8]]) -> AuraV3VariableColumn {
    let mut offsets = vec![0];
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
        offsets.push(data.len() as u32);
    }
    AuraV3VariableColumn { offsets, data }
}

fn fixture_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("compat-planned-v5-prefix-suffix")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .nullable_field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap()
}

fn fixture_batches(schema_id: u32) -> Vec<AuraV3Batch> {
    (0..2)
        .map(|chunk| {
            let start = chunk * 256;
            let text = (start..start + 256)
                .map(|row| format!("long-common-prefix-{row:05}-long-common-suffix"))
                .collect::<Vec<_>>();
            let refs = text
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>();
            AuraV3Batch {
                schema_id,
                row_count: 256,
                columns: vec![
                    AuraV3Column {
                        slot: 0,
                        validity: None,
                        values: Values::TimestampNs(
                            (start..start + 256).map(|row| row as i64).collect(),
                        ),
                    },
                    AuraV3Column {
                        slot: 1,
                        validity: Some(vec![0xff; 32]),
                        values: Values::Utf8(variable(&refs)),
                    },
                ],
            }
        })
        .collect()
}

fn without_schema_ids(mut batches: Vec<AuraV3Batch>) -> Vec<AuraV3Batch> {
    for batch in &mut batches {
        batch.schema_id = 0;
    }
    batches
}

fn fixture_manifest(
    schema: &aura_codec::SchemaDescriptor,
    artifact: &aura_codec::V3PlannedFlatArtifact,
    schema_json: &[u8],
) -> Value {
    let decoded = decode_v3_planned_flat(&artifact.bytes, V3FlatLimits::HARD).unwrap();
    let plan_bytes = decoded.footer.plan.encode(schema).unwrap();
    let header_len = decoded.summary.header_bytes as usize;
    let footer_start = artifact.bytes.len() - decoded.summary.footer_bytes as usize - 12;
    let body = &artifact.bytes[header_len..footer_start];
    json!({
        "$schema": "aura-v3-planned-v5-compatibility-fixture-v1",
        "manifest_version": 1,
        "compatibility_promise": "reader routing, schema/plan binding, bounded verification, logical reconstruction, and canonical hashes for one planned-flat registry-4/layout-7 artifact; planner costs and future writer selection are not frozen",
        "generation": {
            "test": "tests/planned_v5_compatibility.rs::generate_planned_v5_compatibility_fixture",
            "command": "AURA_PLANNED_V5_FIXTURE_GENERATE=1 cargo test --offline --test planned_v5_compatibility -- --ignored generate_planned_v5_compatibility_fixture",
            "aura_commit": "66b230ae22d71b61a30494a6538fd78130a8233f"
        },
        "schema": {
            "file": SCHEMA_FILE,
            "bytes": schema_json.len(),
            "sha256": sha256_hex(schema_json),
            "schema_id": schema.schema_id,
            "fingerprint_sha256": hex_bytes(&canonical_v3_schema_fingerprint(schema).unwrap())
        },
        "fixture": {
            "file": FIXTURE_FILE,
            "artifact_sha256": sha256_hex(&artifact.bytes),
            "file_bytes": artifact.bytes.len(),
            "container_version": 3,
            "footer_layout_version": 3,
            "body_encoding": 4,
            "body_layout_version": decoded.footer.body_layout_version,
            "block_version": decoded.footer.block_version,
            "wrapper_magic": "AUFPZB03",
            "wrapper_version": 3,
            "inner_magic": "AUFPVB04",
            "inner_body_layout_version": 6,
            "inner_block_version": 6,
            "plan_magic": "AUF2",
            "registry_version": decoded.footer.plan.registry_version,
            "physical_codec": "previous_common_prefix_suffix_bytes",
            "schema_id": decoded.footer.schema.schema_id,
            "schema_fingerprint_sha256": hex_bytes(&decoded.footer.schema_fingerprint),
            "plan_sha256": hex_bytes(&decoded.footer.plan_sha256),
            "header_sha256": hex_bytes(&decoded.footer.header_sha256),
            "body_sha256": hex_bytes(&decoded.footer.body_sha256),
            "global_logical_sha256": hex_bytes(&decoded.footer.global_logical_sha256),
            "plan_bytes": plan_bytes.len(),
            "record_count": decoded.footer.record_count,
            "chunk_count": decoded.footer.chunks.len(),
            "header_bytes": decoded.summary.header_bytes,
            "body_bytes": decoded.summary.body_bytes,
            "footer_bytes": decoded.summary.footer_bytes,
            "chunks": decoded.footer.chunks.iter().map(|chunk| json!({
                "chunk_id": chunk.chunk_id,
                "first_global_row": chunk.first_global_row,
                "row_count": chunk.row_count,
                "body_relative_offset": chunk.body_relative_offset,
                "stored_len": chunk.stored_len,
                "stored_sha256": hex_bytes(&chunk.stored_sha256),
                "logical_sha256": hex_bytes(&chunk.chunk_logical_sha256)
            })).collect::<Vec<_>>(),
            "body_rehash_sha256": sha256_hex(body)
        },
        "route_coverage": [
            {"registry": 1, "test": "v3_planned_cli::registry1_planned_file_preserves_v1_verify_receipt"},
            {"registry": 2, "test": "v3_planned_cli::registry2_file_preserves_v2_verify_receipt"},
            {"registry": 3, "test": "v3_planned_cli::planned_v1_seal_selects_planned_tuple_and_auto_verifies"},
            {"registry": 4, "test": "v3_planned_cli::prefix_suffix_seal_and_verify_use_v5_without_changing_prior_receipt_identities"}
        ]
    })
}

fn validate_fixture(schema: &aura_codec::SchemaDescriptor, artifact: &[u8], manifest: &Value) {
    let decoded = decode_v3_planned_flat(artifact, V3FlatLimits::HARD).unwrap();
    assert_eq!(decoded.footer.schema, *schema);
    assert_eq!(decoded.footer.body_layout_version, 7);
    assert_eq!(decoded.footer.block_version, 7);
    assert_eq!(decoded.footer.plan.registry_version, 4);
    assert_eq!(
        decoded.footer.plan.codecs[1],
        PlanV2PhysicalCodec::PreviousCommonPrefixSuffixBytes
    );
    let plan_bytes = decoded.footer.plan.encode(schema).unwrap();
    assert_eq!(&plan_bytes[..4], b"AUF2");
    assert_eq!(decoded.footer.chunks.len(), 2);
    assert_eq!(
        decoded
            .footer
            .chunks
            .iter()
            .map(|chunk| chunk.row_count)
            .sum::<u32>(),
        512
    );
    assert_eq!(decoded.summary.file_bytes, artifact.len() as u64);
    let header_len = decoded.summary.header_bytes as usize;
    let footer_start = artifact.len() - decoded.summary.footer_bytes as usize - 12;
    assert_eq!(&artifact[header_len..header_len + 8], b"AUFPZB03");
    assert_eq!(
        u16::from_le_bytes(
            artifact[header_len + 8..header_len + 10]
                .try_into()
                .unwrap()
        ),
        3
    );
    assert_eq!(&artifact[footer_start..footer_start + 4], b"AURP");
    assert_eq!(
        u16::from_le_bytes(
            artifact[footer_start + 4..footer_start + 6]
                .try_into()
                .unwrap()
        ),
        3
    );
    assert_eq!(
        u16::from_le_bytes(
            artifact[footer_start + 6..footer_start + 8]
                .try_into()
                .unwrap()
        ),
        3
    );
    assert_eq!(
        &artifact[footer_start + 8..footer_start + 12],
        &[4, 0, 0, 0]
    );
    assert_eq!(
        decoded.summary.global_logical_sha256,
        decoded.footer.global_logical_sha256
    );
    assert_eq!(manifest["fixture"]["body_encoding"], 4);
    assert_eq!(manifest["fixture"]["body_layout_version"], 7);
    assert_eq!(manifest["fixture"]["block_version"], 7);
    assert_eq!(manifest["fixture"]["registry_version"], 4);
    assert_eq!(manifest["fixture"]["wrapper_version"], 3);
    assert_eq!(manifest["fixture"]["plan_magic"], "AUF2");
    assert_eq!(manifest["fixture"]["plan_bytes"], plan_bytes.len());
    assert_eq!(manifest["fixture"]["file_bytes"], artifact.len());
    assert_eq!(manifest["fixture"]["artifact_sha256"], sha256_hex(artifact));
    assert_eq!(manifest["schema"]["schema_id"], schema.schema_id);
    assert_eq!(
        manifest["schema"]["fingerprint_sha256"],
        hex_bytes(&canonical_v3_schema_fingerprint(schema).unwrap())
    );
    assert_eq!(manifest["fixture"]["schema_id"], schema.schema_id);
    assert_eq!(
        manifest["fixture"]["global_logical_sha256"],
        hex_bytes(&decoded.footer.global_logical_sha256)
    );
    assert_eq!(
        manifest["fixture"]["plan_sha256"],
        hex_bytes(&decoded.footer.plan_sha256)
    );
    assert_eq!(
        manifest["fixture"]["header_sha256"],
        hex_bytes(&decoded.footer.header_sha256)
    );
    assert_eq!(
        manifest["fixture"]["body_sha256"],
        hex_bytes(&decoded.footer.body_sha256)
    );
    assert_eq!(manifest["fixture"]["record_count"], 512);
    assert_eq!(manifest["fixture"]["chunk_count"], 2);
    assert_eq!(
        manifest["fixture"]["header_bytes"],
        decoded.summary.header_bytes
    );
    assert_eq!(
        manifest["fixture"]["body_bytes"],
        decoded.summary.body_bytes
    );
    assert_eq!(
        manifest["fixture"]["footer_bytes"],
        decoded.summary.footer_bytes
    );
    let manifest_chunks = manifest["fixture"]["chunks"].as_array().unwrap();
    assert_eq!(manifest_chunks.len(), decoded.footer.chunks.len());
    for (manifest_chunk, chunk) in manifest_chunks.iter().zip(&decoded.footer.chunks) {
        assert_eq!(manifest_chunk["chunk_id"], chunk.chunk_id);
        assert_eq!(manifest_chunk["first_global_row"], chunk.first_global_row);
        assert_eq!(manifest_chunk["row_count"], chunk.row_count);
        assert_eq!(
            manifest_chunk["body_relative_offset"],
            chunk.body_relative_offset
        );
        assert_eq!(manifest_chunk["stored_len"], chunk.stored_len);
        assert_eq!(
            manifest_chunk["stored_sha256"],
            hex_bytes(&chunk.stored_sha256)
        );
        assert_eq!(
            manifest_chunk["logical_sha256"],
            hex_bytes(&chunk.chunk_logical_sha256)
        );
    }
    assert_eq!(
        without_schema_ids(decoded.batches),
        without_schema_ids(fixture_batches(schema.schema_id))
    );
}

#[test]
fn planned_v5_fixture_reader_contract_is_frozen_without_planner_cost_claims() {
    let schema_json = fs::read(fixture_path(SCHEMA_FILE)).unwrap();
    let schema = aura_codec::parse_schema_json(std::str::from_utf8(&schema_json).unwrap()).unwrap();
    let artifact = fs::read(fixture_path(FIXTURE_FILE)).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture_path(MANIFEST_FILE)).unwrap()).unwrap();
    validate_fixture(&schema, &artifact, &manifest);
    assert!(manifest["compatibility_promise"]
        .as_str()
        .unwrap()
        .contains("planner costs"));
    assert!(manifest["route_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .any(|route| route["registry"] == 1));
    assert!(manifest["route_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .any(|route| route["registry"] == 2));
    assert!(manifest["route_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .any(|route| route["registry"] == 3));
    assert!(manifest["route_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .any(|route| route["registry"] == 4));
}

#[test]
#[ignore = "explicit maintenance-only fixture generation"]
fn generate_planned_v5_compatibility_fixture() {
    assert_eq!(
        std::env::var("AURA_PLANNED_V5_FIXTURE_GENERATE")
            .ok()
            .as_deref(),
        Some("1"),
        "set AURA_PLANNED_V5_FIXTURE_GENERATE=1 to regenerate the planned-v5 fixture"
    );
    let schema = fixture_schema();
    let batches = fixture_batches(schema.schema_id);
    let artifact = compile_v3_planned_flat(&schema, &batches, V3FlatLimits::HARD).unwrap();
    let schema_json = schema.to_canonical_json().unwrap().into_bytes();
    let manifest = fixture_manifest(&schema, &artifact, &schema_json);
    validate_fixture(&schema, &artifact.bytes, &manifest);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(SCHEMA_FILE), schema_json).unwrap();
    fs::write(directory.join(FIXTURE_FILE), artifact.bytes).unwrap();
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    fs::write(directory.join(MANIFEST_FILE), manifest_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in [SCHEMA_FILE, FIXTURE_FILE, MANIFEST_FILE] {
            fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
}
